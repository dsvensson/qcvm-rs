// SPDX-License-Identifier: MIT OR Apache-2.0

//! Prints the disassembly of every QuakeC function in a progs file, or a histogram of the
//! opcodes it uses with `--ops`.
//!
//! Usage: `cargo run --example disasm -- progs.dat [--ops]`

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;

use qcvm::Program;
use qcvm::progs::FunctionKind;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: disasm <progs.dat> [--ops]");
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
    for f in program.functions() {
        if matches!(f.kind, FunctionKind::QuakeC { .. }) {
            println!("{}", program.disassemble(f.index));
        }
    }
}
