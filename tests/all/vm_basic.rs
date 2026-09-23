// SPDX-License-Identifier: MIT OR Apache-2.0

//! Calls, returns, builtins, re-entrancy, errors, entities and strings.

use std::sync::Arc;

use qcvm::{
    Arg, Builtins, EntRef, ErrorKind, FuncRef, Host, Numbering, Op, Program, ProgsFormat, StrRef,
    Vm, VmConfig, VmError, Warning,
};

use crate::support::asm::{Asm, OFS_PARM0, OFS_RETURN, parm, ty};

/// A host that records what builtins and warnings saw.
#[derive(Default)]
pub struct TestHost {
    pub log: Vec<String>,
    pub warnings: Vec<Warning>,
    pub callback: Option<FuncRef>,
}

impl Host for TestHost {
    fn warning(&mut self, w: &Warning) {
        self.warnings.push(w.clone());
    }
}

pub fn vm_with(asm: &Asm, builtins: Builtins<TestHost>) -> Vm<TestHost> {
    let program = Arc::new(Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap());
    Vm::new(program, Arc::new(builtins), VmConfig::default()).unwrap()
}

pub fn vm(asm: &Asm) -> Vm<TestHost> {
    vm_with(asm, Builtins::empty(Numbering::None))
}

fn func(vm: &Vm<TestHost>, name: &str) -> FuncRef {
    vm.find_function(name).unwrap_or_else(|| panic!("no function {name}"))
}

#[test]
fn add_with_arguments() {
    let mut asm = Asm::new();
    let f = asm.function("add", &[1, 1], 1);
    asm.emit(Op::AddF, f.local(0), f.local(1), f.local(2));
    asm.emit(Op::Return, f.local(2), 0, 0);
    let mut vm = vm(&asm);
    let mut host = TestHost::default();
    let r = vm.call(&mut host, func(&vm, "add"), &[Arg::Float(2.5), Arg::Float(4.0)]).unwrap();
    assert_eq!(r.f32(), 6.5);
}

/// fib(n) = n < 2 ? n : fib(n-1) + fib(n-2), exercising locals across recursion.
fn fib_program() -> Asm {
    let mut asm = Asm::new();
    let two = asm.float(2.0);
    let one = asm.float(1.0);
    let fib_g = asm.global("fib_ref", ty::FUNCTION, &[0]);
    let f = asm.function("fib", &[1], 3);
    let (n, t, acc) = (f.local(0), f.local(1), f.local(2));
    asm.emit(Op::LtF, n, two, t);
    let br = asm.emit(Op::IfNotI, t, 0, 0);
    asm.emit(Op::Return, n, 0, 0);
    let rec = asm.here();
    asm.patch_jump(br, 1, rec);
    asm.emit(Op::SubF, n, one, parm(0));
    asm.emit(Op::Call1, fib_g, 0, 0);
    asm.emit(Op::StoreF, OFS_RETURN, acc, 0);
    asm.emit(Op::SubF, n, two, parm(0));
    asm.emit(Op::Call1, fib_g, 0, 0);
    asm.emit(Op::AddF, acc, OFS_RETURN, acc);
    asm.emit(Op::Return, acc, 0, 0);
    asm.set_global(fib_g, f.index);
    asm
}

#[test]
fn recursion_preserves_locals() {
    let asm = fib_program();
    let mut vm = vm(&asm);
    let mut host = TestHost::default();
    let fib = func(&vm, "fib");
    for (n, want) in [(0.0, 0.0), (1.0, 1.0), (2.0, 1.0), (10.0, 55.0), (20.0, 6765.0)] {
        let r = vm.call(&mut host, fib, &[Arg::Float(n)]).unwrap();
        assert_eq!(r.f32(), want, "fib({n})");
    }
}

fn b_log(vm: &mut Vm<TestHost>, host: &mut TestHost) -> Result<(), VmError> {
    let args: Vec<String> = (0..vm.argc()).map(|i| vm.arg_f32(i).to_string()).collect();
    host.log.push(format!("log({})", args.join(",")));
    vm.ret_f32(vm.arg_f32(0) * 10.0);
    Ok(())
}

fn b_callback(vm: &mut Vm<TestHost>, host: &mut TestHost) -> Result<(), VmError> {
    // Read arguments before re-entering QuakeC: the parameter slots are shared.
    let x = vm.arg_f32(0);
    let cb = host.callback.unwrap();
    let r = vm.call(host, cb, &[Arg::Float(x + 1.0)])?;
    vm.ret_f32(r.f32() * 2.0);
    Ok(())
}

fn b_fail(_vm: &mut Vm<TestHost>, _host: &mut TestHost) -> Result<(), VmError> {
    Err(VmError::builtin("boom"))
}

