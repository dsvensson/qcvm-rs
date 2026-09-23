// SPDX-License-Identifier: MIT OR Apache-2.0

//! The interface between a VM and the program embedding it.

use crate::error::{VmError, Warning};
use crate::vm::{StateOp, Vm};

/// Services the embedding program provides to a VM.
///
/// Every method has a default, so a minimal host is `impl Host for MyState {}`. Host-specific
/// builtins receive `&mut Self` alongside the VM, which is where their state lives.
pub trait Host: Sized {
    /// A non-fatal problem in QuakeC (bad entity, bad string reference, …). Default: ignored.
    fn warning(&mut self, warning: &Warning) {
        let _ = warning;
    }

    /// Text from `print` and similar builtins. Default: discarded.
    fn print(&mut self, text: &[u8]) {
        let _ = text;
    }

    /// Text from `dprint` (developer messages). Default: discarded.
    fn dprint(&mut self, text: &[u8]) {
        let _ = text;
    }

    /// Performs an animation opcode (`STATE`, `CSTATE`, `CWSTATE`, `THINKTIME`).
    ///
    /// Return `Ok(false)` to let the VM apply FTE's default behaviour (which updates `self`'s
    /// `frame`, `think` and `nextthink` fields). Default: `Ok(false)`.
    ///
    /// # Errors
    /// An error aborts the QuakeC call that executed the opcode.
    fn state_op(&mut self, vm: &mut Vm<Self>, op: StateOp) -> Result<bool, VmError> {
        let _ = (vm, op);
        Ok(false)
    }
}

/// A host that provides nothing (every hook keeps its default).
#[derive(Clone, Copy, Debug, Default)]
pub struct NullHost;

impl Host for NullHost {}
