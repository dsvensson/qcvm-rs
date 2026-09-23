// SPDX-License-Identifier: MIT OR Apache-2.0

//! The interpreter loop.
//!
//! It is independent of the host type: builtin calls and the host-overridable animation opcodes
//! leave the loop with an [`Exit`], the generic outer loop handles them and re-enters. All state
//! that has to survive such an exit lives in [`Core`], so nested execution (builtins calling back
//! into QuakeC) needs no special support here.
//!
//! Semantics follow `docs/spec/vm.md`; deliberate deviations are listed in
//! `docs/spec/deviations.md`.

use std::sync::Arc;

use crate::bytes::usize_from;
use crate::error::{ErrorKind, WarningKind};
use crate::opcode::Op;
use crate::value::{EntRef, FuncRef, PrNum};
use crate::vm::core::{Callee, Core, OFS_PARM0, OFS_PARM1, OFS_RETURN, SwitchKind};
use crate::vm::memory::WriteError;
use crate::vm::num::{d2i64, d2u64, f2i, f2u, fbool, float_true, ibool, join64, split64};
use crate::vm::strings::{INDEX_MASK, StrKind, TAG_MASK, TEMP_TAG, classify, until_nul};

/// An animation opcode for the host (or the VM's default implementation) to perform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StateOp {
    /// `STATE`: `self.frame = frame; self.think = think; self.nextthink = time + step`.
    State {
        /// New frame.
        frame: f32,
        /// New think function.
        think: FuncRef,
    },
    /// Hexen 2 `CSTATE`: cycle `self.frame` between `first` and `last`.
    CState {
        /// First frame of the cycle.
        first: f32,
        /// Last frame of the cycle.
        last: f32,
        /// The function running the cycle (becomes `self.think`).
        func: FuncRef,
    },
    /// Hexen 2 `CWSTATE`: like [`StateOp::CState`] on `self.weaponframe`.
    CWState {
        /// First frame of the cycle.
        first: f32,
        /// Last frame of the cycle.
        last: f32,
        /// The function running the cycle (becomes `self.think`).
        func: FuncRef,
    },
    /// Hexen 2 `THINKTIME`: `ent.nextthink = time + delay`.
    ThinkTime {
        /// The entity.
        ent: EntRef,
        /// Delay in seconds.
        delay: f32,
    },
}

/// Why the interpreter loop stopped.
#[derive(Clone, Debug)]
pub(crate) enum Exit {
    /// The function entered at the exit depth returned.
    Returned,
    /// A builtin must be called; execution resumes after the call statement.
    Builtin { slot: u32, func: FuncRef },
    /// An animation opcode; execution resumes after it.
    StateOp(StateOp),
    /// A fatal error.
    Fault(ErrorKind),
}