#[test]
fn builtins_by_number_and_name_and_lazy_failure() {
    let mut asm = Asm::new();
    let log = asm.builtin("log", 7, 2);
    let named = asm.builtin("named_log", 0, 1);
    let missing = asm.builtin("missing", 99, 0);
    let log_g = asm.global("log_g", ty::FUNCTION, &[log]);
    let named_g = asm.global("named_g", ty::FUNCTION, &[named]);
    let missing_g = asm.global("missing_g", ty::FUNCTION, &[missing]);
    let (one, two) = (asm.float(1.0), asm.float(2.0));
    asm.function("main", &[], 0);
    asm.emit(Op::StoreF, one, parm(0), 0);
    asm.emit(Op::StoreF, two, parm(1), 0);
    asm.emit(Op::Call2, log_g, 0, 0);
    asm.emit(Op::StoreF, OFS_RETURN, parm(0), 0);
    asm.emit(Op::Call1, named_g, 0, 0);
    asm.emit(Op::Return, OFS_RETURN, 0, 0);
    asm.function("calls_missing", &[], 0);
    asm.emit(Op::Call0, missing_g, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);

    let mut b = Builtins::empty(Numbering::None);
    b.set_numbered(7, "log", b_log);
    b.set("named_log", b_log);
    let mut vm = vm_with(&asm, b);
    let unbound = vm.unbound_builtins();
    assert_eq!(unbound.len(), 1);
    assert_eq!(&*unbound[0].name, b"missing");
    assert_eq!(unbound[0].number, 99);

    let mut host = TestHost::default();
    let r = vm.call(&mut host, func(&vm, "main"), &[]).unwrap();
    assert_eq!(r.f32(), 100.0);
    assert_eq!(host.log, ["log(1,2)", "log(10)"]);

    let err = vm.call(&mut host, func(&vm, "calls_missing"), &[]).unwrap_err();
    assert_eq!(
        *err.kind(),
        ErrorKind::BuiltinNotImplemented { number: 99, name: (*b"missing").into() }
    );
    assert_eq!(&*err.backtrace().0[0].name, b"calls_missing");
    // The VM is still usable.
    assert_eq!(vm.call(&mut host, func(&vm, "main"), &[]).unwrap().f32(), 100.0);
}

#[test]
fn builtins_can_call_back_into_quakec() {
    let mut asm = Asm::new();
    let cb = asm.builtin("callback", 1, 1);
    let cb_g = asm.global("cb_g", ty::FUNCTION, &[cb]);
    let three = asm.float(3.0);
    // inner(x) = x * x
    let inner = asm.function("inner", &[1], 1);
    asm.emit(Op::MulF, inner.local(0), inner.local(0), inner.local(1));
    asm.emit(Op::Return, inner.local(1), 0, 0);
    // outer(y) { local z = y + 3; r = callback(y); return r + z; }  -- z must survive the callback
    let outer = asm.function("outer", &[1], 2);
    let (y, z) = (outer.local(0), outer.local(1));
    asm.emit(Op::AddF, y, three, z);
    asm.emit(Op::StoreF, y, parm(0), 0);
    asm.emit(Op::Call1, cb_g, 0, 0);
    asm.emit(Op::AddF, OFS_RETURN, z, outer.local(2));
    asm.emit(Op::Return, outer.local(2), 0, 0);

    let mut b = Builtins::empty(Numbering::None);
    b.set_numbered(1, "callback", b_callback);
    let mut vm = vm_with(&asm, b);
    let mut host = TestHost { callback: Some(func(&vm, "inner")), ..TestHost::default() };
    // outer(4): z = 7; callback(4) = inner(5) * 2 = 50; 50 + 7 = 57
    let r = vm.call(&mut host, func(&vm, "outer"), &[Arg::Float(4.0)]).unwrap();
    assert_eq!(r.f32(), 57.0);
}

#[test]
fn errors_unwind_and_carry_backtraces() {
    let mut asm = Asm::new();
    let fail = asm.builtin("fail", 1, 0);
    let fail_g = asm.global("fail_g", ty::FUNCTION, &[fail]);
    let saved = asm.global("saved", ty::FLOAT, &[]);
    let seven = asm.float(7.0);
    let inner = asm.function("inner", &[], 1);
    asm.emit(Op::StoreF, seven, inner.local(0), 0);
    asm.emit(Op::Call0, fail_g, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);
    let inner_g = asm.global("inner_g", ty::FUNCTION, &[inner.index]);
    let outer = asm.function("outer", &[], 0);
    asm.emit(Op::Call0, inner_g, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);
    let _ = (outer, saved);

    let mut b = Builtins::empty(Numbering::None);
    b.set_numbered(1, "fail", b_fail);
    let mut vm = vm_with(&asm, b);
    let mut host = TestHost::default();
    let err = vm.call(&mut host, func(&vm, "outer"), &[]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Builtin("boom".into()));
    let names: Vec<_> = err.backtrace().0.iter().map(|f| f.name.clone()).collect();
    assert_eq!(names, [(*b"inner").into(), (*b"outer").into()]);
    // Locals were restored: inner's local slot holds its pre-call value (0).
    let inner_local: f32 = f32::from_bits(vm.program().initial_global(inner.local(0)).unwrap());
    assert_eq!(inner_local, 0.0);

    // In developer mode the builtin error is a warning and the call completes.
    vm.set_developer(true);
    vm.call(&mut host, func(&vm, "outer"), &[]).unwrap();
    assert!(host.warnings.iter().any(|w| w.to_string().contains("boom")));
}

#[test]
fn runaway_and_call_depth() {
    let mut asm = Asm::new();
    asm.function("spin", &[], 0);
    let l = asm.emit(Op::Goto, 0, 0, 0);
    asm.patch_jump(l, 0, l);
    let deep_g = asm.global("deep_g", ty::FUNCTION, &[0]);
    let deep = asm.function("deep", &[], 0);
    asm.emit(Op::Call0, deep_g, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);
    asm.set_global(deep_g, deep.index);

    let program = Arc::new(Program::parse(&asm.build(ProgsFormat::V6)).unwrap());
    let mut config = VmConfig::default();
    config.limits.runaway = 5000;
    let mut vm: Vm<TestHost> =
        Vm::new(program, Arc::new(Builtins::empty(Numbering::None)), config).unwrap();
    let mut host = TestHost::default();
    let err = vm.call(&mut host, func(&vm, "spin"), &[]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Runaway);
    let err = vm.call(&mut host, func(&vm, "deep"), &[]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::CallDepth);
    assert_eq!(err.backtrace().0.len(), 1024);
    // Still usable after unwinding 1024 frames.
    let err = vm.call(&mut host, func(&vm, "spin"), &[]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Runaway);
}

