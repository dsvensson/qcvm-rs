// SPDX-License-Identifier: MIT OR Apache-2.0

//! Random programs never panic the VM.
//!
//! Each case assembles functions from random statements — any opcode number, operands mostly
//! within the globals (so they do something) and sometimes anywhere — over globals holding random
//! words, fields, a spawned entity and builtins that re-enter QuakeC, allocate temp strings and
//! suspend threads. Running them may fail in any way the VM reports, but must not panic. The
//! standard builtins get the same treatment: random call sequences with hostile arguments. Set
//! `QCVM_FUZZ_CASES` for a longer run.

use std::sync::Arc;

use proptest::prelude::*;
use qcvm::{
    Arg, Builtins, FuncRef, Limits, Numbering, Op, Program, ProgsFormat, Vm, VmConfig, VmError,
};

use crate::support::asm::{Asm, ty};
use crate::vm_basic::TestHost;

type V = Vm<TestHost>;

fn b_spawn(vm: &mut V, _h: &mut TestHost) -> Result<(), VmError> {
    let e = vm.spawn()?;
    vm.ret_ent(e);
    Ok(())
}

fn b_temp(vm: &mut V, _h: &mut TestHost) -> Result<(), VmError> {
    let text = format!("{}{:?}", vm.arg_f32(0), vm.arg_str(1));
    vm.ret_str(text.as_bytes())
}

/// Calls the function in its first argument (re-entering QuakeC from a builtin).
fn b_call(vm: &mut V, h: &mut TestHost) -> Result<(), VmError> {
    let f = vm.arg_func(0);
    let arg = vm.arg_raw(1);
    let r = vm.call(h, f, &[Arg::Raw(arg)])?;
    vm.ret_raw(r.0);
    Ok(())
}

/// Collects garbage, which must fail harmlessly while QuakeC runs.
fn b_gc(vm: &mut V, _h: &mut TestHost) -> Result<(), VmError> {
    let _ = vm.collect_garbage();
    Ok(())
}

