// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs `main` of a progs file with a few console builtins (`#1` puts, `#2` ftos, `#3` spawn,
//! like FTE's standalone runner) and reports how long it took.
//!
//! Usage: `cargo run --release --example run -- progs.dat`

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::sync::Arc;
use std::time::Instant;

use qcvm::{Builtins, Host, Numbering, Program, Vm, VmConfig, VmError};

struct Console;
impl Host for Console {}

fn puts(vm: &mut Vm<Console>, _: &mut Console) -> Result<(), VmError> {
    for i in 0..vm.argc() {
        print!("{}", String::from_utf8_lossy(vm.arg_str(i)));
    }
    Ok(())
}

fn ftos(vm: &mut Vm<Console>, _: &mut Console) -> Result<(), VmError> {
    let s = format!("{}", vm.arg_f32(0));
    vm.ret_str(s.as_bytes())
}

fn spawn(vm: &mut Vm<Console>, _: &mut Console) -> Result<(), VmError> {
    let e = vm.spawn()?;
    vm.ret_ent(e);
    Ok(())
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: run <progs.dat>");
    let program = Arc::new(Program::parse(&std::fs::read(path).unwrap()).unwrap());
    let mut b = Builtins::empty(Numbering::None);
    b.set_numbered(1, "puts", puts).set_numbered(2, "ftos", ftos).set_numbered(3, "spawn", spawn);
    let mut vm = Vm::new(program, Arc::new(b), VmConfig::default()).unwrap();
    let main = vm.find_function("main").expect("no main");
    let start = Instant::now();
    let result = vm.call(&mut Console, main, &[]);
    eprintln!("{:?} in {:.3?}", result.map(|r| r.0), start.elapsed());
}