#[test]
fn null_and_invalid_calls() {
    let mut asm = Asm::new();
    let bogus = asm.int(0x0001_2345);
    asm.function("null_call", &[], 0);
    asm.emit(Op::Call0, 0, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);
    asm.function("bad_call", &[], 0);
    asm.emit(Op::Call0, bogus, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);
    let mut vm = vm(&asm);
    let mut host = TestHost::default();
    let e = vm.call(&mut host, func(&vm, "null_call"), &[]).unwrap_err();
    assert_eq!(*e.kind(), ErrorKind::NullFunction);
    let e = vm.call(&mut host, func(&vm, "bad_call"), &[]).unwrap_err();
    assert_eq!(*e.kind(), ErrorKind::InvalidFunction(FuncRef(0x0001_2345)));
    let e = vm.call(&mut host, FuncRef(0x0500_0001), &[]).unwrap_err();
    assert!(matches!(e.kind(), ErrorKind::InvalidFunction(_)));
}

fn b_spawn(vm: &mut Vm<TestHost>, _host: &mut TestHost) -> Result<(), VmError> {
    let e = vm.spawn()?;
    vm.ret_ent(e);
    Ok(())
}

#[test]
fn entities_and_fields() {
    let mut asm = Asm::new();
    let spawn = asm.builtin("spawn", 14, 0);
    let spawn_g = asm.global("spawn_g", ty::FUNCTION, &[spawn]);
    let (_, health) = asm.field("health", ty::FLOAT);
    let (_, origin) = asm.field("origin", ty::VECTOR);
    let hundred = asm.float(100.0);
    let v = asm.vector([1.0, 2.0, 3.0]);
    let f = asm.function("make", &[], 3);
    let (e, p, out) = (f.local(0), f.local(1), f.local(2));
    asm.emit(Op::Call0, spawn_g, 0, 0);
    asm.emit(Op::StoreEnt, OFS_RETURN, e, 0);
    asm.emit(Op::Address, e, health, p);
    asm.emit(Op::StorePF, hundred, p, 0);
    asm.emit(Op::StoreFieldV, e, origin, v);
    asm.emit(Op::LoadF, e, health, out);
    asm.emit(Op::AddF, out, out, out);
    asm.emit(Op::Address, e, health, p);
    asm.emit(Op::StorePF, out, p, 0);
    asm.emit(Op::Return, e, 0, 0);

    let mut b = Builtins::empty(Numbering::None);
    b.set_numbered(14, "spawn", b_spawn);
    let mut vm = vm_with(&asm, b);
    let mut host = TestHost::default();
    let e = vm.call(&mut host, func(&vm, "make"), &[]).unwrap().ent();
    assert_eq!(e, EntRef(1));
    let health = vm.field::<f32>("health").unwrap();
    let origin = vm.field::<[f32; 3]>("origin").unwrap();
    assert_eq!(vm.get_field(e, health), Some(200.0));
    assert_eq!(vm.get_field(e, origin), Some([1.0, 2.0, 3.0]));
    assert!(vm.is_in_use(e));

    // Protected entities: ADDRESS yields the sentinel and the store is skipped with a warning.
    vm.set_protected(e, true);
    vm.call(&mut host, func(&vm, "make"), &[]).unwrap();
    assert_eq!(vm.get_field(e, health), Some(200.0));
    vm.set_protected(e, false);

    // Removing zeroes only the configured fields; the slot is reused after half a second.
    vm.set_realtime(10.0);
    vm.remove(e, false);
    assert!(!vm.is_in_use(e));
    assert_eq!(vm.get_field(e, health), Some(200.0), "freed entities stay readable");
    vm.set_realtime(10.2);
    assert_eq!(vm.spawn().unwrap(), EntRef(3));
    vm.set_realtime(10.6);
    let reused = vm.spawn().unwrap();
    assert_eq!(reused, e);
    assert_eq!(vm.get_field(reused, health), Some(0.0), "spawn zeroes fields");
}

/// `Limits::max_edicts` counts the world, and every slot up to it can be spawned;
/// `VmConfig::first_spawnable` reserves the slots below it, whether the table grows or is reused.
#[test]
fn entity_limits_and_reserved_slots() {
    let asm = {
        let mut asm = Asm::new();
        asm.field("health", ty::FLOAT);
        asm.function("noop", &[], 0);
        asm.emit(Op::Done, 0, 0, 0);
        asm
    };
    let make = |max_edicts: u32, first_spawnable: u32| -> Vm<TestHost> {
        let mut config = qcvm::VmConfig::ssqc();
        config.limits.max_edicts = max_edicts;
        config.first_spawnable = first_spawnable;
        let program = std::sync::Arc::new(
            qcvm::Program::parse(&asm.build(qcvm::ProgsFormat::Fte16)).unwrap(),
        );
        Vm::new(program, std::sync::Arc::new(Builtins::empty(Numbering::None)), config).unwrap()
    };
    let no_free = |r: Result<EntRef, VmError>| *r.unwrap_err().kind() == ErrorKind::NoFreeEdicts;

    // Only the world.
    let mut vm = make(1, 0);
    assert!(no_free(vm.spawn()));
    // The world and one entity.
    let mut vm = make(2, 0);
    assert_eq!(vm.spawn().unwrap(), EntRef(1));
    assert!(no_free(vm.spawn()));
    // Every slot below the limit.
    let mut vm = make(64, 0);
    for e in 1..64 {
        assert_eq!(vm.spawn().unwrap(), EntRef(e));
    }
    assert!(no_free(vm.spawn()));

    // Slots 1..5 are reserved (SSQC's clients): a fresh table grows past them.
    let mut vm = make(7, 5);
    assert_eq!(vm.spawn().unwrap(), EntRef(5));
    assert_eq!(vm.spawn().unwrap(), EntRef(6));
    assert!(no_free(vm.spawn()));
    assert!(!vm.is_in_use(EntRef(1)), "reserved slots stay unallocated");
    // Reuse honours the reservation too.
    vm.set_realtime(10.0);
    vm.remove(EntRef(5), false);
    vm.set_realtime(11.0);
    assert_eq!(vm.spawn().unwrap(), EntRef(5));
    // A reservation at or beyond the limit leaves nothing to spawn.
    let mut vm = make(4, 4);
    assert!(no_free(vm.spawn()));
}

