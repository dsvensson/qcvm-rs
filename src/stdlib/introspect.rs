// SPDX-License-Identifier: MIT OR Apache-2.0

//! VM introspection: checkbuiltin, isfunction, callfunction, extern*, abort, tracing (docs/spec/builtins.md).
//!
//! Also home of small helpers the other standard builtins share: FTE's "carry on after a
//! builtin error" rule, `__out` parameters and run-time function lookup.

use crate::builtins::{BuiltinFn, Builtins, Numbering, builtin_number};
use crate::bytes::usize_from;
use crate::error::{ErrorKind, VmError};
use crate::host::{DumpKind, Host};
use crate::progs::Type;
use crate::stdlib::util::args_concat;
use crate::value::{Arg, FuncRef, PrNum};
use crate::vm::Vm;
use crate::vm::core::{Core, OFS_PARM0};

use super::util::arg_int;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("checkbuiltin", checkbuiltin::<H>);
    b.set("isfunction", isfunction::<H>);
    set_fte(b, 605, "callfunction", callfunction::<H>);
    b.set("externcall", externcall::<H>);
    b.set("externvalue", externvalue::<H>);
    b.set("externset", externset::<H>);
    set_fte(b, 205, "externrefcall", externrefcall::<H>);
    b.set("abort", abort::<H>);
    b.set("traceon", traceon::<H>);
    b.set("traceoff", traceoff::<H>);
    b.set("breakpoint", breakpoint::<H>);
    b.set("coredump", coredump::<H>);
    b.set("eprint", eprint::<H>);
    b.set("stackdump", stackdump::<H>);
    b.set("crash", crash::<H>);
}

/// Registers a builtin under FTE's number `number` when the registry's numbering table does not
/// list it (FTE's engine tables number a few builtins its extension dump omits).
pub(crate) fn set_fte<H: Host>(b: &mut Builtins<H>, number: u32, name: &str, f: BuiltinFn<H>) {
    let numbering = b.numbering();
    if numbering == Numbering::None || builtin_number(numbering, name).is_some() {
        b.set(name, f);
    } else {
        b.set_numbered(number, name, f);
    }
}

// ---- shared helpers ---------------------------------------------------------------------------

/// FTE's builtin error for builtins that carry on afterwards: in developer mode a warning (and a
/// zeroed return value) after which the caller continues with its fallback; otherwise fatal.
pub(crate) fn soft_error<H: Host>(vm: &mut Vm<H>, msg: impl Into<String>) -> Result<(), VmError> {
    if vm.config().developer {
        vm.warn(msg);
        vm.ret_raw([0; 3]);
        Ok(())
    } else {
        Err(VmError::builtin(msg))
    }
}

/// Writes the first word of parameter slot `i` (an `__out` parameter the compiler copies back
/// after the call).
pub(crate) fn set_arg_word<H: Host>(vm: &mut Vm<H>, i: usize, word: u32) {
    if i < 8 {
        let at = vm.core.gbase().wrapping_add(OFS_PARM0).wrapping_add(i.wrapping_mul(12));
        vm.core.mem.set_g(at, word);
    }
}

/// The progs a QuakeC progs number designates, in search order: 0 is the main progs, −1 the
/// progs running now, −2 every progs (first that has the name wins).
pub(crate) fn progs_candidates(core: &Core, prnum: i32) -> Vec<usize> {
    match prnum {
        -1 => vec![usize::from(core.x.prnum)],
        -2 => (0..core.progs.len()).collect(),
        n => usize::try_from(n).ok().filter(|&n| n < core.progs.len()).into_iter().collect(),
    }
}

/// Looks up a function by name like FTE's `PR_FindFunction`: an `N:` prefix selects progs `N`,
/// and a function-typed global of the same name supplies the function's *current* value (so
/// QuakeC can redirect it). Returns `None` if nothing is found or the global holds no function.
pub(crate) fn find_function_rt(core: &Core, prnum: i32, name: &[u8]) -> Option<FuncRef> {
    let (candidates, name) = match name.iter().position(|&b| b == b':') {
        Some(colon) => {
            let n = parse_int_prefix(name.get(..colon).unwrap_or_default());
            (progs_candidates(core, n), name.get(colon.saturating_add(1)..).unwrap_or_default())
        }
        None => (progs_candidates(core, prnum), name),
    };
    candidates.into_iter().find_map(|pr| find_in_progs(core, pr, name))
}

