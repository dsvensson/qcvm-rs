// SPDX-License-Identifier: MIT OR Apache-2.0

//! QuakeC threads: `sleep` and `fork` (FTE_MULTITHREADED). `abort` lives in `introspect`.
//!
//! The host resumes sleeping threads with [`Vm::run_threads`](crate::Vm::run_threads).

use crate::builtins::Builtins;
use crate::error::VmError;
use crate::host::Host;
use crate::vm::Vm;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("sleep", sleep::<H>);
    b.set("fork", fork::<H>);
}

/// `void(float sleeptime) sleep`: suspends the current QuakeC thread for `sleeptime` seconds of
/// the `time` global. Execution returns to the engine at once (with 0 as the result of the
/// function the engine called); the thread continues after the call when it wakes.
///
/// # Errors
/// [`ErrorKind::OutOfMemory`](crate::ErrorKind::OutOfMemory) if the thread limit is reached.
pub fn sleep<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let delay = vm.arg_f32(0);
    if vm.suspend(delay, [0; 3])? {
        return Err(VmError::abort([0; 3]));
    }
    vm.warn("sleep called outside QuakeC");
    Ok(())
}

/// `float(optional float sleeptime) fork`: returns twice — now with 0, and after `sleeptime`
/// seconds in a copy of the current thread with 1. The copy should end with `abort()`.
///
/// # Errors
/// [`ErrorKind::OutOfMemory`](crate::ErrorKind::OutOfMemory) if the thread limit is reached.
pub fn fork<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let delay = if vm.argc() > 0 { vm.arg_f32(0) } else { 0.0 };
    vm.suspend(delay, [1.0f32.to_bits(), 0, 0])?;
    vm.ret_f32(0.0);
    Ok(())
}
