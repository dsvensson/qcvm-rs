// SPDX-License-Identifier: MIT OR Apache-2.0

//! Program loading: every format, the load-time rewrites, and malformed input.

use proptest::prelude::*;
use qcvm::progs::{FunctionKind, InvalidFunction, Type};
use qcvm::{LoadError, LoadNote, Op, Program, ProgsFormat};

use crate::support::asm::{Asm, OFS_RETURN, parm, ty};
use crate::support::tools;

const ALL_FORMATS: [ProgsFormat; 6] = [
    ProgsFormat::QTest,
    ProgsFormat::V6,
    ProgsFormat::Fte16,
    ProgsFormat::Fte32,
    ProgsFormat::Kk7,
    ProgsFormat::UHexen2,
];

/// A small program exercising every operand kind: globals, jumps (both directions), global
/// indices and immediates.
fn sample() -> Asm {
    let mut asm = Asm::new();
    let one = asm.float(1.0);
    let ten = asm.float(10.0);
    let counter = asm.global("counter", ty::FLOAT, &[]);
    let (_, origin) = asm.field("origin", ty::VECTOR);
    let _health = asm.field("health", ty::FLOAT);
    let print = asm.builtin("print", 1, 1);
    let hello = asm.str_const("hello");
    let print_g = asm.global("print_ref", ty::FUNCTION, &[print]);

    let main = asm.function("main", &[], 2);
    let top = asm.here();
    asm.emit(Op::AddF, counter, one, counter);
    let lt = main.local(0);
    asm.emit(Op::LtF, counter, ten, lt);
    let back = asm.emit(Op::IfI, lt, 0, 0);
    asm.patch_jump(back, 1, top);
    asm.emit(Op::StoreS, hello, parm(0), 0);
    asm.emit(Op::Call1, print_g, 0, 0);
    let arr = asm.alloc(4, &[1, 2, 3, 4]);
    let idx = asm.int(2);
    asm.emit(Op::LoadAF, arr, idx, main.local(1));
    asm.emit(Op::BoundCheck, idx, 4, 0);
    let skip = asm.emit(Op::Goto, 0, 0, 0);
    asm.emit(Op::StoreF, origin, OFS_RETURN, 0);
    let end = asm.emit(Op::Return, 0, 0, 0);
    asm.patch_jump(skip, 0, end);
    asm.emit(Op::Done, 0, 0, 0);
    asm
}

/// Disassembly of every function, without QTest's per-statement line annotations.
fn listing(p: &Program) -> String {
    let text: String = (0..p.num_functions()).map(|i| p.disassemble(i).to_string()).collect();
    text.lines().map(|l| l.split("    ; line").next().unwrap_or(l)).collect::<Vec<_>>().join("\n")
}

#[test]
fn every_format_round_trips_to_the_same_program() {
    let asm = sample();
    let reference = Program::parse(&asm.build(ProgsFormat::V6)).unwrap();
    let expected = listing(&reference);
    assert!(expected.contains("IF_I"), "{expected}");
    for format in ALL_FORMATS {
        let p = Program::parse(&asm.build(format)).unwrap_or_else(|e| panic!("{format:?}: {e}"));
        assert_eq!(p.format(), format);
        let notes: Vec<_> =
            p.load_notes().iter().filter(|n| **n != LoadNote::Hexen2Calls).collect();
        assert!(notes.is_empty(), "{format:?}: {notes:?}");
        if format == ProgsFormat::UHexen2 {
            // uHexen2 always uses the Hexen 2 calling convention.
            assert_eq!(
                listing(&p),
                expected.replace("CALL1          print_ref", "CALL1H         print_ref, g0, g0")
            );
        } else {
            assert_eq!(listing(&p), expected, "{format:?}");
        }
        assert_eq!(p.num_globals(), asm.num_globals());
        assert_eq!(p.global_def("counter").unwrap().ty, Type::Float);
        assert_eq!(p.field_def("origin").unwrap().ty, Type::Vector);
        assert_eq!(p.field_def("origin_y").unwrap().offset, 1);
        assert_eq!(p.entity_fields(), 4);
        let main = p.function_index("main").unwrap();
        assert!(matches!(p.function(main).unwrap().kind, FunctionKind::QuakeC { .. }));
        let print = p.function_index("print").unwrap();
        assert_eq!(p.function(print).unwrap().kind, FunctionKind::Builtin { number: 1 });
    }
}