fn cases() -> u32 {
    std::env::var("QCVM_FUZZ_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256)
}

/// A statement operand: usually a global of the program, sometimes a small jump, sometimes any
/// value at all.
fn operand() -> impl Strategy<Value = u32> {
    prop_oneof![
        8 => 0u32..96,
        2 => (-8i32..8).prop_map(|v| v as u32),
        1 => any::<u32>(),
    ]
}

fn opcode() -> impl Strategy<Value = u32> {
    prop_oneof![9 => 0u32..282, 1 => any::<u16>().prop_map(u32::from)]
}

/// A word for a global: small numbers, floats, entity numbers, function references and
/// addresses, plus arbitrary bits.
fn word() -> impl Strategy<Value = u32> {
    prop_oneof![
        0u32..8,
        (-4.0f32..300.0).prop_map(f32::to_bits),
        prop::sample::select(vec![0x8000_0000, 0xC000_0000, 0xFFFF_FFFF, 1 << 24, 0x0100_0003]),
        any::<u32>(),
    ]
}

type Stmts = Vec<(u32, u32, u32, u32)>;

fn build(funcs: &[Stmts], words: &[u32], format: ProgsFormat) -> Vec<u8> {
    let mut asm = Asm::new();
    asm.global("self", ty::ENTITY, &[]);
    asm.global("time", ty::FLOAT, &[]);
    asm.field("health", ty::FLOAT);
    asm.field("origin", ty::VECTOR);
    asm.field("think", ty::FUNCTION);
    let builtins = [
        asm.builtin("spawn", 1, 0),
        asm.builtin("temp", 2, 2),
        asm.builtin("call", 3, 2),
        asm.builtin("gc", 4, 0),
        asm.builtin("sleep", 212, 1),
        asm.builtin("fork", 210, 0),
    ];
    for (i, b) in builtins.into_iter().enumerate() {
        asm.global(&format!("builtin{i}"), ty::FUNCTION, &[b]);
    }
    asm.string("some text");
    asm.alloc(words.len() as u32, words);
    for (i, stmts) in funcs.iter().enumerate() {
        let f = asm.function(&format!("f{i}"), &[1, 3], 4);
        asm.global(&format!("f{i}_g"), ty::FUNCTION, &[f.index]);
        for &(op, a, b, c) in stmts {
            asm.emit_raw(op, a, b, c);
        }
        asm.emit(Op::Done, 0, 0, 0);
    }
    asm.build(format)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn random_statements_never_panic(
        funcs in prop::collection::vec(
            prop::collection::vec((opcode(), operand(), operand(), operand()), 1..48),
            1..4,
        ),
        words in prop::collection::vec(word(), 8..40),
        wide in any::<bool>(),
        arg in word(),
    ) {
        let format = if wide { ProgsFormat::Fte32 } else { ProgsFormat::Fte16 };
        let Ok(program) = Program::parse(&build(&funcs, &words, format)) else {
            return Ok(());
        };
        let mut b = Builtins::standard(Numbering::Csqc);
        b.set_numbered(1, "spawn", b_spawn);
        b.set_numbered(2, "temp", b_temp);
        b.set_numbered(3, "call", b_call);
        b.set_numbered(4, "gc", b_gc);
        let limits = Limits {
            runaway: 20_000,
            call_depth: 64,
            local_stack_words: 4096,
            reentry: 8,
            ..Limits::default()
        };
        let config = VmConfig { limits, ..VmConfig::csqc() };
        let mut vm = Vm::new(Arc::new(program), Arc::new(b), config).unwrap();
        let mut host = TestHost::default();
        let _ = vm.spawn();
        for i in 0..funcs.len() {
            let f = vm.find_function(format!("f{i}")).unwrap_or(FuncRef::NULL);
            let _ = vm.call(&mut host, f, &[Arg::Raw([arg, 0, 0]), Arg::Vector([1.0, 2.0, 3.0])]);
            let _ = vm.run_threads(&mut host);
        }
        let _ = vm.collect_garbage();
        vm.reset().unwrap();
    }
}

// ---- standard builtins with hostile arguments -------------------------------------------------

/// A progs that declares every builtin of `registry` (by number where it has one, else by name)
/// together with the globals and fields the standard builtins look up.
fn builtin_progs(registry: &Builtins<qcvm::NullHost>) -> (Vec<u8>, Vec<String>) {
    let mut asm = Asm::new();
    asm.global("self", ty::ENTITY, &[]);
    asm.global("other", ty::ENTITY, &[]);
    asm.global("time", ty::FLOAT, &[]);
    for v in ["v_forward", "v_right", "v_up"] {
        asm.global(v, ty::VECTOR, &[0, 0, 0]);
    }
    for (name, t) in [
        ("origin", ty::VECTOR),
        ("mins", ty::VECTOR),
        ("maxs", ty::VECTOR),
        ("angles", ty::VECTOR),
        ("gravitydir", ty::VECTOR),
        ("solid", ty::FLOAT),
        ("flags", ty::FLOAT),
        ("ideal_yaw", ty::FLOAT),
        ("yaw_speed", ty::FLOAT),
        ("idealpitch", ty::FLOAT),
        ("pitch_speed", ty::FLOAT),
        ("health", ty::FLOAT),
        ("chain", ty::ENTITY),
        ("classname", ty::STRING),
        ("think", ty::FUNCTION),
    ] {
        asm.field(name, t);
    }
    for s in ["", "hello world", r"a\b\c", "{\"k\": [1, 2.5, \"x\"]}", "%s %d %v %c", "MD5"] {
        asm.string(s);
    }
    let mut names = Vec::new();
    for (name, number) in registry.registered() {
        let name = String::from_utf8_lossy(name).into_owned();
        let f = asm.builtin(&name, number.unwrap_or(0), -1);
        asm.global(&format!("{name}_ref"), ty::FUNCTION, &[f]);
        names.push(name);
    }
    let f = asm.function("callback", &[1, 1], 1);
    asm.emit(Op::AddF, f.local(0), f.local(1), f.local(2));
    asm.emit(Op::Return, f.local(2), 0, 0);
    (asm.build(ProgsFormat::Fte16), names)
}

/// An argument word: floats (ordinary and extreme), string references (program strings, temps,
/// interned, invalid), entity numbers, pointers and arbitrary bits.
fn arg_word() -> impl Strategy<Value = u32> {
    prop_oneof![
        (-10.0f32..300.0).prop_map(f32::to_bits),
        prop::sample::select(vec![
            f32::NAN.to_bits(),
            f32::INFINITY.to_bits(),
            f32::NEG_INFINITY.to_bits(),
            3.0e9f32.to_bits(),
            (-3.0e9f32).to_bits(),
            1.0e20f32.to_bits(),
            f32::MIN_POSITIVE.to_bits(),
            (-0.0f32).to_bits(),
        ]),
        0u32..64,
        (0u32..8).prop_map(|k| 0x8000_0000 | k),
        (0u32..4).prop_map(|k| 0xC000_0000 | k),
        prop::sample::select(vec![0xFFFF_FFFF, 0x7FFF_FFFF, 0x4000_0000, 1 << 24]),
        any::<u32>(),
    ]
}

type Calls = Vec<(prop::sample::Index, u8, [[u32; 3]; 8])>;

fn run_builtin_calls(numbering: Numbering, calls: &Calls) {
    let registry = Builtins::<qcvm::NullHost>::standard(numbering);
    let (dat, names) = builtin_progs(&registry);
    let program = Arc::new(Program::parse(&dat).unwrap());
    let limits = Limits { runaway: 100_000, heap_bytes: 1 << 22, ..Limits::default() };
    let config = VmConfig { limits, developer: true, ..VmConfig::csqc() };
    let mut vm = Vm::new(program, Arc::new(registry), config).unwrap();
    let mut host = qcvm::NullHost;
    for _ in 0..3 {
        let _ = vm.spawn();
    }
    for text in [&b"temp one"[..], b"", b"t\xC3\xA9mp", b"^1red ^7white"] {
        let _ = vm.temp(text);
    }
    let _ = vm.intern(b"interned");
    for (pick, argc, args) in calls {
        let name = &names[pick.index(names.len())];
        let Some(f) = vm.find_function(name) else { continue };
        let args: Vec<Arg<'_>> =
            args.iter().take(usize::from(*argc % 9)).map(|&w| Arg::Raw(w)).collect();
        let _ = vm.call(&mut host, f, &args);
        let _ = vm.run_threads(&mut host);
    }
    let _ = vm.collect_garbage();
}

fn calls() -> impl Strategy<Value = Calls> {
    prop::collection::vec(
        (
            any::<prop::sample::Index>(),
            any::<u8>(),
            prop::array::uniform8(prop::array::uniform3(arg_word())),
        ),
        1..24,
    )
}

proptest! {
    // Each case builds a VM with every builtin declared, so run a quarter as many by default.
    #![proptest_config(ProptestConfig::with_cases(cases().div_ceil(4)))]

    #[test]
    fn standard_builtins_never_panic_csqc(calls in calls()) {
        run_builtin_calls(Numbering::Csqc, &calls);
    }

    #[test]
    fn standard_builtins_never_panic_ssqc(calls in calls()) {
        run_builtin_calls(Numbering::Ssqc, &calls);
    }

    #[test]
    fn standard_builtins_never_panic_menu(calls in calls()) {
        run_builtin_calls(Numbering::Menu, &calls);
    }
}