/// Fields added by `ensure_field` get the value type's own definition type, so they can be
/// looked up (and ensured again) with the same type.
#[test]
fn ensure_field_keeps_the_value_type() {
    fn check<T: qcvm::QcValue + PartialEq + std::fmt::Debug>(vm: &mut Vm<TestHost>, value: T) {
        let name = format!("added_{}", std::any::type_name::<T>().replace("::", "_"));
        let f = vm.ensure_field::<T>(&name).unwrap();
        assert_eq!(vm.field::<T>(&name).unwrap().offset(), f.offset(), "{name}");
        assert_eq!(vm.ensure_field::<T>(&name).unwrap().offset(), f.offset(), "{name}");
        let e = vm.spawn().unwrap();
        vm.set_field(e, f, value);
        assert_eq!(vm.get_field(e, f), Some(value), "{name}");
    }
    let mut asm = Asm::new();
    asm.function("noop", &[], 0);
    asm.emit(Op::Done, 0, 0, 0);
    let mut vm = vm(&asm);
    check(&mut vm, 1.5f32);
    check(&mut vm, -7i32);
    check(&mut vm, 7u32);
    check(&mut vm, [1.0f32, 2.0, 3.0]);
    check(&mut vm, EntRef(3));
    check(&mut vm, qcvm::StrRef(1));
    check(&mut vm, FuncRef(1));
    check(&mut vm, qcvm::Ptr(64));
    check(&mut vm, qcvm::FieldOfs(2));
    check(&mut vm, -5i64);
    check(&mut vm, 5u64);
    check(&mut vm, 2.25f64);
    // A field of another type under the same name is refused.
    assert!(matches!(
        vm.ensure_field::<f32>("added_qcvm_value_FuncRef"),
        Err(qcvm::LookupError::WrongType(_))
    ));
}

/// Spawn serials tell reused slots apart; hosts may pass at most 8 arguments; `call_as_with`
/// takes a resolved `self`.
#[test]
fn serials_argument_limits_and_resolved_self() {
    let mut asm = Asm::new();
    let self_g = asm.global("self", ty::ENTITY, &[]);
    asm.field("health", ty::FLOAT);
    asm.function("whoami", &[], 0);
    asm.emit(Op::Return, self_g, 0, 0);
    let mut vm = vm(&asm);
    let mut host = TestHost::default();

    let e = vm.spawn().unwrap();
    let first = vm.serial(e).unwrap();
    vm.set_realtime(10.0);
    vm.remove(e, true);
    assert_eq!(vm.serial(e), Some(first), "a free slot keeps its serial");
    let again = vm.spawn().unwrap();
    assert_eq!(again, e);
    assert_eq!(vm.serial(again), Some(first + 1));
    assert_eq!(vm.serial(EntRef(1_000_000)), None);

    let whoami = func(&vm, "whoami");
    let nine = [Arg::Float(0.0); 9];
    let err = vm.call(&mut host, whoami, &nine).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::TooManyArguments(9));
    vm.call(&mut host, whoami, &nine[..8]).unwrap();

    let self_ref = vm.global::<EntRef>("self").unwrap();
    let r = vm.call_as_with(&mut host, self_ref, again, whoami, &[]).unwrap();
    assert_eq!(r.ent(), again);
    assert_eq!(vm.get(self_ref), EntRef(0), "self is restored");
}

/// Only the unbound builtins the code calls count as reachable.
#[test]
fn reachable_unbound_builtins() {
    let mut asm = Asm::new();
    let called = asm.builtin("called", 7, 0);
    let bound = asm.builtin("bound", 8, 0);
    asm.builtin("declared", 9, 0);
    let called_g = asm.global("called_g", ty::FUNCTION, &[called]);
    let bound_g = asm.global("bound_g", ty::FUNCTION, &[bound]);
    asm.function("main", &[], 0);
    asm.emit(Op::Call0, called_g, 0, 0);
    asm.emit(Op::Call0, bound_g, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);
    let mut b = Builtins::empty(Numbering::None);
    b.set_numbered(8, "bound", b_log);
    let vm = vm_with(&asm, b);
    let names = |v: Vec<qcvm::vm::UnboundBuiltin>| -> Vec<Vec<u8>> {
        v.into_iter().map(|u| u.name.to_vec()).collect()
    };
    assert_eq!(names(vm.unbound_builtins()), [b"called".to_vec(), b"declared".to_vec()]);
    assert_eq!(names(vm.reachable_unbound_builtins()), [b"called".to_vec()]);
}