/// Runs until the frame stack returns to `exit_depth`, a builtin must be called, or a fault.
pub(crate) fn run(core: &mut Core, exit_depth: usize, budget: &mut u32) -> Exit {
    // The current progs' relocated statements, cloned once and refreshed only when a call or
    // return switches progs (not on every call, which would cost two atomic operations each).
    let mut cached_pr = u8::MAX;
    let mut cached: Arc<[crate::progs::Stmt]> = match core.progs.first() {
        Some(p) => Arc::clone(&p.code),
        None => return Exit::Fault(ErrorKind::InvalidFunction(FuncRef::NULL)),
    };
    'reload: loop {
        let prnum = core.x.prnum;
        let Some(ps) = core.progs.get(usize::from(prnum)) else {
            return Exit::Fault(ErrorKind::InvalidFunction(FuncRef::new(
                PrNum(prnum),
                core.x.func,
            )));
        };
        if cached_pr != prnum {
            cached = Arc::clone(&ps.code);
            cached_pr = prnum;
        }
        let stmts: &[crate::progs::Stmt] = &cached;
        let gb = usize_from(ps.gbase);
        let gbase_u32 = ps.gbase;
        let ng = ps.num_globals();
        let mut pc = core.x.pc;

        macro_rules! tick {
            () => {{
                *budget = budget.saturating_sub(1);
                if *budget == 0 {
                    core.x.pc = pc;
                    return Exit::Fault(ErrorKind::Runaway);
                }
            }};
        }
        macro_rules! fault {
            ($kind:expr) => {{
                core.x.pc = pc;
                return Exit::Fault($kind);
            }};
        }
        macro_rules! jump {
            ($target:expr) => {{
                tick!();
                pc = $target;
                continue;
            }};
        }

        loop {
            let Some(&st) = stmts.get(usize_from(pc)) else {
                fault!(ErrorKind::JumpOutOfRange);
            };
            let (oa, ob, oc) = (usize_from(st.a), usize_from(st.b), usize_from(st.c));
            let s = core.mem.s.as_mut_slice();

            macro_rules! f3 {
                ($op:tt) => {{
                    let v = gf(s, oa) $op gf(s, ob);
                    setf(s, oc, v);
                }};
            }
            macro_rules! v3 {
                ($op:tt) => {{
                    for k in [0usize, 4, 8] {
                        let v = gf(s, oa.wrapping_add(k)) $op gf(s, ob.wrapping_add(k));
                        setf(s, oc.wrapping_add(k), v);
                    }
                }};
            }
            macro_rules! cmp_f {
                ($op:tt) => {{
                    let v = fbool(gf(s, oa) $op gf(s, ob));
                    set(s, oc, v);
                }};
            }
            macro_rules! cmp_i {
                ($op:tt) => {{
                    let v = ibool((g(s, oa).cast_signed()) $op (g(s, ob).cast_signed()));
                    set(s, oc, v);
                }};
            }
            macro_rules! cmp_if {
                ($op:tt) => {{
                    let v = ibool((g(s, oa).cast_signed() as f32) $op gf(s, ob));
                    set(s, oc, v);
                }};
            }
            macro_rules! cmp_fi {
                ($op:tt) => {{
                    let v = ibool(gf(s, oa) $op (g(s, ob).cast_signed() as f32));
                    set(s, oc, v);
                }};
            }
            macro_rules! i64_op {
                ($f:expr) => {{
                    let a = join64(g(s, oa), g(s, oa.wrapping_add(4))).cast_signed();
                    let b = join64(g(s, ob), g(s, ob.wrapping_add(4))).cast_signed();
                    let f: fn(i64, i64) -> i64 = $f;
                    let (lo, hi) = split64(f(a, b).cast_unsigned());
                    set(s, oc, lo);
                    set(s, oc.wrapping_add(4), hi);
                }};
            }
            macro_rules! i64_cmp {
                ($f:expr) => {{
                    let a = join64(g(s, oa), g(s, oa.wrapping_add(4)));
                    let b = join64(g(s, ob), g(s, ob.wrapping_add(4)));
                    let f: fn(u64, u64) -> bool = $f;
                    set(s, oc, ibool(f(a, b)));
                }};
            }
            macro_rules! d_op {
                ($op:tt) => {{
                    let a = f64::from_bits(join64(g(s, oa), g(s, oa.wrapping_add(4))));
                    let b = f64::from_bits(join64(g(s, ob), g(s, ob.wrapping_add(4))));
                    let (lo, hi) = split64((a $op b).to_bits());
                    set(s, oc, lo);
                    set(s, oc.wrapping_add(4), hi);
                }};
            }
            macro_rules! d_cmp {
                ($op:tt) => {{
                    let a = f64::from_bits(join64(g(s, oa), g(s, oa.wrapping_add(4))));
                    let b = f64::from_bits(join64(g(s, ob), g(s, ob.wrapping_add(4))));
                    set(s, oc, ibool(a $op b));
                }};
            }
            macro_rules! set64 {
                ($off:expr, $v:expr) => {{
                    let (lo, hi) = split64($v);
                    set(s, $off, lo);
                    set(s, $off.wrapping_add(4), hi);
                }};
            }
            macro_rules! get64 {
                ($off:expr) => {
                    join64(g(s, $off), g(s, $off.wrapping_add(4)))
                };
            }

            match st.op {
                // ---- v6 arithmetic -------------------------------------------------------
                Op::MulF => f3!(*),
                Op::DivF => f3!(/),
                Op::AddF => f3!(+),
                Op::SubF => f3!(-),
                Op::MulV => {
                    let a = gv(s, oa);
                    let b = gv(s, ob);
                    setf(s, oc, a[0] * b[0] + a[1] * b[1] + a[2] * b[2]);
                }
                Op::MulFV => {
                    let f = gf(s, oa);
                    for k in [0usize, 4, 8] {
                        let v = f * gf(s, ob.wrapping_add(k));
                        setf(s, oc.wrapping_add(k), v);
                    }
                }
                Op::MulVF => {
                    let f = gf(s, ob);
                    for k in [0usize, 4, 8] {
                        let v = gf(s, oa.wrapping_add(k)) * f;
                        setf(s, oc.wrapping_add(k), v);
                    }
                }
                Op::DivVF => {
                    let f = gf(s, ob);
                    for k in [0usize, 4, 8] {
                        let v = gf(s, oa.wrapping_add(k)) / f;
                        setf(s, oc.wrapping_add(k), v);
                    }
                }
                Op::AddV => v3!(+),
                Op::SubV => v3!(-),

                // ---- v6 comparisons ------------------------------------------------------
                Op::EqF => cmp_f!(==),
                Op::NeF => cmp_f!(!=),
                Op::LeF => cmp_f!(<=),
                Op::GeF => cmp_f!(>=),
                Op::LtF => cmp_f!(<),
                Op::GtF => cmp_f!(>),
                Op::EqV => {
                    let (a, b) = (gv(s, oa), gv(s, ob));
                    set(s, oc, fbool(a[0] == b[0] && a[1] == b[1] && a[2] == b[2]));
                }
                Op::NeV => {
                    let (a, b) = (gv(s, oa), gv(s, ob));
                    set(s, oc, fbool(a[0] != b[0] || a[1] != b[1] || a[2] != b[2]));
                }
                Op::EqE | Op::EqFnc => {
                    let v = fbool(g(s, oa) == g(s, ob));
                    set(s, oc, v);
                }
                Op::NeE | Op::NeFnc => {
                    let v = fbool(g(s, oa) != g(s, ob));
                    set(s, oc, v);
                }
                Op::EqS | Op::NeS => {
                    let (a, b) = (g(s, oa), g(s, ob));
                    core.x.pc = pc;
                    let v = string_compare(core, st.op, a, b);
                    core.mem.set_g(oc, v);
                }

                // ---- logic ---------------------------------------------------------------
                Op::NotF => {
                    let v = fbool(!float_true(g(s, oa)));
                    set(s, oc, v);
                }
                Op::NotV => {
                    let a = gv(s, oa);
                    set(s, oc, fbool(a[0] == 0.0 && a[1] == 0.0 && a[2] == 0.0));
                }
                Op::NotS => {
                    let r = g(s, oa);
                    core.x.pc = pc;
                    let empty = r == 0 || str_or_warn(core, r).is_empty();
                    core.mem.set_g(oc, fbool(empty));
                }
                Op::NotEnt => {
                    let v = fbool(g(s, oa) == 0);
                    set(s, oc, v);
                }
                Op::NotFnc => {
                    let v = fbool(g(s, oa) & 0x00FF_FFFF == 0);
                    set(s, oc, v);
                }
                Op::NotI => {
                    let v = ibool(g(s, oa) == 0);
                    set(s, oc, v);
                }
                Op::AndF => {
                    let v = fbool(float_true(g(s, oa)) && float_true(g(s, ob)));
                    set(s, oc, v);
                }
                Op::OrF => {
                    let v = fbool(float_true(g(s, oa)) || float_true(g(s, ob)));
                    set(s, oc, v);
                }
                Op::BitAndF => {
                    let v = (f2i(gf(s, oa)) & f2i(gf(s, ob))) as f32;
                    setf(s, oc, v);
                }
                Op::BitOrF => {
                    let v = (f2i(gf(s, oa)) | f2i(gf(s, ob))) as f32;
                    setf(s, oc, v);
                }

                // ---- branches ------------------------------------------------------------
                Op::IfI => {
                    if g(s, oa) != 0 {
                        jump!(st.b);
                    }
                    tick!();
                }
                Op::IfNotI => {
                    if g(s, oa) == 0 {
                        jump!(st.b);
                    }
                    tick!();
                }
                Op::IfF => {
                    if float_true(g(s, oa)) {
                        jump!(st.b);
                    }
                    tick!();
                }
                Op::IfNotF => {
                    if !float_true(g(s, oa)) {
                        jump!(st.b);
                    }
                    tick!();
                }
                Op::IfS => {
                    if g(s, oa) != 0 {
                        jump!(st.b);
                    }
                    tick!();
                }
                Op::IfNotS => {
                    if g(s, oa) == 0 {
                        jump!(st.b);
                    }
                    tick!();
                }
                Op::Goto => jump!(st.a),

                // ---- entity fields -------------------------------------------------------
                Op::LoadF
                | Op::LoadS
                | Op::LoadEnt
                | Op::LoadFld
                | Op::LoadFnc
                | Op::LoadI
                | Op::LoadP => {
                    let (e, f) = (g(s, oa), g(s, ob));
                    let v = match core.mem.field_offset(e, f, 1) {
                        Some(o) => core.mem.ent_word(o),
                        None => {
                            core.x.pc = pc;
                            bad_field_access(core, e, f);
                            0
                        }
                    };
                    core.mem.set_g(oc, v);
                }
                Op::LoadV => {
                    let (e, f) = (g(s, oa), g(s, ob));
                    if let Some(o) = core.mem.field_offset(e, f, 3) {
                        let v = [0usize, 4, 8].map(|k| core.mem.ent_word(o.wrapping_add(k)));
                        for (k, w) in [0usize, 4, 8].into_iter().zip(v) {
                            core.mem.set_g(oc.wrapping_add(k), w);
                        }
                    } else {
                        let entity_ok = e < core.mem.num_edicts();
                        core.x.pc = pc;
                        bad_field_access(core, e, f);
                        // FTE: an invalid entity zeroes the whole vector, an invalid field only
                        // the first word.
                        let words: usize = if entity_ok { 1 } else { 3 };
                        for k in 0..words {
                            core.mem.set_g(oc.wrapping_add(k.wrapping_mul(4)), 0);
                        }
                    }
                }
                Op::LoadI64 => {
                    let (e, f) = (g(s, oa), g(s, ob));
                    if let Some(o) = core.mem.field_offset(e, f, 2) {
                        let (lo, hi) = (core.mem.ent_word(o), core.mem.ent_word(o.wrapping_add(4)));
                        core.mem.set_g(oc, lo);
                        core.mem.set_g(oc.wrapping_add(4), hi);
                    } else {
                        let entity_ok = e < core.mem.num_edicts();
                        let words: usize = if entity_ok {
                            1
                        } else if core.config.compat.load_i64_zero3 {
                            3
                        } else {
                            2
                        };
                        core.x.pc = pc;
                        bad_field_access(core, e, f);
                        for k in 0..words {
                            core.mem.set_g(oc.wrapping_add(k.wrapping_mul(4)), 0);
                        }
                    }
                }
                Op::Address => {
                    let (e, f) = (g(s, oa), g(s, ob));
                    if e >= core.mem.num_edicts() {
                        core.x.pc = pc;
                        core.warn(WarningKind::BadEntity(e));
                    } else if core.mem.protected(e) {
                        core.mem.set_g(oc, u32::MAX);
                        core.x.pc = pc;
                        core.warn(WarningKind::ReadOnlyEntity(e));
                    } else {
                        let p = core.mem.field_address(e, f);
                        core.mem.set_g(oc, p);
                    }
                }
                Op::StoreFieldF | Op::StoreFieldS | Op::StoreFieldI => {
                    core.x.pc = pc;
                    store_field(core, oa, ob, oc, 1);
                }
                Op::StoreFieldV => {
                    core.x.pc = pc;
                    store_field(core, oa, ob, oc, 3);
                }
                Op::StoreFieldI64 => {
                    core.x.pc = pc;
                    store_field(core, oa, ob, oc, 2);
                }

                // ---- global stores -------------------------------------------------------
                Op::StoreF
                | Op::StoreS
                | Op::StoreEnt
                | Op::StoreFld
                | Op::StoreFnc
                | Op::StoreI
                | Op::StoreP => {
                    let v = g(s, oa);
                    set(s, ob, v);
                }
                Op::StoreV => copy(s, oa, ob, 3),
                Op::StoreI64 => copy(s, oa, ob, 2),
                Op::StoreIF => {
                    let v = g(s, oa).cast_signed() as f32;
                    setf(s, ob, v);
                }
                Op::StoreFI => {
                    let v = f2i(gf(s, oa)).cast_unsigned();
                    set(s, ob, v);
                }

                // ---- stores through pointers ---------------------------------------------
                Op::StorePF
                | Op::StorePS
                | Op::StorePEnt
                | Op::StorePFld
                | Op::StorePFnc
                | Op::StorePI => {
                    let (v, base, idx) = (g(s, oa), g(s, ob), g(s, oc));
                    core.x.pc = pc;
                    if let Err(k) = ptr_write(core, base, idx.wrapping_mul(4), &v.to_le_bytes()) {
                        fault!(k);
                    }
                }
                Op::StorePV => {
                    let (base, idx) = (g(s, ob), g(s, oc));
                    let mut bytes = [0u8; 12];
                    for (k, chunk) in bytes.chunks_exact_mut(4).enumerate() {
                        chunk.copy_from_slice(
                            &g(s, oa.wrapping_add(k.wrapping_mul(4))).to_le_bytes(),
                        );
                    }
                    core.x.pc = pc;
                    if let Err(k) = ptr_write(core, base, idx.wrapping_mul(4), &bytes) {
                        fault!(k);
                    }
                }
                Op::StorePI64 => {
                    let (base, idx) = (g(s, ob), g(s, oc));
                    let v = get64!(oa);
                    core.x.pc = pc;
                    if let Err(k) = ptr_write(core, base, idx.wrapping_mul(4), &v.to_le_bytes()) {
                        fault!(k);
                    }
                }
                Op::StorePIF | Op::StorePFI => {
                    let a = g(s, oa);
                    let v = if st.op == Op::StorePIF {
                        (a.cast_signed() as f32).to_bits()
                    } else {
                        f2i(f32::from_bits(a)).cast_unsigned()
                    };
                    let (base, idx) = (g(s, ob), g(s, oc));
                    core.x.pc = pc;
                    if let Err(k) = ptr_write(core, base, idx.wrapping_mul(4), &v.to_le_bytes()) {
                        fault!(k);
                    }
                }
                Op::StorePC => {
                    let v = f2i(gf(s, oa)) as u8;
                    let (base, idx) = (g(s, ob), g(s, oc));
                    core.x.pc = pc;
                    if let Err(k) = ptr_write(core, base, idx, &[v]) {
                        fault!(k);
                    }
                }
                Op::StorePI8 => {
                    let v = g(s, oa) as u8;
                    let (base, idx) = (g(s, ob), g(s, oc));
                    core.x.pc = pc;
                    if let Err(k) = ptr_write(core, base, idx, &[v]) {
                        fault!(k);
                    }
                }
                Op::StorePI16 => {
                    let v = g(s, oa) as u16;
                    let (base, idx) = (g(s, ob), g(s, oc));
                    core.x.pc = pc;
                    if let Err(k) = ptr_write(core, base, idx.wrapping_mul(2), &v.to_le_bytes()) {
                        fault!(k);
                    }
                }

                // ---- loads through pointers ----------------------------------------------
                Op::LoadPF
                | Op::LoadPS
                | Op::LoadPEnt
                | Op::LoadPFld
                | Op::LoadPFnc
                | Op::LoadPI => {
                    let (base, idx) = (g(s, oa), g(s, ob));
                    core.x.pc = pc;
                    match ptr_read::<4>(core, base, idx.wrapping_mul(4)) {
                        Ok(b) => core.mem.set_g(oc, u32::from_le_bytes(b)),
                        Err(k) => fault!(k),
                    }
                }
                Op::LoadPV => {
                    let (base, idx) = (g(s, oa), g(s, ob));
                    core.x.pc = pc;
                    match ptr_read::<12>(core, base, idx.wrapping_mul(4)) {
                        Ok(b) => {
                            for (k, w) in b.chunks_exact(4).enumerate() {
                                let v = crate::bytes::u32_at(w, 0).unwrap_or(0);
                                core.mem.set_g(oc.wrapping_add(k.wrapping_mul(4)), v);
                            }
                        }
                        Err(k) => fault!(k),
                    }
                }
                Op::LoadPI64 => {
                    let (base, idx) = (g(s, oa), g(s, ob));
                    core.x.pc = pc;
                    match ptr_read::<8>(core, base, idx.wrapping_mul(4)) {
                        Ok(b) => {
                            let (lo, hi) = split64(u64::from_le_bytes(b));
                            core.mem.set_g(oc, lo);
                            core.mem.set_g(oc.wrapping_add(4), hi);
                        }
                        Err(k) => fault!(k),
                    }
                }
                Op::LoadPC => {
                    let base = g(s, oa);
                    let idx = f2i(gf(s, ob)).cast_unsigned();
                    core.x.pc = pc;
                    match ptr_read::<1>(core, base, idx) {
                        Ok([b]) => core.mem.set_gf(oc, f32::from(b)),
                        Err(k) => fault!(k),
                    }
                }
                Op::LoadPU8 | Op::LoadPI8 => {
                    let (base, idx) = (g(s, oa), g(s, ob));
                    core.x.pc = pc;
                    match ptr_read::<1>(core, base, idx) {
                        Ok([b]) => {
                            let v = if st.op == Op::LoadPI8 {
                                i32::from(b.cast_signed()).cast_unsigned()
                            } else {
                                u32::from(b)
                            };
                            core.mem.set_g(oc, v);
                        }
                        Err(k) => fault!(k),
                    }
                }
                Op::LoadPU16 | Op::LoadPI16 => {
                    let (base, idx) = (g(s, oa), g(s, ob));
                    core.x.pc = pc;
                    match ptr_read::<2>(core, base, idx.wrapping_mul(2)) {
                        Ok(b) => {
                            let h = u16::from_le_bytes(b);
                            let v = if st.op == Op::LoadPI16 {
                                i32::from(h.cast_signed()).cast_unsigned()
                            } else {
                                u32::from(h)
                            };
                            core.mem.set_g(oc, v);
                        }
                        Err(k) => fault!(k),
                    }
                }
                Op::LoadPItoF | Op::LoadPFtoI => {
                    // FTE: operand B is ignored and there is no fallback.
                    let p = g(s, oa);
                    match core.mem.read_u32(p) {
                        Some(w) => {
                            let v = if st.op == Op::LoadPItoF {
                                (w.cast_signed() as f32).to_bits()
                            } else {
                                f2i(f32::from_bits(w)).cast_unsigned()
                            };
                            core.mem.set_g(oc, v);
                        }
                        None => fault!(ErrorKind::BadPointerRead(p)),
                    }
                }

                // ---- Hexen 2 compound stores ---------------------------------------------
                Op::MulStoreF => {
                    let v = gf(s, ob) * gf(s, oa);
                    setf(s, ob, v);
                }
                Op::DivStoreF => {
                    let v = gf(s, ob) / gf(s, oa);
                    setf(s, ob, v);
                }
                Op::AddStoreF => {
                    let v = gf(s, ob) + gf(s, oa);
                    setf(s, ob, v);
                }
                Op::SubStoreF => {
                    let v = gf(s, ob) - gf(s, oa);
                    setf(s, ob, v);
                }
                Op::MulStoreVF => {
                    let f = gf(s, oa);
                    for k in [0usize, 4, 8] {
                        let v = gf(s, ob.wrapping_add(k)) * f;
                        setf(s, ob.wrapping_add(k), v);
                    }
                }
                Op::AddStoreV | Op::SubStoreV => {
                    for k in [0usize, 4, 8] {
                        let (b, a) = (gf(s, ob.wrapping_add(k)), gf(s, oa.wrapping_add(k)));
                        let v = if st.op == Op::AddStoreV { b + a } else { b - a };
                        setf(s, ob.wrapping_add(k), v);
                    }
                }
                Op::BitSetStoreF => {
                    let v = (f2i(gf(s, ob)) | f2i(gf(s, oa))) as f32;
                    setf(s, ob, v);
                }
                Op::BitClrStoreF => {
                    let v = (f2i(gf(s, ob)) & !f2i(gf(s, oa))) as f32;
                    setf(s, ob, v);
                }
                Op::MulStorePF
                | Op::DivStorePF
                | Op::AddStorePF
                | Op::SubStorePF
                | Op::MulStorePVF
                | Op::AddStorePV
                | Op::SubStorePV
                | Op::BitSetStorePF
                | Op::BitClrStorePF => {
                    core.x.pc = pc;
                    if let Err(k) = compound_pointer_store(core, st.op, oa, ob, oc) {
                        fault!(k);
                    }
                }

                // ---- Hexen 2 global arrays -----------------------------------------------
                Op::FetchGblF | Op::FetchGblS | Op::FetchGblE | Op::FetchGblFnc | Op::FetchGblV => {
                    let base = st.a;
                    let i = f2i(gf(s, ob));
                    let Some(prefix_word) = base.checked_sub(1) else {
                        fault!(ErrorKind::ArrayIndex(i64::from(i)));
                    };
                    let prefix = g(s, gb.wrapping_add(usize_from(prefix_word).wrapping_mul(4)));
                    if i.cast_unsigned() > prefix {
                        fault!(ErrorKind::ArrayIndex(i64::from(i)));
                    }
                    let stride: u32 = if st.op == Op::FetchGblV { 3 } else { 1 };
                    let word = base.wrapping_add(i.cast_unsigned().wrapping_mul(stride));
                    let src = gb.wrapping_add(usize_from(word).wrapping_mul(4));
                    copy(s, src, oc, usize_from(stride));
                }

                // ---- animation -----------------------------------------------------------
                Op::State => {
                    let op = StateOp::State { frame: gf(s, oa), think: FuncRef(g(s, ob)) };
                    core.x.pc = pc.wrapping_add(1);
                    return Exit::StateOp(op);
                }
                Op::CState | Op::CWState => {
                    let (first, last) = (gf(s, oa), gf(s, ob));
                    let func = FuncRef::new(PrNum(prnum), core.x.func);
                    let op = if st.op == Op::CState {
                        StateOp::CState { first, last, func }
                    } else {
                        StateOp::CWState { first, last, func }
                    };
                    core.x.pc = pc.wrapping_add(1);
                    return Exit::StateOp(op);
                }
                Op::ThinkTime => {
                    let op = StateOp::ThinkTime { ent: EntRef(g(s, oa)), delay: gf(s, ob) };
                    core.x.pc = pc.wrapping_add(1);
                    return Exit::StateOp(op);
                }

                // ---- random --------------------------------------------------------------
                Op::Rand0 | Op::Rand1 | Op::Rand2 => {
                    let r = core.rng.rand15() as f32 / 32768.0;
                    let s = core.mem.s.as_mut_slice();
                    let v = match st.op {
                        Op::Rand0 => r,
                        Op::Rand1 => r * gf(s, oa),
                        _ => {
                            let (a, b) = (gf(s, oa), gf(s, ob));
                            a + r * (b - a)
                        }
                    };
                    setf(s, oc, v);
                }
                Op::RandV0 | Op::RandV1 | Op::RandV2 => {
                    for k in [0usize, 4, 8] {
                        let r = core.rng.rand15() as f32 / 32767.0;
                        let s = core.mem.s.as_mut_slice();
                        let v = match st.op {
                            Op::RandV0 => r,
                            Op::RandV1 => r * gf(s, oa.wrapping_add(k)),
                            _ => {
                                let (a, b) = (gf(s, oa.wrapping_add(k)), gf(s, ob.wrapping_add(k)));
                                a + r * (b - a)
                            }
                        };
                        setf(s, oc.wrapping_add(k), v);
                    }
                }

                // ---- switch --------------------------------------------------------------
                Op::SwitchF
                | Op::SwitchV
                | Op::SwitchS
                | Op::SwitchE
                | Op::SwitchFnc
                | Op::SwitchI => {
                    core.x.switch_ref = u32::try_from(oa).unwrap_or(u32::MAX);
                    core.x.switch_kind = match st.op {
                        Op::SwitchF => SwitchKind::Float,
                        Op::SwitchV => SwitchKind::Vector,
                        Op::SwitchS => SwitchKind::String,
                        _ => SwitchKind::Int,
                    };
                    jump!(st.b);
                }
                Op::Case => {
                    let r = usize_from(core.x.switch_ref);
                    let hit = match core.x.switch_kind {
                        SwitchKind::Float => gf(s, r) == gf(s, oa),
                        SwitchKind::Vector => gv(s, r) == gv(s, oa),
                        SwitchKind::Int => g(s, r) == g(s, oa),
                        SwitchKind::String => {
                            let (a, b) = (g(s, r), g(s, oa));
                            core.x.pc = pc;
                            strings_equal(core, a, b)
                        }
                    };
                    if hit {
                        jump!(st.b);
                    }
                }
                Op::CaseRange => {
                    let r = usize_from(core.x.switch_ref);
                    let hit = match core.x.switch_kind {
                        SwitchKind::Float => {
                            let v = gf(s, r);
                            gf(s, oa) <= v && v <= gf(s, ob)
                        }
                        SwitchKind::Vector => {
                            let (v, lo, hi) = (gv(s, r), gv(s, oa), gv(s, ob));
                            v.iter().zip(lo).zip(hi).all(|((v, lo), hi)| lo <= *v && *v <= hi)
                        }
                        SwitchKind::Int => {
                            let v = g(s, r).cast_signed();
                            g(s, oa).cast_signed() <= v && v <= g(s, ob).cast_signed()
                        }
                        SwitchKind::String => fault!(ErrorKind::StringCaseRange),
                    };
                    if hit {
                        jump!(st.c);
                    }
                }

                // ---- calls and returns ---------------------------------------------------
                Op::Call0
                | Op::Call1
                | Op::Call2
                | Op::Call3
                | Op::Call4
                | Op::Call5
                | Op::Call6
                | Op::Call7
                | Op::Call8
                | Op::Call1H
                | Op::Call2H
                | Op::Call3H
                | Op::Call4H
                | Op::Call5H
                | Op::Call6H
                | Op::Call7H
                | Op::Call8H => {
                    let argc = st.op.call_argc().unwrap_or(0);
                    if st.op.is_hexen2_call() {
                        if argc >= 2 {
                            copy(s, oc, gb.wrapping_add(OFS_PARM1), 3);
                        }
                        copy(s, ob, gb.wrapping_add(OFS_PARM0), 3);
                    }
                    tick!();
                    let fv = g(s, oa);
                    core.argc = u32::from(argc);
                    let target = FuncRef(fv);
                    let tp = target.progs();
                    let callee = core
                        .progs
                        .get(usize::from(tp.0))
                        .map(|p| p.callees.get(usize_from(target.index())).copied());
                    match callee {
                        Some(Some(Callee::Qc)) => {
                            core.x.pc = pc;
                            if let Err(k) = core.enter(tp.0, target.index(), pc.wrapping_add(1)) {
                                fault!(k);
                            }
                            continue 'reload;
                        }
                        Some(Some(Callee::Builtin(slot))) => {
                            core.x.pc = pc.wrapping_add(1);
                            return Exit::Builtin { slot, func: target };
                        }
                        Some(Some(Callee::Null)) => fault!(ErrorKind::NullFunction),
                        Some(Some(Callee::Missing)) => {
                            core.x.pc = pc;
                            return Exit::Fault(missing_builtin(core, target));
                        }
                        Some(Some(Callee::Invalid)) | Some(None) | None => {
                            fault!(ErrorKind::InvalidFunction(target))
                        }
                    }
                }
                Op::Return | Op::Done => {
                    tick!();
                    copy(s, oa, gb.wrapping_add(OFS_RETURN), 3);
                    core.x.pc = pc;
                    core.leave();
                    if core.frames.len() <= exit_depth {
                        return Exit::Returned;
                    }
                    continue 'reload;
                }

                // ---- integers ------------------------------------------------------------
                Op::AddI => {
                    let v = g(s, oa).wrapping_add(g(s, ob));
                    set(s, oc, v);
                }
                Op::SubI => {
                    let v = g(s, oa).wrapping_sub(g(s, ob));
                    set(s, oc, v);
                }
                Op::MulI => {
                    let v = g(s, oa).wrapping_mul(g(s, ob));
                    set(s, oc, v);
                }
                Op::DivI => {
                    let (a, b) = (g(s, oa).cast_signed(), g(s, ob).cast_signed());
                    let v = match a.checked_div(b) {
                        Some(v) => v,
                        None if b == 0 => 0,
                        None => i32::MAX,
                    };
                    set(s, oc, v.cast_unsigned());
                }
                Op::BitAndI => {
                    let v = g(s, oa) & g(s, ob);
                    set(s, oc, v);
                }
                Op::BitOrI => {
                    let v = g(s, oa) | g(s, ob);
                    set(s, oc, v);
                }
                Op::BitXorI => {
                    let v = g(s, oa) ^ g(s, ob);
                    set(s, oc, v);
                }
                Op::RShiftI => {
                    let v = g(s, oa).cast_signed().wrapping_shr(g(s, ob)).cast_unsigned();
                    set(s, oc, v);
                }
                Op::LShiftI => {
                    let v = g(s, oa).wrapping_shl(g(s, ob));
                    set(s, oc, v);
                }
                Op::EqI => cmp_i!(==),
                Op::NeI => cmp_i!(!=),
                Op::LeI => cmp_i!(<=),
                Op::GeI => cmp_i!(>=),
                Op::LtI => cmp_i!(<),
                Op::GtI => cmp_i!(>),
                Op::AndI => {
                    let v = ibool(g(s, oa) != 0 && g(s, ob) != 0);
                    set(s, oc, v);
                }
                Op::OrI => {
                    let v = ibool(g(s, oa) != 0 || g(s, ob) != 0);
                    set(s, oc, v);
                }
                Op::ConvItoF => {
                    let v = g(s, oa).cast_signed() as f32;
                    setf(s, oc, v);
                }
                Op::ConvFtoI => {
                    let v = f2i(gf(s, oa)).cast_unsigned();
                    set(s, oc, v);
                }

                // ---- mixed int/float -----------------------------------------------------
                Op::AddFI => {
                    let v = gf(s, oa) + g(s, ob).cast_signed() as f32;
                    setf(s, oc, v);
                }
                Op::AddIF => {
                    let v = g(s, oa).cast_signed() as f32 + gf(s, ob);
                    setf(s, oc, v);
                }
                Op::SubFI => {
                    let v = gf(s, oa) - g(s, ob).cast_signed() as f32;
                    setf(s, oc, v);
                }
                Op::SubIF => {
                    let v = g(s, oa).cast_signed() as f32 - gf(s, ob);
                    setf(s, oc, v);
                }
                Op::MulIF => {
                    let v = g(s, oa).cast_signed() as f32 * gf(s, ob);
                    setf(s, oc, v);
                }
                Op::MulFI => {
                    let v = gf(s, oa) * g(s, ob).cast_signed() as f32;
                    setf(s, oc, v);
                }
                Op::DivIF => {
                    let v = g(s, oa).cast_signed() as f32 / gf(s, ob);
                    setf(s, oc, v);
                }
                Op::DivFI => {
                    let v = gf(s, oa) / g(s, ob).cast_signed() as f32;
                    setf(s, oc, v);
                }
                Op::MulVI => {
                    let i = g(s, ob).cast_signed() as f32;
                    for k in [0usize, 4, 8] {
                        let v = gf(s, oa.wrapping_add(k)) * i;
                        setf(s, oc.wrapping_add(k), v);
                    }
                }
                Op::MulIV => {
                    let i = g(s, oa).cast_signed() as f32;
                    for k in [0usize, 4, 8] {
                        let v = i * gf(s, ob.wrapping_add(k));
                        setf(s, oc.wrapping_add(k), v);
                    }
                }
                Op::LeIF => cmp_if!(<=),
                Op::GeIF => cmp_if!(>=),
                Op::LtIF => cmp_if!(<),
                Op::GtIF => cmp_if!(>),
                Op::EqIF => cmp_if!(==),
                Op::NeIF => cmp_if!(!=),
                Op::LeFI => cmp_fi!(<=),
                Op::GeFI => cmp_fi!(>=),
                Op::LtFI => cmp_fi!(<),
                Op::GtFI => cmp_fi!(>),
                Op::EqFI => cmp_fi!(==),
                Op::NeFI => cmp_fi!(!=),
                Op::BitAndIF => {
                    let v = g(s, oa).cast_signed() & f2i(gf(s, ob));
                    set(s, oc, v.cast_unsigned());
                }
                Op::BitOrIF => {
                    let v = g(s, oa).cast_signed() | f2i(gf(s, ob));
                    set(s, oc, v.cast_unsigned());
                }
                Op::BitAndFI => {
                    let v = f2i(gf(s, oa)) & g(s, ob).cast_signed();
                    set(s, oc, v.cast_unsigned());
                }
                Op::BitOrFI => {
                    let v = f2i(gf(s, oa)) | g(s, ob).cast_signed();
                    set(s, oc, v.cast_unsigned());
                }
                Op::AndIF => {
                    let v = ibool(g(s, oa) != 0 && gf(s, ob) != 0.0);
                    set(s, oc, v);
                }
                Op::OrIF => {
                    let v = ibool(g(s, oa) != 0 || gf(s, ob) != 0.0);
                    set(s, oc, v);
                }
                Op::AndFI => {
                    let v = ibool(gf(s, oa) != 0.0 && g(s, ob) != 0);
                    set(s, oc, v);
                }
                Op::OrFI => {
                    let v = ibool(gf(s, oa) != 0.0 || g(s, ob) != 0);
                    set(s, oc, v);
                }

                // ---- pointers, strings, indexed globals ----------------------------------
                Op::GlobalAddress => {
                    let word = st.a.wrapping_add(g(s, ob));
                    let v = gbase_u32.wrapping_add(word.wrapping_mul(4));
                    set(s, oc, v);
                }
                Op::AddPIW => {
                    let v = g(s, oa).wrapping_add(g(s, ob).wrapping_mul(4));
                    set(s, oc, v);
                }
                Op::AddSF => {
                    let v = g(s, oa).wrapping_add(f2i(gf(s, ob)).cast_unsigned());
                    set(s, oc, v);
                }
                Op::SubS => {
                    let v = g(s, oa).wrapping_sub(g(s, ob));
                    set(s, oc, v);
                }
                Op::LoadAF
                | Op::LoadAS
                | Op::LoadAEnt
                | Op::LoadAFld
                | Op::LoadAFnc
                | Op::LoadAI
                | Op::LoadAV
                | Op::LoadAI64 => {
                    let words: u32 = match st.op {
                        Op::LoadAV => 3,
                        Op::LoadAI64 => 2,
                        _ => 1,
                    };
                    let i = i64::from(st.a).wrapping_add(i64::from(g(s, ob).cast_signed()));
                    if i < 0 || i.saturating_add(i64::from(words)) > i64::from(ng) {
                        fault!(ErrorKind::ArrayIndex(i));
                    }
                    let src = gb.wrapping_add(usize::try_from(i).unwrap_or(0).wrapping_mul(4));
                    copy(s, src, oc, usize_from(words));
                }
                Op::GLoadI
                | Op::GLoadF
                | Op::GLoadFld
                | Op::GLoadEnt
                | Op::GLoadS
                | Op::GLoadFnc
                | Op::GLoadV => {
                    let words: u32 = if st.op == Op::GLoadV { 3 } else { 1 };
                    let i = g(s, oa).cast_signed();
                    if i < 0 || i64::from(i).saturating_add(i64::from(words)) > i64::from(ng) {
                        fault!(ErrorKind::ArrayIndex(i64::from(i)));
                    }
                    let src = gb.wrapping_add(usize_from(i.cast_unsigned()).wrapping_mul(4));
                    copy(s, src, oc, usize_from(words));
                }
                Op::GStorePI
                | Op::GStorePF
                | Op::GStorePEnt
                | Op::GStorePFld
                | Op::GStorePS
                | Op::GStorePFnc
                | Op::GStorePV => {
                    let words: u32 = if st.op == Op::GStorePV { 3 } else { 1 };
                    let i = g(s, ob).cast_signed();
                    if i < 0 || i64::from(i).saturating_add(i64::from(words)) > i64::from(ng) {
                        fault!(ErrorKind::ArrayIndex(i64::from(i)));
                    }
                    let dst = gb.wrapping_add(usize_from(i.cast_unsigned()).wrapping_mul(4));
                    copy(s, oa, dst, usize_from(words));
                }
                Op::BoundCheck => {
                    let v = g(s, oa);
                    if v < st.c || v >= st.b {
                        fault!(ErrorKind::BoundCheck {
                            value: v.cast_signed(),
                            low: st.c,
                            high: st.b
                        });
                    }
                }
                Op::Push => {
                    let words = g(s, oa);
                    let word = core.x.ls_top.wrapping_add(core.x.pushed);
                    let addr = core.mem.ls_base.wrapping_add(word.wrapping_mul(4));
                    let pushed = core.x.pushed.saturating_add(words);
                    if core.x.ls_top.saturating_add(pushed) >= core.mem.ls_words {
                        fault!(ErrorKind::PushedTooMuch);
                    }
                    core.x.pushed = pushed;
                    core.mem.set_g(oc, addr);
                }
                Op::GAddress => fault!(ErrorKind::GAddress),
                Op::Unused | Op::Pop | Op::Bad => fault!(ErrorKind::BadOpcode(st.op as u16)),
                Op::JumpOutOfRange => fault!(ErrorKind::JumpOutOfRange),

                // ---- unsigned ------------------------------------------------------------
                Op::LeU => {
                    let v = ibool(g(s, oa) <= g(s, ob));
                    set(s, oc, v);
                }
                Op::LtU => {
                    let v = ibool(g(s, oa) < g(s, ob));
                    set(s, oc, v);
                }
                Op::DivU => {
                    let v = g(s, oa).checked_div(g(s, ob)).unwrap_or(0);
                    set(s, oc, v);
                }
                Op::RShiftU => {
                    let v = g(s, oa).wrapping_shr(g(s, ob));
                    set(s, oc, v);
                }
                Op::ConvUF => {
                    let v = g(s, oa) as f32;
                    setf(s, oc, v);
                }
                Op::ConvFU => {
                    let v = f2u(gf(s, oa));
                    set(s, oc, v);
                }

                // ---- 64-bit integers -----------------------------------------------------
                Op::AddI64 => i64_op!(i64::wrapping_add),
                Op::SubI64 => i64_op!(i64::wrapping_sub),
                Op::MulI64 => i64_op!(i64::wrapping_mul),
                Op::DivI64 => {
                    i64_op!(|a, b| a.checked_div(b).unwrap_or(if b == 0 { 0 } else { i64::MIN }))
                }
                Op::BitAndI64 => i64_op!(|a, b| a & b),
                Op::BitOrI64 => i64_op!(|a, b| a | b),
                Op::BitXorI64 => i64_op!(|a, b| a ^ b),
                Op::LShiftI64I | Op::RShiftI64I | Op::RShiftU64I => {
                    let a = get64!(oa);
                    let n = g(s, ob);
                    let v = match st.op {
                        Op::LShiftI64I => a.wrapping_shl(n),
                        Op::RShiftI64I => a.cast_signed().wrapping_shr(n).cast_unsigned(),
                        _ => a.wrapping_shr(n),
                    };
                    set64!(oc, v);
                }
                Op::LeI64 => i64_cmp!(|a, b| a.cast_signed() <= b.cast_signed()),
                Op::LtI64 => i64_cmp!(|a, b| a.cast_signed() < b.cast_signed()),
                Op::EqI64 => i64_cmp!(|a, b| a == b),
                Op::NeI64 => i64_cmp!(|a, b| a != b),
                Op::LeU64 => i64_cmp!(|a, b| a <= b),
                Op::LtU64 => i64_cmp!(|a, b| a < b),
                Op::DivU64 => {
                    let (a, b) = (get64!(oa), get64!(ob));
                    set64!(oc, a.checked_div(b).unwrap_or(0));
                }
                Op::ConvUI64 => {
                    let v = u64::from(g(s, oa));
                    set64!(oc, v);
                }
                Op::ConvII64 => {
                    let v = i64::from(g(s, oa).cast_signed()).cast_unsigned();
                    set64!(oc, v);
                }
                Op::ConvI64I => {
                    let v = g(s, oa);
                    set(s, oc, v);
                }
                Op::ConvI64F => {
                    let v = get64!(oa).cast_signed() as f32;
                    setf(s, oc, v);
                }
                Op::ConvU64F => {
                    let v = get64!(oa) as f32;
                    setf(s, oc, v);
                }
                Op::ConvFI64 => {
                    let v = d2i64(f64::from(gf(s, oa))).cast_unsigned();
                    set64!(oc, v);
                }
                Op::ConvFU64 => {
                    let v = d2u64(f64::from(gf(s, oa)));
                    set64!(oc, v);
                }

                // ---- doubles -------------------------------------------------------------
                Op::AddD => d_op!(+),
                Op::SubD => d_op!(-),
                Op::MulD => d_op!(*),
                Op::DivD => d_op!(/),
                Op::LeD => d_cmp!(<=),
                Op::LtD => d_cmp!(<),
                Op::EqD => d_cmp!(==),
                Op::NeD => d_cmp!(!=),
                Op::ConvFD => {
                    let v = f64::from(gf(s, oa)).to_bits();
                    set64!(oc, v);
                }
                Op::ConvDF => {
                    let v = f64::from_bits(get64!(oa)) as f32;
                    setf(s, oc, v);
                }
                Op::ConvI64D => {
                    let v = (get64!(oa).cast_signed() as f64).to_bits();
                    set64!(oc, v);
                }
                Op::ConvU64D => {
                    let v = (get64!(oa) as f64).to_bits();
                    set64!(oc, v);
                }
                Op::ConvDI64 => {
                    let v = d2i64(f64::from_bits(get64!(oa))).cast_unsigned();
                    set64!(oc, v);
                }
                Op::ConvDU64 => {
                    let v = d2u64(f64::from_bits(get64!(oa)));
                    set64!(oc, v);
                }

                // ---- bitfields -----------------------------------------------------------
                Op::BitExtendI | Op::BitExtendU => {
                    let (a, d) = (g(s, oa), g(s, ob));
                    let (w, p) = (d & 0xFF, d >> 8);
                    let v = if w == 0 {
                        0
                    } else {
                        let up = a.wrapping_shl(32u32.wrapping_sub(w).wrapping_sub(p));
                        let down = 32u32.wrapping_sub(w);
                        if st.op == Op::BitExtendI {
                            up.cast_signed().wrapping_shr(down).cast_unsigned()
                        } else {
                            up.wrapping_shr(down)
                        }
                    };
                    set(s, oc, v);
                }
                Op::BitCopyI => {
                    let (a, d, c) = (g(s, oa), g(s, ob), g(s, oc));
                    let (w, p) = (d & 0xFF, d >> 8);
                    let mask = if w >= 32 { u32::MAX } else { (1u32 << w).wrapping_sub(1) };
                    let v = (c & !mask.wrapping_shl(p)) | (a & mask).wrapping_shl(p);
                    set(s, oc, v);
                }
            }
            pc = pc.wrapping_add(1);
        }
    }
}

