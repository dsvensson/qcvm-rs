// SPDX-License-Identifier: MIT OR Apache-2.0

//! Several progs in one VM: linking, cross-progs calls, shared globals, field unification and
//! string relocation.

use std::sync::Arc;

use qcvm::{
    Arg, Builtins, EntRef, ErrorKind, FuncRef, Numbering, Op, PrNum, Program, ProgsFormat, StrRef,
    Vm, VmConfig,
};

use crate::support::asm::{Asm, OFS_RETURN, parm, ty};
use crate::vm_basic::TestHost;

/// The main progs: `self`, fields `health` and `origin`, an extern `addon_twice(x)` and
/// `run(x) = addon_twice(x) + 1`.
fn main_progs() -> Asm {
    let mut asm = Asm::new();
    asm.global("self", ty::ENTITY, &[]);
    asm.field("health", ty::FLOAT);
    asm.field("origin", ty::VECTOR);
    let ext = asm.global("addon_twice", ty::FUNCTION, &[0]);
    asm.bodyless("addon_twice");
    let one = asm.float(1.0);
    let f = asm.function("run", &[1], 0);
    asm.emit(Op::StoreF, f.local(0), parm(0), 0);
    asm.emit(Op::Call1, ext, 0, 0);
    asm.emit(Op::AddF, OFS_RETURN, one, OFS_RETURN);
    asm.emit(Op::Return, OFS_RETURN, 0, 0);
    asm
}

/// An add-on: `addon_twice(x) = x * 2 + self.health + self.mana`, a new field `mana`, a string
/// constant, `thisprogs`, and an `init` that records its argument.
fn addon_progs() -> Asm {
    let mut asm = Asm::new();
    asm.string("padding so offsets differ from the main progs");
    let self_g = asm.global("self", ty::ENTITY, &[]);
    let (_, mana) = asm.field("mana", ty::FLOAT);
    let (_, health) = asm.field("health", ty::FLOAT);
    asm.global("thisprogs", ty::FLOAT, &[]);
    let greeting = asm.string("hello from the addon");
    asm.global("greeting", ty::STRING, &[greeting]);
    let init_arg = asm.global("init_arg", ty::FLOAT, &[]);
    let two = asm.float(2.0);
    let f = asm.function("addon_twice", &[1], 2);
    let (x, t) = (f.local(0), f.local(1));
    asm.emit(Op::MulF, x, two, t);
    asm.emit(Op::LoadF, self_g, health, f.local(2));
    asm.emit(Op::AddF, t, f.local(2), t);
    asm.emit(Op::LoadF, self_g, mana, f.local(2));
    asm.emit(Op::AddF, t, f.local(2), t);
    asm.emit(Op::Return, t, 0, 0);
    let init = asm.function("init", &[1], 0);
    asm.emit(Op::StoreF, init.local(0), init_arg, 0);
    asm.emit(Op::Done, 0, 0, 0);
    asm
}

fn load(asm: &Asm) -> Arc<Program> {
    Arc::new(Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap())
}

fn vm() -> Vm<TestHost> {
    Vm::new(load(&main_progs()), Arc::new(Builtins::empty(Numbering::None)), VmConfig::default())
        .unwrap()
}