#[test]
fn bad_entity_access_warns_and_reads_zero() {
    let mut asm = Asm::new();
    let (_, health) = asm.field("health", ty::FLOAT);
    let big = asm.int(9999);
    let f = asm.function("peek", &[], 1);
    asm.emit(Op::LoadF, big, health, f.local(0));
    asm.emit(Op::Return, f.local(0), 0, 0);
    let mut vm = vm(&asm);
    let mut host = TestHost::default();
    let r = vm.call(&mut host, func(&vm, "peek"), &[]).unwrap();
    assert_eq!(r.0[0], 0);
    assert_eq!(host.warnings.len(), 1);
    assert_eq!(host.warnings[0].kind, qcvm::WarningKind::BadEntity(9999));
    assert_eq!(&*host.warnings[0].backtrace.0[0].name, b"peek");
}

fn b_concat(vm: &mut Vm<TestHost>, _host: &mut TestHost) -> Result<(), VmError> {
    let mut out = Vec::new();
    for i in 0..vm.argc() {
        out.extend_from_slice(vm.arg_str(i));
    }
    vm.ret_str(&out)
}

#[test]
fn temp_strings_compare_and_collect() {
    let mut asm = Asm::new();
    let cat = asm.builtin("strcat", 115, 8);
    let cat_g = asm.global("cat_g", ty::FUNCTION, &[cat]);
    let hello = asm.str_const("hello");
    let world = asm.str_const(" world");
    let hw = asm.str_const("hello world");
    let kept = asm.global("kept", ty::STRING, &[]);
    let f = asm.function("go", &[], 1);
    asm.emit(Op::StoreS, hello, parm(0), 0);
    asm.emit(Op::StoreS, world, parm(1), 0);
    asm.emit(Op::Call2, cat_g, 0, 0);
    asm.emit(Op::StoreS, OFS_RETURN, kept, 0);
    asm.emit(Op::EqS, kept, hw, f.local(0));
    asm.emit(Op::Return, f.local(0), 0, 0);
    // Makes a temp string that nothing keeps.
    asm.function("garbage", &[], 0);
    asm.emit(Op::StoreS, hello, parm(0), 0);
    asm.emit(Op::Call1, cat_g, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);

    let mut b = Builtins::empty(Numbering::None);
    b.set_numbered(115, "strcat", b_concat);
    let mut vm = vm_with(&asm, b);
    let mut host = TestHost::default();
    let r = vm.call(&mut host, func(&vm, "go"), &[]).unwrap();
    assert_eq!(r.f32(), 1.0);
    let kept = vm.global::<StrRef>("kept").unwrap();
    assert!(vm.is_temp(vm.get(kept)));
    assert_eq!(vm.str(vm.get(kept)), b"hello world");

    for _ in 0..3000 {
        vm.call(&mut host, func(&vm, "garbage"), &[]).unwrap();
    }
    // Collections ran along the way; the referenced string survived.
    assert!(vm.temp_strings() < 3000, "{}", vm.temp_strings());
    let stats = vm.collect_garbage().unwrap();
    assert_eq!(stats.live, 1);
    assert_eq!(vm.str(vm.get(kept)), b"hello world");
}

#[test]
fn state_opcode_default_behaviour() {
    let mut asm = Asm::new();
    let self_g = asm.global("self", ty::ENTITY, &[]);
    let time_g = asm.global("time", ty::FLOAT, &[]);
    let _ = time_g;
    asm.field("frame", ty::FLOAT);
    asm.field("think", ty::FUNCTION);
    asm.field("nextthink", ty::FLOAT);
    let five = asm.float(5.0);
    let think_fn = asm.function("thinker", &[], 0);
    asm.emit(Op::Done, 0, 0, 0);
    let think_g = asm.global("thinker_g", ty::FUNCTION, &[think_fn.index]);
    asm.function("animate", &[], 0);
    asm.emit(Op::State, five, think_g, 0);
    asm.emit(Op::Done, 0, 0, 0);
    let _ = self_g;

    let mut vm = vm(&asm);
    let mut host = TestHost::default();
    let e = vm.spawn().unwrap();
    let self_h = vm.global::<EntRef>("self").unwrap();
    let time_h = vm.global::<f32>("time").unwrap();
    vm.set(self_h, e);
    vm.set(time_h, 10.0);
    vm.call(&mut host, func(&vm, "animate"), &[]).unwrap();
    assert_eq!(vm.get_field(e, vm.field::<f32>("frame").unwrap()), Some(5.0));
    assert_eq!(vm.get_field(e, vm.field::<f32>("nextthink").unwrap()), Some(10.1));
    assert_eq!(
        vm.get_field(e, vm.field::<FuncRef>("think").unwrap()),
        Some(FuncRef(think_fn.index))
    );
}

#[test]
fn reentrancy_limit() {
    let mut asm = Asm::new();
    let cb = asm.builtin("callback", 1, 1);
    let cb_g = asm.global("cb_g", ty::FUNCTION, &[cb]);
    let f = asm.function("recurse", &[1], 0);
    asm.emit(Op::StoreF, f.local(0), parm(0), 0);
    asm.emit(Op::Call1, cb_g, 0, 0);
    asm.emit(Op::Return, OFS_RETURN, 0, 0);
    let _ = OFS_PARM0;
    let mut b = Builtins::empty(Numbering::None);
    b.set_numbered(1, "callback", b_callback);
    let mut vm = vm_with(&asm, b);
    let mut host = TestHost { callback: Some(func(&vm, "recurse")), ..TestHost::default() };
    let e = vm.call(&mut host, func(&vm, "recurse"), &[Arg::Float(0.0)]).unwrap_err();
    assert_eq!(*e.kind(), ErrorKind::Reentrancy);
    // Fully unwound: a normal call works afterwards.
    host.callback = Some(func(&vm, "recurse"));
    assert!(vm.call(&mut host, func(&vm, "recurse"), &[Arg::Float(0.0)]).is_err());
    assert!(vm.backtrace().0.is_empty());
}