/// Reads a word of region S.
#[inline(always)]
fn g(s: &[u8], o: usize) -> u32 {
    crate::bytes::u32_at(s, o).unwrap_or(0)
}

/// Reads a float of region S.
#[inline(always)]
fn gf(s: &[u8], o: usize) -> f32 {
    f32::from_bits(g(s, o))
}

/// Reads a vector of region S.
#[inline(always)]
fn gv(s: &[u8], o: usize) -> [f32; 3] {
    [gf(s, o), gf(s, o.wrapping_add(4)), gf(s, o.wrapping_add(8))]
}

/// Writes a word of region S (the loader guarantees operands are in range; stray writes are
/// dropped).
#[inline(always)]
fn set(s: &mut [u8], o: usize, v: u32) {
    crate::bytes::put_u32(s, o, v);
}

/// Writes a float of region S.
#[inline(always)]
fn setf(s: &mut [u8], o: usize, v: f32) {
    set(s, o, v.to_bits());
}

/// Copies words within region S in increasing order (overlap-exact, like FTE).
#[inline(always)]
fn copy(s: &mut [u8], src: usize, dst: usize, words: usize) {
    for i in 0..words {
        let k = i.wrapping_mul(4);
        let v = g(s, src.wrapping_add(k));
        set(s, dst.wrapping_add(k), v);
    }
}

