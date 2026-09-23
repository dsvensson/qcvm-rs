// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs `main` of a progs file with the builtins of FTE's standalone runner (`#1` puts, `#2`
//! ftos, `#3` spawn, …; see `tests/all/support/runner.rs`), prints what it printed, and reports
//! how long the call took. `scripts/bench-fte.ps1` uses it to time qcvm against FTE's runner.
//!
//! Usage: `cargo run --release --example run -- progs.dat`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

#[path = "../tests/all/support/runner.rs"]
#[allow(unreachable_pub)]
mod runner;

use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use qcvm::{Program, Vm, VmConfig};

fn main() {
    let path = std::env::args().nth(1).expect("usage: run <progs.dat>");
    let program = Arc::new(Program::parse(&std::fs::read(path).unwrap()).unwrap());
    let mut vm = Vm::new(program, Arc::new(runner::builtins()), VmConfig::default()).unwrap();
    let main = vm.find_function("main").expect("no main");
    let mut host = runner::RunnerHost::default();
    let start = Instant::now();
    let result = vm.call(&mut host, main, &[]);
    let elapsed = start.elapsed();
    std::io::stdout().write_all(&host.out).unwrap();
    eprintln!("{:?} in {elapsed:.3?}", result.map(|r| r.0));
}
