// SPDX-License-Identifier: MIT OR Apache-2.0

//! The VM's non-generic state and the call/return machinery shared by the interpreter and the
//! host API.

use std::collections::HashMap;
use std::sync::Arc;

use crate::bytes::usize_from;
use crate::error::{Backtrace, BacktraceFrame, ErrorKind, Warning, WarningKind};
use crate::progs::{FunctionKind, ParamCopy, Program, Stmt, Type};
use crate::value::FuncRef;
use crate::vm::config::VmConfig;
use crate::vm::memory::Memory;
use crate::vm::strings::Strings;

/// `func` value meaning "no QuakeC function is running" (the engine).
pub(crate) const NO_FUNCTION: u32 = u32::MAX;

/// Byte offset of the RETURN slot within a progs' globals.
pub(crate) const OFS_RETURN: usize = 4;
/// Byte offset of PARM0 within a progs' globals.
pub(crate) const OFS_PARM0: usize = 16;
/// Byte offset of PARM1 within a progs' globals.
pub(crate) const OFS_PARM1: usize = 28;

/// The comparison a `SWITCH` selected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SwitchKind {
    #[default]
    Float,
    Vector,
    String,
    Int,
}

/// Interpreter registers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Exec {
    /// Statement to execute next.
    pub(crate) pc: u32,
    /// Current function index within its progs, or [`NO_FUNCTION`].
    pub(crate) func: u32,
    pub(crate) prnum: u8,
    /// Words pushed (`PUSH`) by the current function.
    pub(crate) pushed: u32,
    /// Local-stack words in use.
    pub(crate) ls_top: u32,
    /// Absolute byte offset (in region S) of the global a `SWITCH` refers to.
    pub(crate) switch_ref: u32,
    pub(crate) switch_kind: SwitchKind,
    /// Byte address in region S of the current function's locals, and how many words they are
    /// (saved on the local stack while it runs, restored when it returns).
    pub(crate) locals_addr: u32,
    pub(crate) locals_words: u32,
}

impl Default for Exec {
    fn default() -> Self {
        Self {
            pc: 0,
            func: NO_FUNCTION,
            prnum: 0,
            pushed: 0,
            ls_top: 0,
            switch_ref: 0,
            switch_kind: SwitchKind::Float,
            locals_addr: 0,
            locals_words: 0,
        }
    }
}

/// A saved caller context.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame {
    pub(crate) resume_pc: u32,
    pub(crate) func: u32,
    pub(crate) prnum: u8,
    pub(crate) pushed: u32,
    pub(crate) switch_ref: u32,
    pub(crate) switch_kind: SwitchKind,
    /// Local-stack word where the callee's saved locals start.
    pub(crate) locals_at: u32,
    /// The caller's [`Exec::locals_addr`] and [`Exec::locals_words`].
    pub(crate) locals_addr: u32,
    pub(crate) locals_words: u32,
}

/// What entering a QuakeC function needs, with absolute addresses, so a call looks nothing up in
/// its [`Program`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct QcFunc {
    /// First statement.
    pub(crate) entry: u32,
    /// Byte address in region S of its locals (parameters first), and how many words they are.
    pub(crate) locals_addr: u32,
    pub(crate) locals_words: u32,
    /// Its parameter copies: `ProgsState::copies[copies_start..copies_end]`.
    pub(crate) copies_start: u32,
    pub(crate) copies_end: u32,
}

/// How a function slot is dispatched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Callee {
    Null,
    Qc,
    /// A bound builtin: index into the VM's builtin table.
    Builtin(u32),
    /// A builtin this VM does not provide.
    Missing,
    Invalid,
}

/// Globals and fields used by the in-VM animation opcodes (`STATE` and friends).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StateHandles {
    pub(crate) self_g: Option<u32>,
    pub(crate) time_g: Option<u32>,
    pub(crate) cycle_wrapped_g: Option<u32>,
    pub(crate) frame_f: Option<u32>,
    pub(crate) think_f: Option<u32>,
    pub(crate) nextthink_f: Option<u32>,
    pub(crate) weaponframe_f: Option<u32>,
}

