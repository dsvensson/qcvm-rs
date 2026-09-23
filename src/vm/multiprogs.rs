// SPDX-License-Identifier: MIT OR Apache-2.0

//! Several progs in one VM (FTE's multiprogs, as used by `addprogs` and CSQC add-ons).
//!
//! Each progs keeps its own globals block (with its own PARM and RETURN slots) in region S; its
//! string table is appended there too, so string offsets become addresses once relocated.
//! Function values carry the progs number in their top byte. Entity fields are unified by name:
//! a later progs' fields map onto the existing layout, new ones take words from the space each
//! entity reserves, so entity data never moves. Globals flagged as shared (and the configured
//! system globals) are copied between progs whenever execution switches from one to another.

use std::collections::HashMap;
use std::sync::Arc;

use crate::bytes::usize_from;
use crate::error::{ErrorKind, Resource, VmError};
use crate::host::Host;
use crate::progs::{FunctionKind, Program, Type};
use crate::value::{Arg, FuncRef, PrNum};
use crate::vm::Vm;
use crate::vm::core::{Core, FieldTable, OFS_PARM0, OFS_RETURN, ProgsState, StateHandles};

/// Words from PARM0 through the end of PARM7.
const PARM_WORDS: usize = 24;

/// Bytes a progs' globals block is followed by: FTE's 3-word zero tail plus 2 guard words, so
/// vector reads at the last legal operand stay inside the block.
const GLOBALS_TAIL_BYTES: usize = 20;

/// A global kept in sync between progs: its absolute byte offset and size in each progs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SharedGlobal {
    pub(crate) offset: u32,
    pub(crate) words: u32,
}

/// The globals kept in sync between progs, as slots shared by every progs.
#[derive(Clone, Debug, Default)]
pub(crate) struct SharedTable {
    names: Vec<Box<[u8]>>,
    by_name: HashMap<Box<[u8]>, usize>,
}

impl SharedTable {
    fn slot(&mut self, name: &[u8]) -> usize {
        if let Some(&slot) = self.by_name.get(name) {
            return slot;
        }
        let slot = self.names.len();
        self.names.push(name.into());
        self.by_name.insert(name.into(), slot);
        slot
    }
}

/// The handles the in-VM animation opcodes use, for a progs whose globals start at `gbase`.
pub(crate) fn state_handles(program: &Program, gbase: u32, fields: &FieldTable) -> StateHandles {
    let global_at = |name: &str| {
        program.global_def(name).and_then(|d| d.offset.checked_mul(4)?.checked_add(gbase))
    };
    let field_ofs = |name: &str| fields.get(name.as_bytes()).map(|f| f.ofs);
    StateHandles {
        self_g: global_at("self"),
        time_g: global_at("time"),
        cycle_wrapped_g: global_at("cycle_wrapped"),
        frame_f: field_ofs("frame"),
        think_f: field_ofs("think"),
        nextthink_f: field_ofs("nextthink"),
        weaponframe_f: field_ofs("weaponframe"),
    }
}

/// Registers the shared globals of progs `pr` (and fills in slots other progs share with it).
pub(crate) fn register_shared(core: &mut Core, pr: usize) {
    let Some(ps) = core.progs.get(pr) else { return };
    let program = Arc::clone(&ps.program);
    let configured: Vec<Box<[u8]>> =
        core.config.shared_globals.iter().map(|n| n.as_bytes().into()).collect();
    for d in program.global_defs() {
        if d.name.is_empty() || !(d.shared || configured.iter().any(|n| **n == *d.name)) {
            continue;
        }
        core.shared.slot(d.name);
    }
    // Refresh every progs' view of every slot.
    let names = core.shared.names.clone();
    for ps in &mut core.progs {
        ps.shared = names
            .iter()
            .map(|name| {
                let d = ps.program.global_def(name)?;
                let words = d.ty.words().unwrap_or(1).max(1);
                Some(SharedGlobal {
                    offset: d.offset.checked_mul(4)?.checked_add(ps.gbase)?,
                    words,
                })
            })
            .collect();
    }
}