/// Host lookups see what QuakeC has done to function globals: redirected entry points, cleared
/// ones, globals without a function of their own, and references into other progs.
#[test]
fn function_lookup_reads_live_function_globals() {
    let mut asm = Asm::new();
    let (one, two) = (asm.float(1.0), asm.float(2.0));
    let first = asm.function("first", &[], 0);
    asm.emit(Op::Return, one, 0, 0);
    let second = asm.function("second", &[], 0);
    asm.emit(Op::Return, two, 0, 0);
    asm.function("gone", &[], 0);
    asm.emit(Op::Done, 0, 0, 0);
    asm.global("hook", ty::FUNCTION, &[second.index]);
    let program = load(&asm);
    let mut vm: Vm<TestHost> = Vm::new(
        Arc::clone(&program),
        Arc::new(Builtins::empty(Numbering::None)),
        VmConfig::default(),
    )
    .unwrap();
    let mut host = TestHost::default();
    let mut run = |vm: &mut Vm<TestHost>, name: &str| {
        let f = vm.find_function(name).unwrap();
        vm.call(&mut host, f, &[]).unwrap().f32()
    };
    assert_eq!(run(&mut vm, "first"), 1.0);
    assert_eq!(run(&mut vm, "hook"), 2.0, "a function global without a function of its own");

    // Redirect `first` to `second`, and clear `gone`.
    let first_g = vm.global::<FuncRef>("first").unwrap();
    let second_f = vm.find_function("second").unwrap();
    vm.set(first_g, second_f);
    assert_eq!(run(&mut vm, "first"), 2.0);
    let gone = vm.global::<FuncRef>("gone").unwrap();
    vm.set(gone, FuncRef::NULL);
    assert_eq!(vm.find_function("gone"), None);
    // The program itself still describes the initial state.
    assert_eq!(program.function_index("first"), Some(first.index));

    // A global of an add-on pointing into the main progs keeps its progs tag.
    let pr = vm.add_progs(&mut host, load(&addon_progs())).unwrap();
    let redirect = vm.global_in::<FuncRef>(pr, "addon_twice").unwrap();
    vm.set(redirect, second_f);
    let f = vm.find_function_in(pr, "addon_twice").unwrap();
    assert_eq!(f.progs(), PrNum(0));
    assert_eq!(vm.call(&mut host, f, &[]).unwrap().f32(), 2.0);
}

#[test]
fn cross_progs_calls_share_fields_and_globals() {
    let mut vm = vm();
    let mut host = TestHost::default();
    // Unlinked extern: calling it fails like a null function.
    let run = vm.find_function("run").unwrap();
    assert_eq!(
        *vm.call(&mut host, run, &[Arg::Float(1.0)]).unwrap_err().kind(),
        ErrorKind::NullFunction
    );

    let pr = vm.add_progs(&mut host, load(&addon_progs())).unwrap();
    assert_eq!(pr, PrNum(1));
    assert_eq!(vm.num_progs(), 2);

    // Fields: `health` is shared by name, `mana` was added.
    let health = vm.field::<f32>("health").unwrap();
    let mana = vm.field::<f32>("mana").unwrap();
    assert_ne!(health.offset(), mana.offset());
    let e = vm.spawn().unwrap();
    vm.set_field(e, health, 10.0);
    vm.set_field(e, mana, 100.0);
    let self_g = vm.global::<EntRef>("self").unwrap();
    vm.set(self_g, e);

    // run(3) = addon_twice(3) + 1 = (6 + 10 + 100) + 1; `self` travels to the add-on.
    assert_eq!(vm.call(&mut host, run, &[Arg::Float(3.0)]).unwrap().f32(), 117.0);

    // Calling into the add-on directly from the host.
    let twice = vm.find_function_in(pr, "addon_twice").unwrap();
    assert_eq!(twice.progs(), pr);
    assert_eq!(vm.call(&mut host, twice, &[Arg::Float(1.0)]).unwrap().f32(), 112.0);

    // Relocated globals of the add-on.
    let greeting = vm.global_in::<StrRef>(pr, "greeting").unwrap();
    assert_eq!(vm.str(vm.get(greeting)), b"hello from the addon");
    let thisprogs = vm.global_in::<f32>(pr, "thisprogs").unwrap();
    assert_eq!(vm.get(thisprogs), 1.0);
    let init_arg = vm.global_in::<f32>(pr, "init_arg").unwrap();
    assert_eq!(vm.get(init_arg), 0.0, "init(prevprogs) receives the previous progs number");

    // reset() drops the add-on again.
    vm.reset().unwrap();
    assert_eq!(vm.num_progs(), 1);
}

/// A progs claiming billions of field words but defining none is refused as soon as the reserved
/// space runs out, and leaves the field layout as it was.
#[test]
fn huge_field_counts_are_refused_quickly() {
    let mut vm = vm();
    let mut host = TestHost::default();
    let mut tiny = Asm::new();
    tiny.header_overrides.push((14, u32::MAX)); // entity_fields
    let program = load(&tiny);
    assert_eq!(program.entity_fields(), u32::MAX);
    let words_before = vm.field::<[f32; 3]>("origin").unwrap().offset();
    let started = std::time::Instant::now();
    let err = vm.add_progs(&mut host, program).unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(matches!(err.kind(), ErrorKind::OutOfMemory(_)));
    assert_eq!(vm.num_progs(), 1);
    assert_eq!(vm.field::<[f32; 3]>("origin").unwrap().offset(), words_before);
    // The layout still has its room: a well-formed add-on fits afterwards.
    vm.add_progs(&mut host, load(&addon_progs())).unwrap();
}