/// A progs loaded into a VM.
#[derive(Clone, Debug)]
pub(crate) struct ProgsState {
    pub(crate) program: Arc<Program>,
    /// Byte address of its globals.
    pub(crate) gbase: u32,
    /// The program's statements with their global operands relocated to absolute byte addresses
    /// in region S (so the interpreter does not add the globals base to every operand).
    pub(crate) code: Arc<[Stmt]>,
    pub(crate) callees: Box<[Callee]>,
    /// Call information for each function (`None`: not a QuakeC function).
    pub(crate) funcs: Box<[Option<QcFunc>]>,
    /// The parameter copies of all functions, with absolute addresses.
    pub(crate) copies: Box<[ParamCopy]>,
    pub(crate) state: StateHandles,
    /// This progs' copy of each shared-global slot (see `multiprogs`).
    pub(crate) shared: Vec<Option<crate::vm::multiprogs::SharedGlobal>>,
}

impl ProgsState {
    /// The state of `program` loaded with its globals at `gbase`.
    pub(crate) fn new(
        program: Arc<Program>,
        gbase: u32,
        callees: Box<[Callee]>,
        state: StateHandles,
    ) -> Self {
        let mut copies = Vec::new();
        let funcs = program
            .functions
            .iter()
            .map(|f| {
                let FunctionKind::QuakeC { entry } = f.kind else { return None };
                let copies_start = u32::try_from(copies.len()).ok()?;
                copies.extend(program.copies(f).iter().map(|c| ParamCopy {
                    src: c.src.wrapping_add(gbase),
                    dst: c.dst.wrapping_add(gbase),
                }));
                Some(QcFunc {
                    entry,
                    locals_addr: gbase.wrapping_add(f.parm_start.wrapping_mul(4)),
                    locals_words: f.locals,
                    copies_start,
                    copies_end: u32::try_from(copies.len()).ok()?,
                })
            })
            .collect();
        Self {
            code: relocate(&program, gbase),
            gbase,
            callees,
            funcs,
            copies: copies.into(),
            state,
            shared: Vec::new(),
            program,
        }
    }

    pub(crate) fn num_globals(&self) -> u32 {
        self.program.num_globals()
    }
}

/// A program's statements with global operands relocated from globals-relative to absolute byte
/// addresses (the globals start at `gbase`).
pub(crate) fn relocate(program: &Program, gbase: u32) -> Arc<[Stmt]> {
    program
        .statements
        .iter()
        .map(|&st| {
            let [ka, kb, kc] = st.op.operands();
            let at = |kind: crate::opcode::Operand, v: u32| {
                if kind.is_relocated() { v.wrapping_add(gbase) } else { v }
            };
            Stmt { a: at(ka, st.a), b: at(kb, st.b), c: at(kc, st.c), ..st }
        })
        .collect()
}

/// One entity field of the VM-wide field layout.
#[derive(Clone, Debug)]
pub(crate) struct FieldEntry {
    pub(crate) name: Box<[u8]>,
    pub(crate) ty: Type,
    pub(crate) ofs: u32,
}

/// The VM-wide entity field layout: the main progs' fields, plus fields added by the host or by
/// later progs.
#[derive(Clone, Debug, Default)]
pub(crate) struct FieldTable {
    pub(crate) entries: Vec<FieldEntry>,
    pub(crate) by_name: HashMap<Box<[u8]>, usize>,
    /// Words in use per entity.
    pub(crate) words: u32,
}

impl FieldTable {
    pub(crate) fn get(&self, name: &[u8]) -> Option<&FieldEntry> {
        self.entries.get(*self.by_name.get(name)?)
    }

    pub(crate) fn push(&mut self, name: &[u8], ty: Type, ofs: u32) {
        let index = self.entries.len();
        self.entries.push(FieldEntry { name: name.into(), ty, ofs });
        if !name.is_empty() {
            self.by_name.entry(name.into()).or_insert(index);
        }
    }
}

/// A small, fast, seedable PRNG (SplitMix64).
#[derive(Clone, Debug)]
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A 15-bit value, like C's `rand() & 0x7fff`.
    pub(crate) fn rand15(&mut self) -> u32 {
        ((self.next_u64() >> 33) & 0x7FFF) as u32
    }
}

/// A field filled in on spawn: from a float global (absolute S offset) if present, else `value`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpawnFill {
    pub(crate) field: u32,
    pub(crate) global: Option<usize>,
    pub(crate) value: u32,
}