/// Copies PARM0–7 and the shared globals from progs `from` to progs `to` (a call switching
/// progs).
pub(crate) fn switch_in(core: &mut Core, from: u8, to: u8) {
    let (Some(a), Some(b)) = (core.progs.get(usize::from(from)), core.progs.get(usize::from(to)))
    else {
        return;
    };
    let src = usize_from(a.gbase).wrapping_add(OFS_PARM0);
    let dst = usize_from(b.gbase).wrapping_add(OFS_PARM0);
    let shared = pairs(a, b);
    core.mem.copy_g(src, dst, PARM_WORDS);
    for (s, d, n) in shared {
        core.mem.copy_g(s, d, n);
    }
}

/// Copies RETURN and the shared globals from progs `from` back to progs `to` (a return switching
/// progs).
pub(crate) fn switch_out(core: &mut Core, from: u8, to: u8) {
    let (Some(a), Some(b)) = (core.progs.get(usize::from(from)), core.progs.get(usize::from(to)))
    else {
        return;
    };
    let src = usize_from(a.gbase).wrapping_add(OFS_RETURN);
    let dst = usize_from(b.gbase).wrapping_add(OFS_RETURN);
    let shared = pairs(a, b);
    core.mem.copy_g(src, dst, 3);
    for (s, d, n) in shared {
        core.mem.copy_g(s, d, n);
    }
}

/// `(source offset, destination offset, words)` for every slot both progs have.
fn pairs(a: &ProgsState, b: &ProgsState) -> Vec<(usize, usize, usize)> {
    a.shared
        .iter()
        .zip(&b.shared)
        .filter_map(|(x, y)| {
            let (x, y) = ((*x)?, (*y)?);
            Some((usize_from(x.offset), usize_from(y.offset), usize_from(x.words.min(y.words))))
        })
        .collect()
}

/// Looks up function `name` of progs `pr` as QuakeC sees it now: a function-typed global of that
/// name supplies its current value (`None` if QuakeC cleared it), otherwise the function record
/// of that name.
pub(crate) fn find_live_function(core: &Core, pr: u8, name: &[u8]) -> Option<FuncRef> {
    let ps = core.progs.get(usize::from(pr))?;
    let program = &ps.program;
    let global = match program.global_def_raw(name) {
        Some(d) if d.ty == Type::Function => Some(d.ofs),
        // A field or variable may share the name; look for the function global itself.
        Some(_) => program
            .global_defs()
            .find(|d| d.name == name && d.ty == Type::Function)
            .map(|d| d.offset),
        None => None,
    };
    if let Some(word) = global {
        let v = core.mem.g(core.global_offset(pr, word)?);
        return (v & 0x00FF_FFFF != 0).then_some(FuncRef(v));
    }
    let index = program.functions_by_name.get(name).copied()?;
    Some(FuncRef::new(PrNum(pr), index))
}

/// Maps a later progs' field words onto the VM-wide layout, adding fields it introduces.
/// Returns the word map, or `None` if the new fields do not fit the reserved space. Capacity is
/// checked as words are added, so a huge field count in the header costs no more than the
/// reserved space before it is refused.
fn map_fields(core: &mut Core, program: &Program) -> Option<HashMap<u32, u32>> {
    let capacity_words = 1u32.checked_shl(core.mem.stride_shift)? / 4;
    // Takes `words` new field words, if they fit.
    let take = |core: &mut Core, words: u32| {
        let u = core.fields.words;
        let next = u.checked_add(words).filter(|&n| n <= capacity_words)?;
        core.fields.words = next;
        Some(u)
    };
    let mut map: HashMap<u32, u32> = HashMap::new();
    let mut defs: Vec<_> = program.field_defs().collect();
    // Bigger definitions first at equal offsets, so a vector maps before its components.
    defs.sort_by_key(|d| (d.offset, std::cmp::Reverse(d.ty.words().unwrap_or(1))));
    for d in defs {
        let words = d.ty.words().unwrap_or(1).max(1);
        let unified = match core.fields.get(d.name) {
            Some(existing) if existing.ty.words() == d.ty.words() => existing.ofs,
            _ => match map.get(&d.offset) {
                Some(&u) => {
                    core.fields.push(d.name, d.ty, u);
                    u
                }
                None => {
                    let u = take(core, words)?;
                    core.fields.push(d.name, d.ty, u);
                    u
                }
            },
        };
        for w in 0..words {
            map.entry(d.offset.checked_add(w)?).or_insert(unified.checked_add(w)?);
        }
    }
    // Words no definition covers still need a home.
    for w in 0..program.entity_fields() {
        if let std::collections::hash_map::Entry::Vacant(v) = map.entry(w) {
            v.insert(take(core, 1)?);
        }
    }
    core.mem.field_bytes = core.fields.words.checked_mul(4)?;
    Some(map)
}