#[test]
fn qtest_statements_carry_line_numbers() {
    let p = Program::parse(&sample().build(ProgsFormat::QTest)).unwrap();
    assert_eq!(p.source_line(0), Some(1));
    assert_eq!(p.source_line(3), Some(4));
}

#[test]
fn hexen2_calls_are_detected_and_rewritten() {
    let mut asm = Asm::new();
    let f = asm.builtin("f", 1, 2);
    let fg = asm.global("fg", ty::FUNCTION, &[f]);
    let x = asm.float(1.0);
    asm.function("main", &[], 0);
    asm.emit(Op::Call2, fg, x, x);
    asm.emit(Op::Call0, fg, 0, 0);
    asm.emit(Op::Rand0, 0, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);
    let p = Program::parse(&asm.build(ProgsFormat::V6)).unwrap();
    assert!(p.load_notes().contains(&LoadNote::Hexen2Calls));
    let text = listing(&p);
    assert!(text.contains("CALL2H"), "{text}");
    assert!(text.contains("CALL0 "), "CALL0 stays: {text}");
    // RAND0 with c == 0 gets the return slot.
    assert!(text.contains("RAND0          g1"), "{text}");

    // Without an operand b, calls are left alone.
    let mut asm = Asm::new();
    let f = asm.builtin("f", 1, 1);
    let fg = asm.global("fg", ty::FUNCTION, &[f]);
    asm.function("main", &[], 0);
    asm.emit(Op::Call1, fg, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);
    let p = Program::parse(&asm.build(ProgsFormat::V6)).unwrap();
    assert!(!p.load_notes().contains(&LoadNote::Hexen2Calls));
}

#[test]
fn out_of_range_operands_and_unknown_opcodes_poison_the_statement() {
    let mut asm = Asm::new();
    asm.function("main", &[], 0);
    let n = asm.num_globals();
    let bad = asm.emit(Op::AddF, 1, 2, n + 100);
    let unknown = asm.emit_raw(500, 0, 0, 0);
    // Operands may reach three words past the globals (FTE's zero tail) ...
    let edge = asm.emit(Op::StoreF, 1, 2, 0);
    asm.emit(Op::Done, 0, 0, 0);
    let limit = asm.num_globals() + 3;
    asm.patch(edge, 1, limit - 1);
    // ... but not further.
    let over = asm.emit(Op::StoreF, 1, limit, 0);
    let p = Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap();
    let notes = p.load_notes();
    assert!(notes.contains(&LoadNote::PoisonedStatement { index: bad, opcode: Op::AddF as u32 }));
    assert!(notes.contains(&LoadNote::PoisonedStatement { index: unknown, opcode: 500 }));
    assert!(
        notes.contains(&LoadNote::PoisonedStatement { index: over, opcode: Op::StoreF as u32 })
    );
    assert!(
        !notes.contains(&LoadNote::PoisonedStatement { index: edge, opcode: Op::StoreF as u32 })
    );
    assert!(listing(&p).contains("BAD"));
}

#[test]
fn switch_jump_operands_are_not_range_checked() {
    // FTE's sanitizer treats SWITCH_*.b as a global and would poison a long forward switch.
    let mut asm = Asm::new();
    asm.function("main", &[], 1);
    let s = asm.emit(Op::SwitchF, 1, 0, 0);
    for _ in 0..(asm.num_globals() + 10) {
        asm.emit(Op::Done, 0, 0, 0);
    }
    let target = asm.here();
    asm.emit(Op::Done, 0, 0, 0);
    asm.patch_jump(s, 1, target);
    let p = Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap();
    assert!(p.load_notes().is_empty(), "{:?}", p.load_notes());
}

