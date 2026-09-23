// SPDX-License-Identifier: MIT OR Apache-2.0

//! Helpers shared by the standard builtins.

use crate::host::Host;
use crate::vm::Vm;
use crate::vm::num::f2i;

/// Maximum size of a concatenated variadic string argument (FTE's buffer size).
pub(crate) const CONCAT_MAX: usize = 65_543;

/// FTE's "concatenated varargs": the string arguments from `from` to the last one, joined.
/// A single argument is used as is; two or more are joined and truncated to [`CONCAT_MAX`].
pub(crate) fn args_concat<H: Host>(vm: &Vm<H>, from: usize) -> Vec<u8> {
    let argc = vm.argc().min(8);
    if argc <= from {
        return Vec::new();
    }
    if argc == from.saturating_add(1) {
        return vm.arg_str(from).to_vec();
    }
    let mut out = Vec::new();
    for i in from..argc {
        let s = vm.arg_str(i);
        let room = CONCAT_MAX.saturating_sub(out.len());
        out.extend_from_slice(s.get(..s.len().min(room)).unwrap_or_default());
    }
    out
}

/// Argument `i` as a float truncated to an integer (C semantics, x86 overflow behaviour).
pub(crate) fn arg_int<H: Host>(vm: &Vm<H>, i: usize) -> i32 {
    f2i(vm.arg_f32(i))
}

/// Argument `i` as a float, or `default` if fewer arguments were passed.
pub(crate) fn opt_f32<H: Host>(vm: &Vm<H>, i: usize, default: f32) -> f32 {
    if vm.argc() > i { vm.arg_f32(i) } else { default }
}