/// Writes function references into the globals of bodyless (extern) functions that some loaded
/// progs defines.
pub(crate) fn link_externs(core: &mut Core) {
    let mut writes = Vec::new();
    for (pr, ps) in core.progs.iter().enumerate() {
        for name in ps.program.bodyless_functions() {
            let Some(def) = ps.program.global_def(name) else { continue };
            if def.ty != Type::Function {
                continue;
            }
            let Some(at) = def.offset.checked_mul(4).and_then(|o| o.checked_add(ps.gbase)) else {
                continue;
            };
            if core.mem.g(usize_from(at)) & 0x00FF_FFFF != 0 {
                continue;
            }
            let found =
                core.progs.iter().enumerate().filter(|(p, _)| *p != pr).find_map(|(p, o)| {
                    let index = o.program.functions_by_name.get(name).copied()?;
                    let f = o.program.function(index)?;
                    let p = u8::try_from(p).ok()?;
                    (!matches!(f.kind, FunctionKind::Null | FunctionKind::Invalid(_)))
                        .then(|| FuncRef::new(PrNum(p), index))
                });
            if let Some(f) = found {
                writes.push((at, f.0));
            }
        }
    }
    for (at, v) in writes {
        core.mem.set_g(usize_from(at), v);
    }
}

impl<H: Host> Vm<H> {
    /// Loads another progs into this VM (FTE's `addprogs`) and returns its number.
    ///
    /// Its functions become callable (function values carry the progs number), its entity
    /// fields are unified by name with the fields already known, its extern (bodyless) functions
    /// are linked, and its `init(prevprogs)` function, if any, is called.
    ///
    /// # Errors
    /// [`ErrorKind::OutOfMemory`] if the progs limit, the address space reserved for progs, or
    /// the per-entity field reserve is exhausted; any error raised by `init`.
    pub fn add_progs(&mut self, host: &mut H, program: Arc<Program>) -> Result<PrNum, VmError> {
        let oom = |r| VmError::from(ErrorKind::OutOfMemory(r));
        let pr = self.core.progs.len();
        if pr >= usize_from(self.core.config.limits.progs).min(256) {
            return Err(oom(Resource::Progs));
        }
        let prnum = u8::try_from(pr).map_err(|_| oom(Resource::Progs))?;

        // Lay out strings and globals at the end of region S.
        let sbase = self
            .core
            .mem
            .s
            .len()
            .checked_add(15)
            .map(|v| v & !15)
            .ok_or(oom(Resource::ProgsArea))?;
        let gbase = sbase
            .checked_add(program.strings.len())
            .and_then(|v| v.checked_add(4))
            .map(|v| v & !3)
            .ok_or(oom(Resource::ProgsArea))?;
        let end = usize_from(program.num_globals())
            .checked_mul(4)
            .and_then(|b| b.checked_add(gbase))
            .and_then(|v| v.checked_add(GLOBALS_TAIL_BYTES))
            .ok_or(oom(Resource::ProgsArea))?;
        if end > usize_from(self.core.mem.e_base) {
            return Err(oom(Resource::ProgsArea));
        }
        let saved_fields = (self.core.fields.clone(), self.core.mem.field_bytes);
        let Some(field_map) = map_fields(&mut self.core, &program) else {
            (self.core.fields, self.core.mem.field_bytes) = saved_fields;
            return Err(oom(Resource::Fields));
        };
        let s = &mut self.core.mem.s;
        s.try_reserve(end.saturating_sub(s.len())).map_err(|_| oom(Resource::ProgsArea))?;
        s.resize(end, 0);
        if let Some(dst) = s.get_mut(sbase..sbase.saturating_add(program.strings.len())) {
            dst.copy_from_slice(&program.strings);
        }
        let (sbase_u32, gbase_u32) = (
            u32::try_from(sbase).map_err(|_| oom(Resource::ProgsArea))?,
            u32::try_from(gbase).map_err(|_| oom(Resource::ProgsArea))?,
        );
        for (i, &w) in program.globals.iter().enumerate() {
            self.core.mem.set_g(gbase.saturating_add(i.saturating_mul(4)), w);
        }

        // Relocate typed globals.
        let tag = u32::from(prnum) << 24;
        for d in program.global_defs() {
            let at = gbase.saturating_add(usize_from(d.offset).saturating_mul(4));
            let v = self.core.mem.g(at);
            let relocated = match d.ty {
                Type::String if v != 0 && usize_from(v) < program.strings.len() => {
                    v.wrapping_add(sbase_u32)
                }
                Type::Function if v != 0 && v < program.num_functions() => v | tag,
                Type::Field => field_map.get(&v).copied().unwrap_or(v),
                _ => continue,
            };
            self.core.mem.set_g(at, relocated);
        }
        for &word in program.pointer_relocs.iter() {
            let at = gbase.saturating_add(usize_from(word).saturating_mul(4));
            let v = self.core.mem.g(at);
            self.core.mem.set_g(at, (v & 0x7FFF_FFFF).wrapping_add(gbase_u32));
        }
        if let Some(word) = program.special.thisprogs {
            let at = gbase.saturating_add(usize_from(word).saturating_mul(4));
            self.core.mem.set_gf(at, f32::from(prnum));
        }
        if let Some(word) = program.special.fasttrackarrays {
            let at = gbase.saturating_add(usize_from(word).saturating_mul(4));
            let one = match program.global_def("__ext__fasttrackarrays").map(|d| d.ty) {
                Some(Type::Float) => 1.0f32.to_bits(),
                _ => 1,
            };
            self.core.mem.set_g(at, one);
        }

        let callees = super::bind(&program, &self.builtins);
        let state = state_handles(&program, gbase_u32, &self.core.fields);
        self.core.progs.push(ProgsState {
            code: super::core::relocate(&program, gbase_u32),
            program: Arc::clone(&program),
            gbase: gbase_u32,
            callees,
            state,
            shared: Vec::new(),
        });
        register_shared(&mut self.core, pr);
        link_externs(&mut self.core);

        if let Some(index) = program.function_index("init") {
            let prev = f32::from(prnum.saturating_sub(1));
            self.call(host, FuncRef::new(PrNum(prnum), index), &[Arg::Float(prev)])?;
        }
        Ok(PrNum(prnum))
    }