#[test]
fn wild_jumps_fault_only_when_taken() {
    let mut asm = Asm::new();
    asm.function("main", &[], 0);
    let j = asm.emit(Op::Goto, 0, 0, 0);
    asm.patch(j, 0, 5000);
    let back = asm.emit(Op::IfI, 1, 0, 0);
    asm.patch(back, 1, (-100i32) as u32);
    asm.emit(Op::Done, 0, 0, 0);
    let p = Program::parse(&asm.build(ProgsFormat::V6)).unwrap();
    assert!(p.load_notes().contains(&LoadNote::JumpOutOfRange { index: j }));
    assert!(p.load_notes().contains(&LoadNote::JumpOutOfRange { index: back }));
    let sentinel = p.num_statements();
    assert!(listing(&p).contains(&format!("GOTO           -> {sentinel}")), "{}", listing(&p));
}

#[test]
fn malformed_functions_are_marked_invalid() {
    let mut asm = Asm::new();
    let entry = asm.function("entry_oob", &[], 0);
    asm.emit(Op::Done, 0, 0, 0);
    let locals = asm.function("locals_oob", &[], 0);
    asm.emit(Op::Done, 0, 0, 0);
    let params = asm.function("params_oob", &[3], 0);
    asm.emit(Op::Done, 0, 0, 0);
    let named = asm.builtin("named", 0, 0);
    asm.patch_function(entry.index, 99_999, entry.parm_start, 0);
    asm.patch_function(locals.index, 3, 60_000, 100);
    let n = asm.num_globals();
    asm.patch_function(params.index, 3, n + 2, 1);
    let p = Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap();
    let kind = |i| p.function(i).unwrap().kind;
    assert_eq!(kind(entry.index), FunctionKind::Invalid(InvalidFunction::EntryOutOfRange));
    assert_eq!(kind(locals.index), FunctionKind::Invalid(InvalidFunction::LocalsOutOfRange));
    assert_eq!(kind(params.index), FunctionKind::Invalid(InvalidFunction::LocalsOutOfRange));
    assert_eq!(kind(named), FunctionKind::NamedBuiltin);
    assert_eq!(kind(0), FunctionKind::Null);
    assert_eq!(
        p.load_notes().iter().filter(|n| matches!(n, LoadNote::InvalidFunction { .. })).count(),
        3
    );
}

#[test]
fn breakpoint_bit_is_masked() {
    let mut asm = Asm::new();
    asm.function("main", &[], 0);
    asm.emit_raw(0x8000 | Op::AddF as u32, 1, 2, 3);
    asm.emit(Op::Done, 0, 0, 0);
    let p = Program::parse(&asm.build(ProgsFormat::V6)).unwrap();
    assert!(p.load_notes().is_empty());
    assert!(listing(&p).contains("ADD_F"));
}

#[test]
fn header_errors() {
    assert_eq!(Program::parse(&[]).unwrap_err(), LoadError::Truncated);
    assert_eq!(Program::parse(&[6, 0, 0, 0]).unwrap_err(), LoadError::Truncated);

    let mut asm = sample();
    asm.header_overrides.push((0, 5));
    assert_eq!(
        Program::parse(&asm.build(ProgsFormat::V6)).unwrap_err(),
        LoadError::UnsupportedVersion(5)
    );

    // A version-7 header needs the extended fields.
    let data = sample().build(ProgsFormat::V6);
    let mut v7 = data[..60].to_vec();
    v7[0] = 7;
    assert_eq!(Program::parse(&v7).unwrap_err(), LoadError::Truncated);

    let mut asm = sample();
    asm.header_overrides.push((21, 0x3)); // blockscompressed
    assert_eq!(
        Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap_err(),
        LoadError::Compressed(3)
    );

    let mut asm = sample();
    asm.header_overrides.push((3, 1_000_000)); // num_statements
    assert!(matches!(
        Program::parse(&asm.build(ProgsFormat::V6)).unwrap_err(),
        LoadError::SectionOutOfBounds("statement")
    ));
}