/// Warns about an invalid entity or field in a field load.
#[cold]
fn bad_field_access(core: &mut Core, e: u32, f: u32) {
    if e >= core.mem.num_edicts() {
        core.warn(WarningKind::BadEntity(e));
    } else {
        core.warn(WarningKind::BadField(f.cast_signed()));
    }
}

/// `STOREF_*`: writes `words` words from global `oc` into field `B` of entity `A`.
fn store_field(core: &mut Core, oa: usize, ob: usize, oc: usize, words: u32) {
    let (e, f) = (core.mem.g(oa), core.mem.g(ob));
    if e >= core.mem.num_edicts() {
        core.warn(WarningKind::BadEntity(e));
        return;
    }
    let Some(o) = core.mem.field_offset(e, f, words) else {
        core.warn(WarningKind::BadField(f.cast_signed()));
        return;
    };
    if core.mem.protected(e) {
        core.warn(WarningKind::ReadOnlyEntity(e));
        return;
    }
    for k in 0..usize_from(words) {
        let v = core.mem.g(oc.wrapping_add(k.wrapping_mul(4)));
        core.mem.set_ent_word(o.wrapping_add(k.wrapping_mul(4)), v);
    }
}

/// The text of a string reference, warning (and using `""`) if it resolves to nothing.
pub(crate) fn str_or_warn(core: &mut Core, r: u32) -> &[u8] {
    if core.str_bytes(r).is_none() {
        core.warn(WarningKind::BadString(r));
        return b"";
    }
    core.str_bytes(r).unwrap_or_default()
}