/// Everything the interpreter touches; independent of the host type.
#[derive(Clone, Debug)]
pub(crate) struct Core {
    pub(crate) mem: Memory,
    pub(crate) strings: Strings,
    pub(crate) progs: Vec<ProgsState>,
    pub(crate) fields: FieldTable,
    pub(crate) frames: Vec<Frame>,
    pub(crate) x: Exec,
    pub(crate) argc: u32,
    /// The builtin currently executing (for introspection).
    pub(crate) builtin: FuncRef,
    /// Nested host calls in progress.
    pub(crate) nesting: u32,
    pub(crate) rng: Rng,
    pub(crate) config: VmConfig,
    pub(crate) warnings: Vec<Warning>,
    pub(crate) warnings_this_call: u32,
    pub(crate) suppressed: u32,
    /// Statement tracing (`traceon`): every statement is reported to [`Host::trace`] before it
    /// runs.
    ///
    /// [`Host::trace`]: crate::Host::trace
    pub(crate) trace: bool,
    /// Tracing: the statement at `x.pc` has been reported and runs next.
    pub(crate) traced: bool,
    /// A panic unwound through QuakeC execution; only `reset` makes the VM usable again.
    pub(crate) poisoned: bool,
    /// Field words zeroed by `remove`.
    pub(crate) remove_clears: Vec<u32>,
    /// Resolved [`crate::vm::SpawnDefault`]s.
    pub(crate) spawn_defaults: Vec<SpawnFill>,
    /// State of the standard builtins (tokens, hash tables, string buffers).
    pub(crate) std: crate::stdlib::StdState,
    /// The value `abort(ret)` returns from the engine boundary it unwound to.
    pub(crate) abort_ret: Option<[u32; 3]>,
    /// Globals kept in sync between progs.
    pub(crate) shared: crate::vm::multiprogs::SharedTable,
    /// Frame depth the innermost running `execute` returns at (the nearest engine boundary).
    pub(crate) entry_depth: usize,
    /// Sleeping QuakeC threads (`sleep`, `fork`).
    pub(crate) threads: Vec<crate::vm::threads::Thread>,
}

impl Core {
    /// Applies the configured spawn defaults to a freshly zeroed entity.
    pub(crate) fn apply_spawn_defaults(&mut self, e: u32) {
        for i in 0..self.spawn_defaults.len() {
            let Some(&SpawnFill { field, global, value }) = self.spawn_defaults.get(i) else {
                break;
            };
            let v = global.map_or(value, |at| self.mem.g(at));
            if let Some(at) = self.mem.field_offset(e, field, 1) {
                self.mem.set_ent_word(at, v);
            }
        }
    }

    /// Records a warning with the current backtrace (rate limited per top-level call).
    #[cold]
    pub(crate) fn warn(&mut self, kind: WarningKind) {
        self.warnings_this_call = self.warnings_this_call.saturating_add(1);
        if self.warnings_this_call > self.config.limits.warnings_per_call {
            self.suppressed = self.suppressed.saturating_add(1);
            return;
        }
        let backtrace = self.backtrace();
        self.warnings.push(Warning { kind, backtrace });
    }

    /// The current QuakeC call stack, innermost first.
    pub(crate) fn backtrace(&self) -> Backtrace {
        let mut frames = Vec::new();
        let mut push = |prnum: u8, func: u32, stmt: u32| {
            if func == NO_FUNCTION {
                return;
            }
            let Some(ps) = self.progs.get(usize::from(prnum)) else { return };
            let info = ps.program.function(func);
            frames.push(BacktraceFrame {
                function: FuncRef::new(crate::value::PrNum(prnum), func),
                name: info.map(|i| i.name.into()).unwrap_or_default(),
                file: info.map(|i| i.file.into()).unwrap_or_default(),
                statement: stmt,
                line: ps.program.source_line(stmt),
            });
        };
        push(self.x.prnum, self.x.func, self.x.pc);
        for f in self.frames.iter().rev() {
            push(f.prnum, f.func, f.resume_pc.saturating_sub(1));
        }
        Backtrace(frames)
    }