#[test]
fn unknown_secondary_version_is_loaded_as_kk7() {
    let mut asm = sample();
    asm.kk7_magic = 0xDEAD_BEEF;
    let p = Program::parse(&asm.build(ProgsFormat::Kk7)).unwrap();
    assert_eq!(p.format(), ProgsFormat::Kk7);
    assert_eq!(p.load_notes(), &[LoadNote::AssumedKk7 { secondary_version: 0xDEAD_BEEF }]);
}

#[test]
fn bodyless_function_names() {
    let mut asm = sample();
    asm.bodyless("ext_one");
    asm.bodyless("ext_two");
    let p = Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap();
    let names: Vec<_> = p.bodyless_functions().collect();
    assert_eq!(names, [&b"ext_one"[..], b"ext_two"]);
}

#[test]
fn function_lookup_prefers_function_globals() {
    let mut asm = Asm::new();
    let a = asm.function("a", &[], 0);
    asm.emit(Op::Done, 0, 0, 0);
    let b = asm.function("b", &[], 0);
    asm.emit(Op::Done, 0, 0, 0);
    // A `var` entry point named "entry" whose value is function b.
    asm.global("entry", ty::FUNCTION, &[b.index]);
    let e = asm.function("entry", &[], 0);
    asm.emit(Op::Done, 0, 0, 0);
    let p = Program::parse(&asm.build(ProgsFormat::V6)).unwrap();
    assert_eq!(p.function_index("a"), Some(a.index));
    assert_eq!(p.function_index("entry"), Some(b.index));
    assert_ne!(e.index, b.index);
    assert_eq!(p.function_index("missing"), None);
}

#[test]
fn line_number_files() {
    let asm = sample();
    let p = Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap();
    let words = |counts: [u32; 4], lines: u32| {
        let mut v: Vec<u8> = b"LNOF".to_vec();
        v.extend_from_slice(&1u32.to_le_bytes());
        for c in counts {
            v.extend_from_slice(&c.to_le_bytes());
        }
        for i in 0..lines {
            v.extend_from_slice(&(100 + i).to_le_bytes());
        }
        v
    };
    let gdefs = p.global_defs().count() as u32;
    let fdefs = p.field_defs().count() as u32;
    let counts = [gdefs, p.num_globals(), fdefs, p.num_statements()];
    let with = p.clone().with_line_numbers(&words(counts, p.num_statements())).unwrap();
    assert_eq!(with.source_line(2), Some(102));
    assert!(with.disassemble(2).to_string().contains("; line"));

    let mut wrong = counts;
    wrong[1] += 1;
    assert!(p.clone().with_line_numbers(&words(wrong, p.num_statements())).is_err());
    assert!(p.clone().with_line_numbers(&words(counts, 1)).is_err());
    assert!(p.clone().with_line_numbers(b"nope").is_err());
}

/// `called_builtins` finds the builtins the code calls, not merely the ones it declares; calls
/// through variables that only receive a builtin at run time are not visible.
#[test]
fn called_builtins_are_found_without_disassembling() {
    let mut asm = Asm::new();
    let used = asm.builtin("used", 1, 0);
    let also = asm.builtin("also", 0, 0); // bound by name
    let unused = asm.builtin("unused", 3, 0);
    let late = asm.builtin("late", 4, 0);
    let _ = (unused, late);
    let used_g = asm.global("used_g", ty::FUNCTION, &[used]);
    let also_g = asm.global("also_g", ty::FUNCTION, &[also]);
    let var_g = asm.global("var_g", ty::FUNCTION, &[0]);
    let qc = asm.function("qc", &[], 0);
    asm.emit(Op::Done, 0, 0, 0);
    let qc_g = asm.global("qc_g", ty::FUNCTION, &[qc.index]);
    asm.function("main", &[], 0);
    asm.emit(Op::Call0, used_g, 0, 0);
    asm.emit(Op::Call1H, also_g, 0, 0);
    asm.emit(Op::Call0, used_g, 0, 0);
    asm.emit(Op::Call0, qc_g, 0, 0);
    asm.emit(Op::Call0, var_g, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);
    for format in ALL_FORMATS {
        let p = Program::parse(&asm.build(format)).unwrap();
        assert_eq!(p.called_builtins(), [used, also], "{format:?}");
    }
}