fn b_abort(vm: &mut Vm<TestHost>, _host: &mut TestHost) -> Result<(), VmError> {
    let v = vm.arg_f32(0);
    Err(VmError::abort([v.to_bits(), 0, 0]))
}

#[test]
fn abort_unwinds_to_the_engine_boundary() {
    let mut asm = Asm::new();
    let ab = asm.builtin("abort", 211, 1);
    let ab_g = asm.global("ab_g", ty::FUNCTION, &[ab]);
    let (forty2, seven) = (asm.float(42.0), asm.float(7.0));
    let inner = asm.function("inner", &[], 1);
    asm.emit(Op::StoreF, seven, inner.local(0), 0);
    asm.emit(Op::StoreF, forty2, parm(0), 0);
    asm.emit(Op::Call1, ab_g, 0, 0);
    asm.emit(Op::Return, seven, 0, 0);
    let inner_g = asm.global("inner_g", ty::FUNCTION, &[inner.index]);
    let outer = asm.function("outer", &[], 0);
    asm.emit(Op::Call0, inner_g, 0, 0);
    asm.emit(Op::Return, seven, 0, 0);
    let _ = outer;
    let mut b = Builtins::empty(Numbering::None);
    b.set_numbered(211, "abort", b_abort);
    let mut vm = vm_with(&asm, b);
    let mut host = TestHost::default();
    let r = vm.call(&mut host, func(&vm, "outer"), &[]).unwrap();
    assert_eq!(r.f32(), 42.0);
    assert!(vm.backtrace().0.is_empty());
    // The inner function's local was restored by the unwind.
    assert_eq!(vm.call(&mut host, func(&vm, "outer"), &[]).unwrap().f32(), 42.0);
}

#[test]
fn csqc_spawn_defaults_fill_the_dimension_fields() {
    let mut asm = Asm::new();
    asm.field("dimension_solid", ty::FLOAT);
    asm.field("dimension_hit", ty::FLOAT);
    asm.function("noop", &[], 0);
    asm.emit(Op::Done, 0, 0, 0);
    let mut m = vm(&asm);
    let solid = m.field::<f32>("dimension_solid").unwrap();
    let hit = m.field::<f32>("dimension_hit").unwrap();
    assert_eq!(m.get_field(EntRef(0), solid), Some(255.0), "the world is spawned too");
    let e = m.spawn().unwrap();
    assert_eq!((m.get_field(e, solid), m.get_field(e, hit)), (Some(255.0), Some(255.0)));

    // With a `dimension_default` global, its value at spawn time is used instead.
    let mut asm = Asm::new();
    asm.field("dimension_solid", ty::FLOAT);
    asm.global("dimension_default", ty::FLOAT, &[7.0f32.to_bits()]);
    let mut m = vm(&asm);
    let solid = m.field::<f32>("dimension_solid").unwrap();
    let g = m.global::<f32>("dimension_default").unwrap();
    let e = m.spawn().unwrap();
    assert_eq!(m.get_field(e, solid), Some(7.0));
    m.set(g, 3.0);
    let e = m.spawn().unwrap();
    assert_eq!(m.get_field(e, solid), Some(3.0));

    // Other presets leave spawned entities all zero.
    let mut vm: Vm<TestHost> = Vm::new(
        std::sync::Arc::new(qcvm::Program::parse(&asm.build(qcvm::ProgsFormat::Fte16)).unwrap()),
        std::sync::Arc::new(Builtins::empty(Numbering::None)),
        qcvm::VmConfig::ssqc(),
    )
    .unwrap();
    let solid = vm.field::<f32>("dimension_solid").unwrap();
    let e = vm.spawn().unwrap();
    assert_eq!(vm.get_field(e, solid), Some(0.0));
}

/// The re-entrancy cap stops runaway builtin recursion before it exhausts a small native stack:
/// each level costs about 1 KiB in optimised builds. Unoptimised, the interpreter's own frame is
/// around 100 KiB, so debug builds get a larger stack.
#[test]
fn reentrancy_limit_fits_a_small_thread_stack() {
    let run = || {
        let mut asm = Asm::new();
        let cb = asm.builtin("callback", 1, 1);
        let cb_g = asm.global("cb_g", ty::FUNCTION, &[cb]);
        let f = asm.function("recurse", &[1], 0);
        asm.emit(Op::StoreF, f.local(0), parm(0), 0);
        asm.emit(Op::Call1, cb_g, 0, 0);
        asm.emit(Op::Return, OFS_RETURN, 0, 0);
        let mut b = Builtins::empty(Numbering::None);
        b.set_numbered(1, "callback", b_callback);
        let mut vm = vm_with(&asm, b);
        let mut host = TestHost { callback: Some(func(&vm, "recurse")), ..TestHost::default() };
        let e = vm.call(&mut host, func(&vm, "recurse"), &[Arg::Float(0.0)]).unwrap_err();
        assert_eq!(*e.kind(), ErrorKind::Reentrancy);
    };
    let stack = if cfg!(debug_assertions) { 1024 } else { 256 } * 1024;
    std::thread::Builder::new().stack_size(stack).spawn(run).unwrap().join().unwrap();
}