/// String equality with FTE's null rules: identical references are equal; a null reference
/// equals any string that resolves to empty; otherwise contents are compared.
pub(crate) fn strings_equal(core: &mut Core, a: u32, b: u32) -> bool {
    if a == b {
        return true;
    }
    if a == 0 || b == 0 {
        let other = if a == 0 { b } else { a };
        return str_or_warn(core, other).is_empty();
    }
    str_or_warn(core, a);
    str_or_warn(core, b);
    core.str_bytes(a).unwrap_or_default() == core.str_bytes(b).unwrap_or_default()
}

/// `EQ_S` / `NE_S` result words.
#[cold]
fn string_compare(core: &mut Core, op: Op, a: u32, b: u32) -> u32 {
    let eq = strings_equal(core, a, b);
    if op == Op::EqS {
        return fbool(eq);
    }
    if eq || !core.config.compat.ne_s_raw_strcmp {
        return fbool(!eq);
    }
    // FTE compatibility: the raw strcmp result, converted to float.
    let (sa, sb) = (core.str_bytes(a).unwrap_or_default(), core.str_bytes(b).unwrap_or_default());
    let diff = sa
        .iter()
        .chain(std::iter::once(&0))
        .zip(sb.iter().chain(std::iter::once(&0)))
        .map(|(x, y)| i32::from(*x).wrapping_sub(i32::from(*y)))
        .find(|d| *d != 0)
        .unwrap_or(0);
    (diff as f32).to_bits()
}

