// SPDX-License-Identifier: MIT OR Apache-2.0

//! Prints the disassembly of every QuakeC function in a progs file, a histogram of the opcodes it
//! uses with `--ops`, or the builtins its code calls with `--builtins`.
//!
//! Usage: `cargo run --example disasm -- progs.dat [--ops | --builtins]`

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::{BTreeMap, BTreeSet};

use qcvm::Program;
use qcvm::progs::FunctionKind;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: disasm <progs.dat> [--ops | --builtins]");
    let data = std::fs::read(path).expect("read progs");
    let mut program = Program::parse(&data).expect("parse progs");
    let lno = std::path::Path::new(path).with_extension("lno");
    if let Ok(lines) = std::fs::read(lno) {
        program = program.with_line_numbers(&lines).expect("line numbers");
    }
    for note in program.load_notes() {
        eprintln!("note: {note}");
    }
    if args.iter().any(|a| a == "--ops") {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for f in program.functions() {
            if matches!(f.kind, FunctionKind::QuakeC { .. }) {
                for line in program.disassemble(f.index).to_string().lines().skip(2) {
                    if let Some(op) = line.split_whitespace().nth(1) {
                        *counts.entry(op.to_string()).or_default() += 1;
                    }
                }
            }
        }
        for (op, n) in counts {
            println!("{n:6} {op}");
        }
        return;
    }
    if args.iter().any(|a| a == "--builtins") {
        let mut called = BTreeSet::new();
        for f in program.functions() {
            if matches!(f.kind, FunctionKind::QuakeC { .. }) {
                for line in program.disassemble(f.index).to_string().lines().skip(2) {
                    let mut words = line.split_whitespace().skip(1);
                    if words.next().is_some_and(|op| op.starts_with("CALL"))
                        && let Some(name) = words.next()
                    {
                        called.insert(name.trim_end_matches(',').to_string());
                    }
                }
            }
        }
        for f in program.functions() {
            let number = match f.kind {
                FunctionKind::Builtin { number } => number.to_string(),
                FunctionKind::NamedBuiltin => "0".to_string(),
                _ => continue,
            };
            let name = String::from_utf8_lossy(f.name);
            if called.contains(name.as_ref()) {
                println!("#{number:<5} {name}");
            }
        }
        return;
    }
    for f in program.functions() {
        if matches!(f.kind, FunctionKind::QuakeC { .. }) {
            println!("{}", program.disassemble(f.index));
        }
    }
}