fn find_in_progs(core: &Core, pr: usize, name: &[u8]) -> Option<FuncRef> {
    let ps = core.progs.get(pr)?;
    let index = *ps.program.functions_by_name.get(name)?;
    let def = match ps.program.global_def_raw(name) {
        Some(d) if d.ty == Type::Function => Some(d.ofs),
        _ => ps
            .program
            .global_defs()
            .find(|d| d.name == name && d.ty == Type::Function)
            .map(|d| d.offset),
    };
    let pr = u8::try_from(pr).ok()?;
    if let Some(ofs) = def {
        let v = core.mem.g(core.global_offset(pr, ofs)?);
        return (v != 0).then_some(FuncRef(v));
    }
    Some(FuncRef::new(PrNum(pr), index))
}

/// C `atoi` on a short prefix (for `N:name` function references).
fn parse_int_prefix(s: &[u8]) -> i32 {
    let s = s.trim_ascii_start();
    let (neg, digits) = match s.split_first() {
        Some((b'-', rest)) => (true, rest),
        Some((b'+', rest)) => (false, rest),
        _ => (false, s),
    };
    let mut v: i64 = 0;
    for &d in digits.iter().take_while(|d| d.is_ascii_digit()) {
        v = v.saturating_mul(10).saturating_add(i64::from(d.wrapping_sub(b'0')));
    }
    let v = if neg { v.saturating_neg() } else { v };
    i32::try_from(v).unwrap_or(if neg { i32::MIN } else { i32::MAX })
}

/// Arguments `from..argc` as raw words, for passing on to another function.
fn raw_args<H: Host>(vm: &Vm<H>, from: usize) -> Vec<[u32; 3]> {
    (from..vm.argc().min(8)).map(|i| vm.arg_raw(i)).collect()
}

fn call_raw<H: Host>(
    vm: &mut Vm<H>,
    host: &mut H,
    f: FuncRef,
    args: &[[u32; 3]],
) -> Result<(), VmError> {
    let args: Vec<Arg<'_>> = args.iter().map(|&w| Arg::Raw(w)).collect();
    let ret = vm.call(host, f, &args)?;
    vm.ret_raw(ret.0);
    Ok(())
}

// ---- builtins ---------------------------------------------------------------------------------

/// `float checkbuiltin(__variant funcref)`: whether the function is a builtin this VM
/// implements.
pub fn checkbuiltin<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let bound = vm.is_builtin_bound(vm.arg_func(0));
    vm.ret_f32(if bound { 1.0 } else { 0.0 });
    Ok(())
}

/// `float isfunction(string name)`: whether a function of that name exists.
pub fn isfunction<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let found = find_function_rt(&vm.core, -2, vm.arg_str(0)).is_some();
    vm.ret_f32(if found { 1.0 } else { 0.0 });
    Ok(())
}

/// `void callfunction(..., string name)`: calls the named function with the preceding arguments;
/// does nothing if there is no such function. The callee's return value is left in the return
/// slot.
pub fn callfunction<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let argc = vm.argc().min(8);
    let Some(last) = argc.checked_sub(1) else {
        return Err(VmError::builtin("callfunction needs at least one argument"));
    };
    let f = find_function_rt(&vm.core, -2, vm.arg_str(last));
    let args: Vec<[u32; 3]> = (0..last).map(|i| vm.arg_raw(i)).collect();
    match f {
        Some(f) => call_raw(vm, host, f, &args),
        None => Ok(()),
    }
}

/// `__variant externcall(float prnum, string name, ...)`: calls a function of another progs by
/// name (prnum 0 = main, −1 = current, −2 = any). A missing function calls
/// `MissingFunc(name, ...)` instead if the progs defines it.
pub fn externcall<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let prnum = arg_int(vm, 0);
    let name_ref = vm.arg_u32(1);
    let name = vm.arg_str(1).to_vec();
    let args = raw_args(vm, 2);
    if let Some(f) = find_function_rt(&vm.core, prnum, &name) {
        return call_raw(vm, host, f, &args);
    }
    match find_function_rt(&vm.core, prnum, b"MissingFunc") {
        Some(f) => {
            let mut with_name = vec![[name_ref, 0, 0]];
            with_name.extend(args.into_iter().take(7));
            call_raw(vm, host, f, &with_name)
        }
        None => Err(VmError::builtin(format!(
            "Couldn't find function {}",
            String::from_utf8_lossy(&name)
        ))),
    }
}