/// Reads `N` bytes through a pointer: `base + offset`.
///
/// Falls back like FTE: the `0xFFFFFFFF` sentinel (from `ADDRESS` on a protected entity) reads
/// as zero, and a temp-string handle as `base` reads inside that temp string (zero past its end).
pub(crate) fn ptr_read<const N: usize>(
    core: &mut Core,
    base: u32,
    offset: u32,
) -> Result<[u8; N], ErrorKind> {
    let addr = base.wrapping_add(offset);
    if let Some(bytes) = core.mem.read::<N>(addr) {
        return Ok(bytes);
    }
    ptr_read_slow(core, base, offset, addr)
}

#[cold]
fn ptr_read_slow<const N: usize>(
    core: &mut Core,
    base: u32,
    offset: u32,
    addr: u32,
) -> Result<[u8; N], ErrorKind> {
    if addr == u32::MAX {
        return Ok([0; N]);
    }
    let data = match classify(base) {
        StrKind::Temp(slot) => core.strings.temp_raw(slot),
        StrKind::Static(index) => core.strings.static_str(index),
        StrKind::Linear(_) => None,
    };
    let Some(data) = data else {
        return Err(ErrorKind::BadPointerRead(addr));
    };
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        let at = usize_from(offset).wrapping_add(i);
        *b = data.get(at).copied().unwrap_or(0);
    }
    Ok(out)
}