#[test]
fn field_reserve_is_enforced() {
    let config = VmConfig { field_reserve_bytes: 16, ..VmConfig::default() };
    let mut vm: Vm<TestHost> =
        Vm::new(load(&main_progs()), Arc::new(Builtins::empty(Numbering::None)), config).unwrap();
    let mut greedy = Asm::new();
    for i in 0..200 {
        greedy.field(&format!("extra{i}"), ty::VECTOR);
    }
    let mut host = TestHost::default();
    let err = vm.add_progs(&mut host, load(&greedy)).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::OutOfMemory(_)));
    assert_eq!(vm.num_progs(), 1);
    assert!(vm.field::<[f32; 3]>("extra0").is_err(), "failed add_progs leaves fields untouched");
}

/// Builtins use the globals of the progs that calls them: an add-on's `makevectors` writes the
/// add-on's view vectors (which the main progs need not even define), and `changeyaw` turns the
/// add-on's `self`.
#[test]
fn builtins_use_the_calling_progs_globals() {
    let mut main = Asm::new();
    main.global("self", ty::ENTITY, &[]);
    for (name, t) in [("angles", ty::VECTOR), ("ideal_yaw", ty::FLOAT), ("yaw_speed", ty::FLOAT)] {
        main.field(name, t);
    }
    let with_view = {
        let mut asm = main.clone();
        for v in ["v_forward", "v_right", "v_up"] {
            asm.global(v, ty::VECTOR, &[0, 0, 0]);
        }
        asm
    };
    let mut addon = Asm::new();
    let self_g = addon.global("self", ty::ENTITY, &[]);
    addon.field("angles", ty::VECTOR);
    for v in ["v_forward", "v_right", "v_up"] {
        addon.global(v, ty::VECTOR, &[0, 0, 0]);
    }
    let target = addon.global("target", ty::ENTITY, &[]);
    let mv = addon.builtin("makevectors", 1, 1);
    let mv_g = addon.global("makevectors", ty::FUNCTION, &[mv]);
    let cy = addon.builtin("changeyaw", 49, 0);
    let cy_g = addon.global("changeyaw", ty::FUNCTION, &[cy]);
    let yaw90 = addon.vector([0.0, 90.0, 0.0]);
    addon.function("look", &[], 0);
    addon.emit(Op::StoreV, yaw90, parm(0), 0);
    addon.emit(Op::Call1, mv_g, 0, 0);
    addon.emit(Op::Done, 0, 0, 0);
    addon.function("turn", &[], 0);
    addon.emit(Op::StoreEnt, target, self_g, 0);
    addon.emit(Op::Call0, cy_g, 0, 0);
    addon.emit(Op::Done, 0, 0, 0);

    for main in [main, with_view] {
        let mut vm: Vm<TestHost> = Vm::new(
            load(&main),
            Arc::new(Builtins::standard(Numbering::Csqc)),
            VmConfig::default(),
        )
        .unwrap();
        let mut host = TestHost::default();
        let pr = vm.add_progs(&mut host, load(&addon)).unwrap();
        let look = vm.find_function_in(pr, "look").unwrap();
        vm.call(&mut host, look, &[]).unwrap();
        let forward = vm.get(vm.global_in::<[f32; 3]>(pr, "v_forward").unwrap());
        assert!((forward[1] - 1.0).abs() < 1e-6 && forward[0].abs() < 1e-6, "{forward:?}");
        if let Ok(main_forward) = vm.global::<[f32; 3]>("v_forward") {
            assert_eq!(vm.get(main_forward), [0.0; 3], "the main progs' vectors are untouched");
        }

        let e = vm.spawn().unwrap();
        vm.set_field(e, vm.field::<f32>("ideal_yaw").unwrap(), 90.0);
        vm.set_field(e, vm.field::<f32>("yaw_speed").unwrap(), 45.0);
        let target = vm.global_in::<EntRef>(pr, "target").unwrap();
        vm.set(target, e);
        let turn = vm.find_function_in(pr, "turn").unwrap();
        vm.call(&mut host, turn, &[]).unwrap();
        let angles = vm.get_field(e, vm.field::<[f32; 3]>("angles").unwrap()).unwrap();
        assert!((angles[1] - 45.0).abs() < 0.01, "{angles:?}");
    }
}