    /// Number of progs loaded (1 plus those added with [`Vm::add_progs`]).
    #[must_use]
    pub fn num_progs(&self) -> usize {
        self.core.progs.len()
    }

    /// The program loaded as progs `pr`.
    #[must_use]
    pub fn progs(&self, pr: PrNum) -> Option<&Arc<Program>> {
        self.core.progs.get(usize::from(pr.0)).map(|p| &p.program)
    }

    /// Looks up a function of progs `pr` by name, as QuakeC sees it now (see
    /// [`Vm::find_function`]).
    #[must_use]
    pub fn find_function_in(&self, pr: PrNum, name: impl AsRef<[u8]>) -> Option<FuncRef> {
        find_live_function(&self.core, pr.0, name.as_ref())
    }

    /// A typed handle to a global of progs `pr`.
    ///
    /// # Errors
    /// Fails if there is no such progs or global, or its type does not hold a `T`.
    pub fn global_in<T: crate::value::QcValue>(
        &self,
        pr: PrNum,
        name: impl AsRef<[u8]>,
    ) -> Result<crate::value::Global<T>, super::LookupError> {
        let ps = self.core.progs.get(usize::from(pr.0)).ok_or(super::LookupError::NotFound)?;
        let def = ps.program.global_def(name).ok_or(super::LookupError::NotFound)?;
        if !T::accepts(def.ty) {
            return Err(super::LookupError::WrongType(def.ty));
        }
        let addr = def.offset.checked_mul(4).and_then(|o| o.checked_add(ps.gbase));
        Ok(crate::value::Global::new(addr.ok_or(super::LookupError::NotFound)?))
    }

    /// A typed handle to a global of the progs running now: inside a builtin, the progs of the
    /// QuakeC code that called it (whose `self`, `v_forward`, … are the ones in use).
    pub(crate) fn current_global<T: crate::value::QcValue>(
        &self,
        name: &str,
    ) -> Result<crate::value::Global<T>, super::LookupError> {
        self.global_in(PrNum(self.core.x.prnum), name)
    }
}