#[derive(Default)]
struct Tracer {
    lines: Vec<String>,
}

impl qcvm::Host for Tracer {
    fn trace(&mut self, line: &str) {
        self.lines.push(line.to_owned());
    }
}

/// `traceon`/`traceoff` report every statement in between to `Host::trace`, including those of
/// called functions.
#[test]
fn traceon_reports_statements() {
    let mut asm = Asm::new();
    let on = asm.builtin("traceon", 29, 0);
    let off = asm.builtin("traceoff", 30, 0);
    let (on_g, off_g) =
        (asm.global("on_g", ty::FUNCTION, &[on]), asm.global("off_g", ty::FUNCTION, &[off]));
    let (one, two) = (asm.float(1.0), asm.float(2.0));
    let helper = asm.function("helper", &[], 1);
    asm.emit(Op::MulF, two, two, helper.local(0));
    asm.emit(Op::Return, helper.local(0), 0, 0);
    let helper_g = asm.global("helper_g", ty::FUNCTION, &[helper.index]);
    let f = asm.function("main", &[], 1);
    asm.emit(Op::AddF, one, one, f.local(0));
    asm.emit(Op::Call0, on_g, 0, 0);
    asm.emit(Op::AddF, one, two, f.local(0));
    asm.emit(Op::Call0, helper_g, 0, 0);
    asm.emit(Op::Call0, off_g, 0, 0);
    asm.emit(Op::SubF, one, two, f.local(0));
    asm.emit(Op::Done, 0, 0, 0);
    let program =
        std::sync::Arc::new(qcvm::Program::parse(&asm.build(qcvm::ProgsFormat::Fte16)).unwrap());
    let mut vm: Vm<Tracer> = Vm::new(
        program,
        std::sync::Arc::new(Builtins::standard(Numbering::Csqc)),
        qcvm::VmConfig::default(),
    )
    .unwrap();
    let mut host = Tracer::default();
    let main = vm.find_function("main").unwrap();
    vm.call(&mut host, main, &[]).unwrap();
    let ops: Vec<String> = host
        .lines
        .iter()
        .map(|l| {
            let (func, rest) = l.split_once(": ").unwrap();
            let op = rest.split_whitespace().nth(1).unwrap();
            format!("{func} {op}")
        })
        .collect();
    assert_eq!(
        ops,
        ["main ADD_F", "main CALL0", "helper MUL_F", "helper RETURN", "main CALL0"],
        "{:#?}",
        host.lines
    );
    assert!(!vm.is_tracing());
    vm.set_trace(true);
    vm.call(&mut host, main, &[]).unwrap();
    assert!(host.lines.len() > 5 && host.lines[5].starts_with("main:"), "{:#?}", host.lines);
}

fn b_panic(_vm: &mut Vm<TestHost>, _host: &mut TestHost) -> Result<(), VmError> {
    panic!("host builtin bug");
}

/// A panic in a host builtin propagates unchanged, and leaves the VM refusing to run QuakeC
/// until it is reset.
#[test]
fn a_panicking_builtin_poisons_the_vm() {
    let mut asm = Asm::new();
    let p = asm.builtin("boom", 1, 0);
    let p_g = asm.global("boom_g", ty::FUNCTION, &[p]);
    asm.function("main", &[], 0);
    asm.emit(Op::Call0, p_g, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);
    asm.function("fine", &[], 0);
    asm.emit(Op::Done, 0, 0, 0);
    let mut b = Builtins::empty(Numbering::None);
    b.set_numbered(1, "boom", b_panic);
    let mut vm = vm_with(&asm, b);
    let mut host = TestHost::default();
    let main = func(&vm, "main");
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = vm.call(&mut host, main, &[]);
    }));
    let msg = caught.unwrap_err();
    assert_eq!(msg.downcast_ref::<&str>(), Some(&"host builtin bug"));
    let fine = func(&vm, "fine");
    assert_eq!(*vm.call(&mut host, fine, &[]).unwrap_err().kind(), ErrorKind::Poisoned);
    assert_eq!(*vm.run_threads(&mut host).unwrap_err().kind(), ErrorKind::Poisoned);
    vm.reset().unwrap();
    vm.call(&mut host, fine, &[]).unwrap();
}

/// Vector copies through pointers whose source and destination overlap behave as if copied
/// through a temporary (FTE's result depends on its C compiler; see docs/spec/deviations.md).
#[test]
fn overlapping_vector_pointer_copies() {
    let mut asm = Asm::new();
    let block = asm.alloc(4, &[1.0f32, 2.0, 3.0, 4.0].map(f32::to_bits));
    let (zero, one) = (asm.int(0), asm.int(1));
    let f = asm.function("store", &[], 1);
    // STOREP_V: the first three words onto the last three.
    asm.emit(Op::GlobalAddress, block, one, f.local(0));
    asm.emit(Op::StorePV, block, f.local(0), 0);
    asm.emit(Op::Done, 0, 0, 0);
    let g = asm.function("load", &[], 1);
    // LOADP_V: read through a pointer to the first word, writing from the second.
    asm.emit(Op::GlobalAddress, block, zero, g.local(0));
    asm.emit(Op::LoadPV, g.local(0), zero, block + 1);
    asm.emit(Op::Done, 0, 0, 0);
    asm.def_global("block", ty::FLOAT, block);
    for (name, format) in [("store", qcvm::ProgsFormat::Fte16), ("load", qcvm::ProgsFormat::Fte32)]
    {
        let program = std::sync::Arc::new(qcvm::Program::parse(&asm.build(format)).unwrap());
        let mut vm: Vm<TestHost> = Vm::new(
            program,
            std::sync::Arc::new(Builtins::empty(Numbering::None)),
            qcvm::VmConfig::default(),
        )
        .unwrap();
        let mut host = TestHost::default();
        let f = vm.find_function(name).unwrap();
        vm.call(&mut host, f, &[]).unwrap();
        let base = vm.global::<f32>("block").unwrap().ptr().0;
        let words: Vec<f32> = (0..4)
            .map(|i| {
                let b = vm.read_mem(qcvm::Ptr(base + 4 * i), 4).unwrap();
                f32::from_le_bytes(b.try_into().unwrap())
            })
            .collect();
        assert_eq!(words, [1.0, 1.0, 2.0, 3.0], "{name}");
    }
}