/// The introspection builtins reach into other progs: `externcall`, `externvalue`, `externset`,
/// `isfunction` and `callfunction`.
#[test]
fn extern_builtins_reach_other_progs() {
    let mut main = main_progs();
    for (name, number) in [
        ("externcall", 201),
        ("externvalue", 203),
        ("externset", 204),
        ("isfunction", 607),
        ("callfunction", 605),
    ] {
        asm_builtin(&mut main, name, number);
    }
    let mut vm: Vm<TestHost> =
        Vm::new(load(&main), Arc::new(Builtins::standard(Numbering::Csqc)), VmConfig::default())
            .unwrap();
    let mut host = TestHost::default();
    let pr = vm.add_progs(&mut host, load(&addon_progs())).unwrap();
    let (health, mana) = (vm.field::<f32>("health").unwrap(), vm.field::<f32>("mana").unwrap());
    let e = vm.spawn().unwrap();
    vm.set_field(e, health, 10.0);
    vm.set_field(e, mana, 100.0);
    let self_g = vm.global::<EntRef>("self").unwrap();
    vm.set(self_g, e);
    let mut call = |vm: &mut Vm<TestHost>, name: &str, args: &[Arg<'_>]| {
        let f = vm.find_function(name).unwrap();
        vm.call(&mut host, f, args).unwrap()
    };

    // externcall by progs number and in whichever progs has the function.
    let r = call(
        &mut vm,
        "externcall",
        &[Arg::Float(1.0), Arg::Bytes(b"addon_twice"), Arg::Float(3.0)],
    );
    assert_eq!(r.f32(), 116.0);
    let r = call(
        &mut vm,
        "externcall",
        &[Arg::Float(-2.0), Arg::Bytes(b"addon_twice"), Arg::Float(1.0)],
    );
    assert_eq!(r.f32(), 112.0);
    let r = call(
        &mut vm,
        "externcall",
        &[Arg::Float(1.0), Arg::Bytes(b"1:addon_twice"), Arg::Float(0.0)],
    );
    assert_eq!(r.f32(), 110.0, "an N: prefix selects the progs");

    // externvalue reads another progs' globals (relocated strings included) or their addresses.
    let r = call(&mut vm, "externvalue", &[Arg::Float(1.0), Arg::Bytes(b"greeting")]);
    assert_eq!(vm.str(r.str_ref()), b"hello from the addon");
    let r = call(&mut vm, "externvalue", &[Arg::Float(1.0), Arg::Bytes(b"thisprogs")]);
    assert_eq!(r.f32(), 1.0);
    let r = call(&mut vm, "externvalue", &[Arg::Float(0.0), Arg::Bytes(b"greeting")]);
    assert_eq!(r.0[0], 0, "the main progs has no such global");
    let r = call(&mut vm, "externvalue", &[Arg::Float(1.0), Arg::Bytes(b"addon_twice")]);
    assert_eq!(r.func(), vm.find_function_in(pr, "addon_twice").unwrap(), "a function by name");

    // externset writes them; `&name` gives an address QuakeC pointers can use.
    call(&mut vm, "externset", &[Arg::Float(1.0), Arg::Float(42.0), Arg::Bytes(b"init_arg")]);
    let init_arg = vm.global_in::<f32>(pr, "init_arg").unwrap();
    assert_eq!(vm.get(init_arg), 42.0);
    let r = call(&mut vm, "externvalue", &[Arg::Float(1.0), Arg::Bytes(b"&init_arg")]);
    let bytes = vm.read_mem(qcvm::Ptr(r.0[0]), 4).unwrap();
    assert_eq!(f32::from_le_bytes(bytes.try_into().unwrap()), 42.0);

    // isfunction and callfunction search every progs.
    assert_eq!(call(&mut vm, "isfunction", &[Arg::Bytes(b"addon_twice")]).f32(), 1.0);
    assert_eq!(call(&mut vm, "isfunction", &[Arg::Bytes(b"nonexistent")]).f32(), 0.0);
    let r = call(&mut vm, "callfunction", &[Arg::Float(4.0), Arg::Bytes(b"addon_twice")]);
    assert_eq!(r.f32(), 118.0);
}

fn asm_builtin(asm: &mut Asm, name: &str, number: u32) {
    let f = asm.builtin(name, number, -1);
    asm.global(name, ty::FUNCTION, &[f]);
}
