// SPDX-License-Identifier: MIT OR Apache-2.0

//! Random programs never panic the VM.
//!
//! Each case assembles functions from random statements — any opcode number, operands mostly
//! within the globals (so they do something) and sometimes anywhere — over globals holding random
//! words, fields, a spawned entity and builtins that re-enter QuakeC, allocate temp strings and
//! suspend threads. Running them may fail in any way the VM reports, but must not panic. Set
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