/// A builtin that runs the host's callback function (a QuakeC loop) once.
fn b_run_callback(vm: &mut Vm<TestHost>, host: &mut TestHost) -> Result<(), VmError> {
    let f = host.callback.unwrap();
    vm.call(host, f, &[])?;
    Ok(())
}

/// A function of `gotos` jumps to the next statement and a `DONE`: `gotos + 1` counted
/// instructions.
fn straight_line(asm: &mut Asm, name: &str, gotos: u32) {
    asm.function(name, &[], 0);
    for _ in 0..gotos {
        let at = asm.emit(Op::Goto, 0, 0, 0);
        asm.patch_jump(at, 0, at + 1);
    }
    asm.emit(Op::Done, 0, 0, 0);
}

/// The runaway budget is exact across the chunks the interpreter runs in, and calls builtins make
/// back into QuakeC share their host call's budget.
#[test]
fn runaway_budget_is_exact_and_shared() {
    let mut asm = Asm::new();
    straight_line(&mut asm, "fits", 69_998);
    straight_line(&mut asm, "too_long", 69_999);
    straight_line(&mut asm, "work", 999);
    let b = asm.builtin("run_callback", 1, 0);
    let b_g = asm.global("run_callback_g", ty::FUNCTION, &[b]);
    asm.function("many", &[], 0);
    for _ in 0..10 {
        asm.emit(Op::Call0, b_g, 0, 0);
    }
    asm.emit(Op::Done, 0, 0, 0);
    let program = Arc::new(Program::parse(&asm.build(ProgsFormat::Fte32)).unwrap());
    let mut config = VmConfig::default();
    config.limits.runaway = 70_000;
    let mut b = Builtins::empty(Numbering::None);
    b.set_numbered(1, "run_callback", b_run_callback);
    let mut vm: Vm<TestHost> = Vm::new(program, Arc::new(b), config).unwrap();
    let mut host = TestHost::default();
    // 69,999 counted instructions fit a budget of 70,000; the 70,000th is refused. With a
    // deadline the budget is handed out in chunks of 65,536, and stays exact across them.
    vm.call(&mut host, func(&vm, "fits"), &[]).unwrap();
    let err = vm.call(&mut host, func(&vm, "too_long"), &[]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Runaway);
    let mut chunked = vm.config().clone();
    chunked.limits.deadline = Some(std::time::Duration::from_secs(3600));
    let mut b = Builtins::empty(Numbering::None);
    b.set_numbered(1, "run_callback", b_run_callback);
    let mut cvm: Vm<TestHost> = Vm::new(Arc::clone(vm.program()), Arc::new(b), chunked).unwrap();
    cvm.call(&mut host, func(&cvm, "fits"), &[]).unwrap();
    let err = cvm.call(&mut host, func(&cvm, "too_long"), &[]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Runaway);

    // Ten nested runs of 1,000 each exceed a budget of 5,000 between them.
    host.callback = Some(func(&vm, "work"));
    vm.call(&mut host, func(&vm, "many"), &[]).unwrap();
    let mut config = vm.config().clone();
    config.limits.runaway = 5_000;
    let program = Arc::clone(vm.program());
    let mut b = Builtins::empty(Numbering::None);
    b.set_numbered(1, "run_callback", b_run_callback);
    let mut vm: Vm<TestHost> = Vm::new(program, Arc::new(b), config).unwrap();
    host.callback = Some(func(&vm, "work"));
    let err = vm.call(&mut host, func(&vm, "many"), &[]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Runaway);
    // The next host call starts with a full budget again.
    vm.call(&mut host, func(&vm, "work"), &[]).unwrap();
}

/// A deadline stops a call that would otherwise run for a long time.
#[test]
fn deadline_stops_long_calls() {
    let mut asm = Asm::new();
    asm.function("spin", &[], 0);
    let l = asm.emit(Op::Goto, 0, 0, 0);
    asm.patch_jump(l, 0, l);
    let program = Arc::new(Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap());
    let mut config = VmConfig::default();
    config.limits.runaway = u32::MAX;
    config.limits.deadline = Some(std::time::Duration::from_millis(50));
    let mut vm: Vm<TestHost> =
        Vm::new(program, Arc::new(Builtins::empty(Numbering::None)), config).unwrap();
    let mut host = TestHost::default();
    let started = std::time::Instant::now();
    let err = vm.call(&mut host, func(&vm, "spin"), &[]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Deadline);
    let took = started.elapsed();
    assert!(
        took >= std::time::Duration::from_millis(50) && took < std::time::Duration::from_secs(5),
        "{took:?}"
    );
}
