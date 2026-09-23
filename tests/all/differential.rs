// SPDX-License-Identifier: MIT OR Apache-2.0

//! QuakeC fixtures (tests/qc) compiled with fteqcc and run under both FTE's standalone `qcvm`
//! runner and qcvm; their output must match.
//!
//! When the runner is unavailable, output is compared against `tests/qc/<fixture>.expected`,
//! which is (re)generated from the runner with `QCVM_BLESS=1`.

use std::fs;
use std::process::Command;
use std::sync::Arc;

use qcvm::{Program, Vm, VmConfig};

use crate::support::qcc::{compile, fixture_dir};
use crate::support::runner::{RunnerHost, builtins};
use crate::support::tools;

/// Compiler targets every fixture is built for.
const TARGETS: &[(&str, &[&str])] =
    &[("fte", &["-Tfte"]), ("fte-O3", &["-Tfte", "-O3"]), ("vanilla", &["-Tvanilla"])];

fn run_ours(dat: &[u8]) -> String {
    let program = Arc::new(Program::parse(dat).expect("parse"));
    let mut config = VmConfig::default();
    config.limits.local_stack_words = 1 << 16;
    let mut vm = Vm::new(program, Arc::new(builtins()), config).expect("vm");
    let mut host = RunnerHost::default();
    let main = vm.find_function("main").expect("main");
    let result = vm.call(&mut host, main, &[]);
    let mut out = String::from_utf8_lossy(&host.out).into_owned();
    if let Err(e) = result {
        out.push_str(&format!("ERROR: {}\n", e.kind()));
    }
    out
}

fn run_oracle(path: &std::path::Path) -> Option<String> {
    let runner = tools::fte_qcvm()?;
    let output = Command::new(runner).arg(path).output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"))
}

/// Builds `fixture` for each of `targets`, runs it under both VMs and compares.
fn check_targets(fixture: &str, targets: &[(&str, &[&str])]) {
    let expected_path = fixture_dir().join(format!("{fixture}.expected"));
    let expected_file = fs::read_to_string(&expected_path).ok().map(|s| s.replace("\r\n", "\n"));
    for (label, flags) in targets {
        let Some(compiled) = compile(&["runner.qh", fixture], flags) else { return };
        let ours = run_ours(&compiled.dat);
        match run_oracle(&compiled.path) {
            Some(oracle) => {
                assert_eq!(ours, oracle, "{fixture} [{label}]: qcvm vs FTE qcvm");
                if std::env::var_os("QCVM_BLESS").is_some() {
                    fs::write(&expected_path, &oracle).unwrap();
                } else if let Some(expected) = &expected_file {
                    assert_eq!(&oracle, expected, "{fixture}: FTE output drifted from .expected");
                }
            }
            None => match &expected_file {
                Some(expected) => assert_eq!(&ours, expected, "{fixture} [{label}]"),
                None => tools::skip(fixture, "FTE_QCVM and an .expected file"),
            },
        }
    }
}

fn check(fixture: &str) {
    check_targets(fixture, TARGETS);
}

#[test]
fn arith() {
    check("arith.qc");
}

/// Targets for fixtures using FTE-only language features.
const FTE_ONLY: &[(&str, &[&str])] = &[("fte", &["-Tfte"]), ("fte-O3", &["-Tfte", "-O3"])];

#[test]
fn control() {
    check("control.qc");
}

#[test]
fn ints() {
    check_targets("ints.qc", FTE_ONLY);
}

#[test]
fn pointers() {
    check_targets("pointers.qc", FTE_ONLY);
}

#[test]
fn entities() {
    check("entities.qc");
}

#[test]
fn strings() {
    check("strings.qc");
}

#[test]
fn funcs() {
    check_targets(
        "funcs.qc",
        &[("fte_6614", &["-Tfte_6614"]), ("fte_6614-O3", &["-Tfte_6614", "-O3"])],
    );
}

#[test]
fn hexen2() {
    check_targets("hexen2.qc", &[("h2", &["-Th2"])]);
}

#[test]
fn state() {
    check("state.qc");
}

#[test]
fn advanced() {
    check_targets("advanced.qc", FTE_ONLY);
}