/// Writes bytes through a pointer: `base + offset`, with FTE's fallbacks (sentinel skip, temp
/// strings grow when written past their end). Protected entities are skipped with a warning.
pub(crate) fn ptr_write(
    core: &mut Core,
    base: u32,
    offset: u32,
    bytes: &[u8],
) -> Result<(), ErrorKind> {
    let addr = base.wrapping_add(offset);
    match core.mem.write(addr, bytes) {
        Ok(()) => Ok(()),
        Err(WriteError::Protected(e)) => {
            core.warn(WarningKind::ReadOnlyEntity(e));
            Ok(())
        }
        Err(err) => ptr_write_slow(core, base, offset, addr, bytes, err),
    }
}

#[cold]
fn ptr_write_slow(
    core: &mut Core,
    base: u32,
    offset: u32,
    addr: u32,
    bytes: &[u8],
    err: WriteError,
) -> Result<(), ErrorKind> {
    if addr == u32::MAX {
        return Ok(());
    }
    if base & TAG_MASK == TEMP_TAG {
        let start = usize_from(offset);
        let end = start.checked_add(bytes.len()).ok_or(ErrorKind::BadPointerWrite(addr))?;
        let data = core
            .strings
            .temp_raw_mut(base & INDEX_MASK, end)
            .ok_or(ErrorKind::BadPointerWrite(addr))?;
        data.get_mut(start..end).ok_or(ErrorKind::BadPointerWrite(addr))?.copy_from_slice(bytes);
        return Ok(());
    }
    Err(match err {
        WriteError::Null => ErrorKind::NullPointerWrite,
        WriteError::Invalid(_) | WriteError::Protected(_) => ErrorKind::BadPointerWrite(addr),
    })
}

