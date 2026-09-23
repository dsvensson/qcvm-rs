// SPDX-License-Identifier: MIT OR Apache-2.0

//! `addprogs`: loading further progs into the VM (docs/spec/builtins.md).

use crate::builtins::Builtins;
use crate::error::VmError;
use crate::host::Host;
use crate::vm::Vm;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("addprogs", addprogs::<H>);
}

/// `float(string progsname) addprogs`: loads a progs through [`Host::load_progs`] and returns
/// its progs number, or −1 if the host cannot provide it or it does not fit.
///
/// # Errors
/// Errors raised by the new progs' `init` function.
pub fn addprogs<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let name = vm.arg_str(0).to_vec();
    let result = match host.load_progs(&name) {
        Some(program) => match vm.add_progs(host, program) {
            Ok(pr) => f32::from(pr.0),
            Err(e) if matches!(e.kind(), crate::ErrorKind::OutOfMemory(_)) => {
                vm.warn(format!("addprogs {}: {}", String::from_utf8_lossy(&name), e.kind()));
                -1.0
            }
            Err(e) => return Err(e),
        },
        None => -1.0,
    };
    vm.ret_f32(result);
    Ok(())
}
