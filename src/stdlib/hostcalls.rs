// SPDX-License-Identifier: MIT OR Apache-2.0

//! Builtins that forward to the host: print, dprint, error, objerror, cvars, localcmd, … (docs/spec/builtins.md).
//!
//! Variadic text builtins concatenate their string arguments first.

use crate::builtins::Builtins;
use crate::error::{ErrorKind, VmError};
use crate::host::{DumpKind, Host};
use crate::value::EntRef;
use crate::vm::Vm;

use super::util::arg_int;
use super::util::args_concat;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("print", print::<H>);
    b.set("dprint", dprint::<H>);
    b.set("cprint", cprint::<H>);
    b.set("error", error::<H>);
    b.set("objerror", objerror::<H>);
    b.set("localcmd", localcmd::<H>);
    b.set("cvar", cvar::<H>);
    b.set("cvar_string", cvar_string::<H>);
    b.set("cvar_set", cvar_set::<H>);
    b.set("cvar_type", cvar_type::<H>);
    b.set("cvar_defstring", cvar_defstring::<H>);
    b.set("cvar_description", cvar_description::<H>);
    b.set("registercvar", registercvar::<H>);
    b.set("checkextension", checkextension::<H>);
    b.set("checkcommand", checkcommand::<H>);
    b.set("registercommand", registercommand::<H>);
    b.set("isdemo", isdemo::<H>);
    b.set("isserver", isserver::<H>);
    b.set("cvars_haveunsaved", cvars_haveunsaved::<H>);
}

fn ret_bool<H: Host>(vm: &mut Vm<H>, b: bool) {
    vm.ret_f32(if b { 1.0 } else { 0.0 });
}

/// Returns `text` as a temp string, or null for `None`.
fn ret_opt_str<H: Host>(vm: &mut Vm<H>, text: Option<Vec<u8>>) -> Result<(), VmError> {
    match text {
        Some(t) => vm.ret_str(&t),
        None => {
            vm.ret_raw([0; 3]);
            Ok(())
        }
    }
}

/// `void print(string, ...)`: prints the concatenated arguments ([`Host::print`]).
pub fn print<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    host.print(&args_concat(vm, 0));
    Ok(())
}

/// `void dprint(string, ...)`: prints only in developer mode ([`Host::dprint`]).
pub fn dprint<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    if vm.config().developer {
        host.dprint(&args_concat(vm, 0));
    }
    Ok(())
}

/// `void cprint(string, ...)`: centre-prints the concatenated arguments ([`Host::centerprint`]).
pub fn cprint<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    host.centerprint(&args_concat(vm, 0));
    Ok(())
}

/// `void error(string, ...)`: aborts with [`ErrorKind::QcError`] carrying the concatenated
/// message.
pub fn error<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    Err(VmError::new(ErrorKind::QcError(args_concat(vm, 0).into())))
}

/// `void objerror(string, ...)`: dumps `self`'s fields ([`Host::dump`] with
/// [`DumpKind::ObjError`]), frees `self` and aborts with [`ErrorKind::QcError`] (fatal, as in
/// FTE's CSQC).
pub fn objerror<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let msg = args_concat(vm, 0);
    if let Ok(g) = vm.current_global::<EntRef>("self") {
        let e = vm.get(g).0;
        let e = if e < vm.num_edicts() { e } else { 0 };
        let mut text = format!("Entity {e}:\n").into_bytes();
        text.extend_from_slice(&super::reflect::entity_block(&vm.core, e));
        host.dump(DumpKind::ObjError, &text);
        if e != 0 {
            super::entity::remove_entity(vm, host, EntRef(e), false);
        }
    }
    Err(VmError::new(ErrorKind::QcError(msg.into())))
}

/// `void localcmd(string, ...)`: appends the concatenated arguments to the console command
/// buffer ([`Host::localcmd`]).
pub fn localcmd<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    host.localcmd(&args_concat(vm, 0));
    Ok(())
}

/// `float cvar(string name)`: the cvar's numeric value ([`Host::cvar_float`]).
pub fn cvar<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let v = host.cvar_float(vm.arg_str(0));
    vm.ret_f32(v);
    Ok(())
}

/// `string cvar_string(string name)`: the cvar's value, or null if it does not exist.
pub fn cvar_string<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let v = host.cvar_string(vm.arg_str(0)).map(std::borrow::Cow::into_owned);
    ret_opt_str(vm, v)
}

/// `void cvar_set(string name, string value)`.
pub fn cvar_set<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    host.cvar_set(vm.arg_str(0), vm.arg_str(1));
    Ok(())
}

/// `float cvar_type(string name)`: 0 if the cvar does not exist, else bits 1 exists, 2 archived,
/// 4 private, 8 engine-created, 16 has a description, 32 read-only.
pub fn cvar_type<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let flags = host.cvar_info(vm.arg_str(0)).map_or(0, |i| i.flags);
    vm.ret_f32(flags as f32);
    Ok(())
}

/// `string cvar_defstring(string name)`: the cvar's default value, or null.
pub fn cvar_defstring<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let v = host.cvar_info(vm.arg_str(0)).map(|i| i.default);
    ret_opt_str(vm, v)
}

/// `string cvar_description(string name)`: the cvar's description, or null.
pub fn cvar_description<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let v = host.cvar_info(vm.arg_str(0)).and_then(|i| i.description);
    ret_opt_str(vm, v)
}

/// `float registercvar(string name, string value, optional float flags)`: creates the cvar if it
/// does not exist; 1 if created, 0 if it already existed. FTE ignores `value` unless `flags` is
/// passed too; qcvm always uses it.
pub fn registercvar<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let value = if vm.argc() > 1 { vm.arg_str(1) } else { b"" };
    let flags = if vm.argc() > 2 { arg_int(vm, 2).cast_unsigned() } else { 0 };
    let created = host.register_cvar(vm.arg_str(0), value, flags);
    ret_bool(vm, created);
    Ok(())
}

/// `float checkextension(string name)`: whether the host supports the extension
/// ([`Host::check_extension`]; case-sensitive).
pub fn checkextension<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let has = host.check_extension(vm.arg_str(0));
    ret_bool(vm, has);
    Ok(())
}

/// `float checkcommand(string name)`: 1 command, 2 alias, 3 cvar, 0 none.
pub fn checkcommand<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let kind = host.check_command(vm.arg_str(0));
    vm.ret_f32(kind as f32);
    Ok(())
}

/// `void registercommand(string name)`: a console command that calls back into QuakeC.
pub fn registercommand<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    host.register_command(vm.arg_str(0));
    Ok(())
}

/// `float isdemo()`: 0 live, 1 playing a demo, 2 playing an MVD.
pub fn isdemo<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    vm.ret_f32(host.is_demo());
    Ok(())
}

/// `float isserver()`: whether a local server is running.
pub fn isserver<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let b = host.is_server();
    ret_bool(vm, b);
    Ok(())
}

/// `float cvars_haveunsaved()`: whether archived cvars changed since the config was saved.
pub fn cvars_haveunsaved<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let b = host.cvars_have_unsaved();
    ret_bool(vm, b);
    Ok(())
}
