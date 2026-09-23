// SPDX-License-Identifier: MIT OR Apache-2.0

//! QuakeC threads (`sleep`, `fork`) resumed by the host with `run_threads`.

use std::sync::Arc;

use qcvm::{Builtins, Host, Numbering, Op, Program, ProgsFormat, Vm, VmConfig, VmError};

use crate::support::asm::{Asm, parm, ty};
use crate::support::qcc::compile;

#[derive(Default)]
struct Out(Vec<u8>);

impl Host for Out {}

fn puts(vm: &mut Vm<Out>, host: &mut Out) -> Result<(), VmError> {
    for i in 0..vm.argc() {
        host.0.extend_from_slice(vm.arg_str(i));
    }
    Ok(())
}

fn ftos(vm: &mut Vm<Out>, _: &mut Out) -> Result<(), VmError> {
    let s = format!("{}", vm.arg_f32(0));
    vm.ret_str(s.as_bytes())
}

fn abort(vm: &mut Vm<Out>, _: &mut Out) -> Result<(), VmError> {
    Err(VmError::abort(vm.arg_raw(0)))
}

/// A resumed thread that ends with `abort`, or suspends again, leaves nothing behind for the
/// next, unrelated host call.
#[test]
fn resumed_threads_do_not_leak_return_values() {
    let mut asm = Asm::new();
    asm.global("time", ty::FLOAT, &[]);
    let sleep = asm.builtin("sleep", 212, 1);
    let sleep_g = asm.global("sleep_g", ty::FUNCTION, &[sleep]);
    let ab = asm.builtin("abort", 211, 1);
    let abort_g = asm.global("abort_g", ty::FUNCTION, &[ab]);
    let (zero, n99, n42) = (asm.float(0.0), asm.float(99.0), asm.float(42.0));
    asm.function("then_abort", &[], 0);
    asm.emit(Op::StoreF, zero, parm(0), 0);
    asm.emit(Op::Call1, sleep_g, 0, 0);
    asm.emit(Op::StoreF, n99, parm(0), 0);
    asm.emit(Op::Call1, abort_g, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);
    asm.function("sleep_twice", &[], 0);
    asm.emit(Op::StoreF, zero, parm(0), 0);
    asm.emit(Op::Call1, sleep_g, 0, 0);
    asm.emit(Op::StoreF, zero, parm(0), 0);
    asm.emit(Op::Call1, sleep_g, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);
    asm.function("answer", &[], 0);
    asm.emit(Op::Return, n42, 0, 0);

    let mut b = Builtins::standard(Numbering::Ssqc);
    b.set_numbered(211, "abort", abort);
    let program = Arc::new(Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap());
    let mut vm = Vm::new(program, Arc::new(b), VmConfig::ssqc()).unwrap();
    let mut host = Out::default();
    let answer = vm.find_function("answer").unwrap();
    for worker in ["then_abort", "sleep_twice"] {
        let f = vm.find_function(worker).unwrap();
        vm.call(&mut host, f, &[]).unwrap();
        assert_eq!(vm.run_threads(&mut host).unwrap(), 1, "{worker}");
        assert_eq!(vm.call(&mut host, answer, &[]).unwrap().f32(), 42.0, "after {worker}");
    }
    assert_eq!(vm.sleeping_threads(), 1, "sleep_twice is asleep again");
}

#[test]
fn sleep_fork_and_nested_sleeps() {
    let Some(compiled) = compile(&["threads.qc"], &["-Tfte"]) else { return };
    let program = Arc::new(Program::parse(&compiled.dat).unwrap());
    let mut b = Builtins::standard(Numbering::Ssqc);
    b.set_numbered(1, "puts", puts).set_numbered(2, "ftos", ftos).set_numbered(211, "abort", abort);
    let mut vm = Vm::new(program, Arc::new(b), VmConfig::ssqc()).unwrap();
    let mut host = Out::default();
    let time = vm.global::<f32>("time").unwrap();
    let main = vm.find_function("main").unwrap();

    vm.call(&mut host, main, &[]).unwrap();
    assert_eq!(vm.sleeping_threads(), 1);
    for step in 1..=12 {
        vm.set(time, step as f32 * 0.5);
        vm.run_threads(&mut host).unwrap();
        // Temp strings referenced only by sleeping threads survive collections.
        vm.collect_garbage().unwrap();
    }
    assert_eq!(vm.sleeping_threads(), 0);
    let expected = "loop 0 at 0\n  label 0\nloop 1 at 1\n  label 10\nloop 2 at 2\n  label 20\n\
                    loop done\nmain continues\nparent after fork\nnested 103\nforked child at 5\n";
    assert_eq!(String::from_utf8_lossy(&host.0), expected);
}