/// Definition names may overlap in the string table, so their total size is not bounded by the
/// file size: every suffix of one long string is a distinct name. The loader rejects programs
/// whose names add up to more than it is willing to index, without scanning them all.
#[test]
fn overlapping_names_are_bounded() {
    let mut asm = Asm::new();
    let long = asm.string("a".repeat(20_000));
    for k in 0..20_000 {
        asm.def_global_at(long + k, ty::FLOAT, 0);
    }
    let data = asm.build(ProgsFormat::Fte32);
    assert!(data.len() < 400_000);
    let started = std::time::Instant::now();
    assert_eq!(Program::parse(&data).unwrap_err(), LoadError::NamesTooLarge);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));

    // A few hundred overlapping names are fine.
    let mut asm = Asm::new();
    let long = asm.string("b".repeat(300));
    for k in 0..300 {
        asm.def_global_at(long + k, ty::FLOAT, 0);
    }
    let p = Program::parse(&asm.build(ProgsFormat::Fte32)).unwrap();
    assert_eq!(p.global_name_at(0), None);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Corrupted programs never panic the loader or the disassembler.
    #[test]
    fn corrupted_programs_never_panic(
        format in prop::sample::select(ALL_FORMATS.to_vec()),
        flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..24),
        truncate in any::<prop::sample::Index>(),
        do_truncate in any::<bool>(),
    ) {
        let mut data = sample().build(format);
        for (at, value) in flips {
            let i = at.index(data.len());
            data[i] = value;
        }
        if do_truncate {
            data.truncate(truncate.index(data.len()));
        }
        if let Ok(p) = Program::parse(&data) {
            for i in 0..p.num_functions().min(64) {
                let _ = p.disassemble(i).to_string();
            }
            let _ = p.autocvars().count();
        }
    }
}

#[test]
fn ktx_csprogs() {
    let Some(path) = tools::csprogs() else {
        tools::skip("ktx_csprogs", "QCVM_CSPROGS");
        return;
    };
    let data = std::fs::read(&path).unwrap();
    let p = Program::parse(&data).unwrap();
    assert_eq!(p.format(), ProgsFormat::Fte16);
    // The CRC covers the engine-defined system globals and fields, so it is stable across
    // rebuilds of the mod; the sizes below are only sanity bounds, as the mod keeps changing.
    assert_eq!(p.crc(), 22390);
    assert!(p.num_globals() > 1000 && p.num_functions() > 400 && p.num_statements() > 1000);
    assert!(p.entity_fields() > 100);
    assert!(p.load_notes().is_empty(), "{:?}", p.load_notes());

    let qc: Vec<_> =
        p.functions().filter(|f| matches!(f.kind, FunctionKind::QuakeC { .. })).collect();
    assert!(qc.len() > 50);
    for f in &qc {
        let text = p.disassemble(f.index).to_string();
        assert!(!text.contains("BAD"), "{text}");
    }
    assert!(p.function_index("CSQC_UpdateView").is_some());
    assert!(p.autocvars().any(|c| c.name.starts_with(b"cl_")));

    let lno = path.with_extension("lno");
    if let Ok(lno) = std::fs::read(lno) {
        let p = p.with_line_numbers(&lno).unwrap();
        let entry = p.function_index("CSQC_Init").unwrap();
        let FunctionKind::QuakeC { entry } = p.function(entry).unwrap().kind else { panic!() };
        assert!(p.source_line(entry).is_some_and(|l| l > 0));
    }
}