/// `__variant externrefcall(float prnum, __variant func, ...)`: calls a function reference with
/// the remaining arguments.
pub fn externrefcall<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let f = vm.arg_func(1);
    let args = raw_args(vm, 2);
    call_raw(vm, host, f, &args)
}

/// The byte address and type of global `name` in the progs `prnum` selects.
fn find_global(core: &Core, prnum: i32, name: &[u8]) -> Option<(u32, Type)> {
    progs_candidates(core, prnum).into_iter().find_map(|pr| {
        let def = core.progs.get(pr)?.program.global_def_raw(name)?;
        let addr = core.global_offset(u8::try_from(pr).ok()?, def.ofs)?;
        Some((u32::try_from(addr).ok()?, def.ty))
    })
}

/// `__variant externvalue(float prnum, string name, ...)`: a global's value (three words), or
/// with a leading `&` its address; if no such global exists, the function of that name (or 0).
pub fn externvalue<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let prnum = arg_int(vm, 0);
    let name = args_concat(vm, 1);
    if let Some(stripped) = name.strip_prefix(b"&") {
        let addr = find_global(&vm.core, prnum, stripped).map_or(0, |(a, _)| a);
        vm.ret_raw([addr, 0, 0]);
        return Ok(());
    }
    let words = match find_global(&vm.core, prnum, &name) {
        Some((addr, _)) => {
            let at = usize_from(addr);
            [0usize, 4, 8].map(|k| vm.core.mem.g(at.wrapping_add(k)))
        }
        None => [find_function_rt(&vm.core, prnum, &name).map_or(0, |f| f.0), 0, 0],
    };
    vm.ret_raw(words);
    Ok(())
}

/// `void externset(float prnum, __variant value, string name, ...)`: writes a global of another
/// progs (three words for vectors, two for 64-bit types, else one).
pub fn externset<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let prnum = arg_int(vm, 0);
    let value = vm.arg_raw(1);
    let name = args_concat(vm, 2);
    if let Some((addr, ty)) = find_global(&vm.core, prnum, &name) {
        let words = match ty {
            Type::Vector => 3,
            Type::Int64 | Type::UInt64 | Type::Double => 2,
            _ => 1,
        };
        let at = usize_from(addr);
        for (k, w) in value.into_iter().take(words).enumerate() {
            vm.core.mem.set_g(at.wrapping_add(k.wrapping_mul(4)), w);
        }
    }
    Ok(())
}

/// `void abort(optional __variant ret)`: unwinds every QuakeC frame up to the engine call that
/// started them, which then returns `ret`.
pub fn abort<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let ret = if vm.argc() > 0 { vm.arg_raw(0) } else { [0; 3] };
    Err(VmError::abort(ret))
}

/// `void traceon()`: switches statement tracing on.
pub fn traceon<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    vm.core.trace = true;
    Ok(())
}

/// `void traceoff()`: switches statement tracing off.
pub fn traceoff<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    vm.core.trace = false;
    Ok(())
}

/// `void breakpoint()`: a "break statement" warning with the current stack.
pub fn breakpoint<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    vm.warn("break statement");
    Ok(())
}

/// `void coredump()`: sends a dump of the call stack, the saved globals and every entity to
/// [`Host::dump`].
pub fn coredump<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let text = crate::stdlib::reflect::coredump_text(&vm.core);
    host.dump(DumpKind::CoreDump, &text);
    Ok(())
}

/// `void eprint(entity e)`: sends the entity's non-default fields to [`Host::dump`].
pub fn eprint<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let e = crate::stdlib::entity::ent_arg(vm, 0);
    let mut text = format!("Entity {e}:\n").into_bytes();
    text.extend_from_slice(&crate::stdlib::reflect::entity_block(&vm.core, e));
    text.push(b'\n');
    host.dump(DumpKind::Entity, &text);
    Ok(())
}

/// `void stackdump()` (menu): sends the QuakeC call stack to [`Host::dump`].
pub fn stackdump<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let text = vm.backtrace().to_string();
    host.dump(DumpKind::Trace, text.as_bytes());
    Ok(())
}

/// `void crash()` (menu): a fatal error naming the builtin.
pub fn crash<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let name = vm.builtin().name;
    let name = if name.is_empty() { b"?unknown?".as_slice() } else { name };
    let mut msg = name.to_vec();
    msg.extend_from_slice(b" called");
    Err(VmError::new(ErrorKind::QcError(msg.into())))
}