    /// Enters QuakeC function `index` of progs `prnum`, resuming the current context at
    /// `resume_pc` when it returns.
    #[inline(never)]
    pub(crate) fn enter(&mut self, prnum: u8, index: u32, resume_pc: u32) -> Result<(), ErrorKind> {
        let depth_limit = usize_from(self.config.limits.call_depth);
        if self.frames.len() >= depth_limit {
            return Err(ErrorKind::CallDepth);
        }
        if prnum != self.x.prnum && self.progs.len() > 1 {
            crate::vm::multiprogs::switch_in(self, self.x.prnum, prnum);
        }
        let invalid =
            || ErrorKind::InvalidFunction(FuncRef::new(crate::value::PrNum(prnum), index));
        let ps = self.progs.get(usize::from(prnum)).ok_or_else(invalid)?;
        let f = ps.funcs.get(usize_from(index)).copied().flatten().ok_or_else(invalid)?;

        // Keep the caller's pushed memory, then save the callee's locals.
        let ls_top = self.x.ls_top.checked_add(self.x.pushed).ok_or(ErrorKind::LocalStack)?;
        let new_top = ls_top.checked_add(f.locals_words).ok_or(ErrorKind::LocalStack)?;
        if new_top > self.mem.ls_words {
            return Err(ErrorKind::LocalStack);
        }
        let save_at =
            usize_from(self.mem.ls_base).saturating_add(usize_from(ls_top).saturating_mul(4));
        let len = usize_from(f.locals_words).saturating_mul(4);
        if !self.mem.move_s(usize_from(f.locals_addr), save_at, len) {
            return Err(ErrorKind::LocalStack);
        }
        let copies =
            ps.copies.get(usize_from(f.copies_start)..usize_from(f.copies_end)).unwrap_or_default();
        for c in copies {
            let v = self.mem.g(usize_from(c.src));
            self.mem.set_g(usize_from(c.dst), v);
        }

        self.frames.push(Frame {
            resume_pc,
            func: self.x.func,
            prnum: self.x.prnum,
            pushed: self.x.pushed,
            switch_ref: self.x.switch_ref,
            switch_kind: self.x.switch_kind,
            locals_at: ls_top,
            locals_addr: self.x.locals_addr,
            locals_words: self.x.locals_words,
        });
        self.x = Exec {
            pc: f.entry,
            func: index,
            prnum,
            pushed: 0,
            ls_top: new_top,
            switch_ref: 0,
            switch_kind: SwitchKind::Float,
            locals_addr: f.locals_addr,
            locals_words: f.locals_words,
        };
        Ok(())
    }

    /// Returns from the current function: restores its caller's locals and context.
    #[inline(never)]
    pub(crate) fn leave(&mut self) {
        let callee_prnum = self.x.prnum;
        let top = self.x.ls_top.saturating_sub(self.x.locals_words);
        let saved_at =
            usize_from(self.mem.ls_base).saturating_add(usize_from(top).saturating_mul(4));
        let len = usize_from(self.x.locals_words).saturating_mul(4);
        self.mem.move_s(saved_at, usize_from(self.x.locals_addr), len);
        self.x.ls_top = top;
        if let Some(frame) = self.frames.pop() {
            self.x.ls_top = self.x.ls_top.saturating_sub(frame.pushed);
            self.x.pc = frame.resume_pc;
            self.x.func = frame.func;
            self.x.prnum = frame.prnum;
            self.x.pushed = frame.pushed;
            self.x.locals_addr = frame.locals_addr;
            self.x.locals_words = frame.locals_words;
            if self.config.compat.switch_reset_on_call {
                self.x.switch_ref = 0;
                self.x.switch_kind = SwitchKind::Float;
            } else {
                self.x.switch_ref = frame.switch_ref;
                self.x.switch_kind = frame.switch_kind;
            }
            if frame.prnum != callee_prnum {
                crate::vm::multiprogs::switch_out(self, callee_prnum, frame.prnum);
            }
        }
    }

    /// Pops frames down to `depth`, restoring locals, without touching the return slot.
    pub(crate) fn unwind(&mut self, depth: usize) {
        while self.frames.len() > depth {
            self.leave();
        }
    }

    /// Absolute byte offset of a progs' global word.
    pub(crate) fn global_offset(&self, prnum: u8, word: u32) -> Option<usize> {
        let ps = self.progs.get(usize::from(prnum))?;
        usize_from(ps.gbase).checked_add(usize_from(word).checked_mul(4)?)
    }

    /// Byte offset of the current progs' globals.
    pub(crate) fn gbase(&self) -> usize {
        self.progs.get(usize::from(self.x.prnum)).map_or(0, |p| usize_from(p.gbase))
    }
}