/// The Hexen 2 read-modify-write opcodes that go through a pointer in `B`.
fn compound_pointer_store(
    core: &mut Core,
    op: Op,
    oa: usize,
    ob: usize,
    oc: usize,
) -> Result<(), ErrorKind> {
    let p = core.mem.g(ob);
    let vector = matches!(op, Op::MulStorePVF | Op::AddStorePV | Op::SubStorePV);
    let len: u32 = if vector { 12 } else { 4 };
    match core.mem.check_write(p, len) {
        Ok(_) => {}
        Err(WriteError::Protected(e)) => {
            core.warn(WarningKind::ReadOnlyEntity(e));
            return Ok(());
        }
        Err(WriteError::Null) => return Err(ErrorKind::NullPointerWrite),
        Err(WriteError::Invalid(_)) => return Err(ErrorKind::BadPointerWrite(p)),
    }
    let read = |core: &Core, at: u32| f32::from_bits(core.mem.read_u32(at).unwrap_or(0));
    let write = |core: &mut Core, at: u32, v: f32| {
        let _ = core.mem.write_u32(at, v.to_bits());
    };
    match op {
        Op::MulStorePVF => {
            let f = core.mem.gf(oa);
            for k in 0..3u32 {
                let at = p.wrapping_add(k.wrapping_mul(4));
                let v = read(core, at) * f;
                write(core, at, v);
                core.mem.set_gf(oc.wrapping_add(usize_from(k.wrapping_mul(4))), v);
            }
        }
        Op::AddStorePV | Op::SubStorePV => {
            for k in 0..3u32 {
                let at = p.wrapping_add(k.wrapping_mul(4));
                let a = core.mem.gf(oa.wrapping_add(usize_from(k.wrapping_mul(4))));
                let b = read(core, at);
                let v = if op == Op::AddStorePV { b + a } else { b - a };
                write(core, at, v);
                core.mem.set_gf(oc.wrapping_add(usize_from(k.wrapping_mul(4))), v);
            }
        }
        Op::BitSetStorePF | Op::BitClrStorePF => {
            let (b, a) = (f2i(read(core, p)), f2i(core.mem.gf(oa)));
            let v = if op == Op::BitSetStorePF { b | a } else { b & !a };
            write(core, p, v as f32);
        }
        _ => {
            let (b, a) = (read(core, p), core.mem.gf(oa));
            let v = match op {
                Op::MulStorePF => b * a,
                Op::DivStorePF => b / a,
                Op::AddStorePF => b + a,
                _ => b - a,
            };
            write(core, p, v);
            core.mem.set_gf(oc, v);
        }
    }
    Ok(())
}

/// The error for calling an unbound builtin.
#[cold]
fn missing_builtin(core: &Core, target: FuncRef) -> ErrorKind {
    let info = core
        .progs
        .get(usize::from(target.progs().0))
        .and_then(|p| p.program.function(target.index()));
    let (number, name) = match info {
        Some(i) => (
            match i.kind {
                crate::progs::FunctionKind::Builtin { number } => number,
                _ => 0,
            },
            i.name.into(),
        ),
        None => (0, Box::default()),
    };
    ErrorKind::BuiltinNotImplemented { number, name }
}

/// Resolves text for [`Core`] string references (kept here to sit next to its users).
impl Core {
    /// The text of a string reference, or `None` if it resolves to nothing.
    pub(crate) fn str_bytes(&self, r: u32) -> Option<&[u8]> {
        match classify(r) {
            StrKind::Linear(p) => self.mem.cstr(p),
            StrKind::Temp(slot) => self.strings.temp_raw(slot).map(until_nul),
            StrKind::Static(index) => self.strings.static_str(index),
        }
    }
}
