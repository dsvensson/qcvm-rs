// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests of the standard builtins (see `src/stdlib`) other than the string and formatting ones:
//! maths, vectors, entities, reflection, hash tables, string buffers, memory, JSON,
//! introspection, time and host calls.

mod entity;
mod hash;
mod hostcalls;
mod introspect;
#[cfg(feature = "json")]
mod json;
mod math;
mod memory;
mod reflect;
mod strbuf;
mod time;
mod vector;

use qcvm::{Arg, EntRef, Op, Ptr};

use crate::support::asm::Asm;
use crate::support::harness::Harness;

/// The first word of parameter slot `i` after a call (how `__out` parameters come back).
pub fn parm_word(h: &Harness, i: u32) -> u32 {
    // The harness declares `self` first, right after the 28 reserved global words.
    let gbase = h.vm.global::<EntRef>("self").unwrap().ptr().0 - 28 * 4;
    let addr = gbase + (4 + 3 * i) * 4;
    u32::from_le_bytes(h.vm.read_mem(Ptr(addr), 4).unwrap().try_into().unwrap())
}

/// Adds `peek(pointer p, int i) = p[i]` (an int load through a pointer), so tests can read memory
/// the way QuakeC does, temp buffers included.
pub fn add_peek(asm: &mut Asm) {
    let f = asm.function("peek", &[1, 1], 1);
    asm.emit(Op::LoadPI, f.local(0), f.local(1), f.local(2));
    asm.emit(Op::Return, f.local(2), 0, 0);
}

/// Calls `peek` (see [`add_peek`]).
pub fn peek(h: &mut Harness, p: u32, i: i32) -> i32 {
    h.i("peek", &[Arg::Raw([p, 0, 0]), Arg::Int(i)])
}

/// A raw one-word argument (pointers, fields, functions, entities).
pub fn w(x: u32) -> Arg<'static> {
    Arg::Raw([x, 0, 0])
}

/// An entity argument.
pub fn ent(e: EntRef) -> Arg<'static> {
    Arg::Ent(e)
}

/// The text of warning messages recorded by the host.
pub fn warnings(h: &Harness) -> Vec<String> {
    h.host.warnings.iter().map(|w| w.kind.to_string()).collect()
}
