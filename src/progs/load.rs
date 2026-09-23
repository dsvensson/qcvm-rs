// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conversion of raw records into the canonical, validated program.

use std::collections::HashMap;

use super::format::{
    self, RawFunction, RawStatement, decode_def, decode_function, decode_statement, section,
};
use super::{
    Def, Function, FunctionKind, InvalidFunction, LoadError, LoadNote, ParamCopy, Program,
    ProgsFormat, STMT_BREAKPOINT, SpecialGlobals, Stmt, Type,
};
use crate::bytes::{cstr_at, u32_at, usize_from};
use crate::opcode::{Op, Operand};

/// The breakpoint flag in a stored opcode.
const BREAKPOINT_BIT: u32 = 0x8000;

/// `CALLn` + this = `CALLnH`.
const HEXEN2_CALL_DELTA: u32 = (Op::Call1H as u32).wrapping_sub(Op::Call1 as u32);

/// Globals are followed by three zero words; operands may name those, as in FTE.
const GLOBALS_TAIL: u64 = 3;

pub(super) fn parse(data: &[u8]) -> Result<Program, LoadError> {
    let mut notes = Vec::new();
    let (h, ext, format) = format::read_header(data, &mut notes)?;
    let layout = format::layout(format);

    let stmt_bytes =
        section(data, h.ofs_statements, h.num_statements, layout.statement, "statement")?;
    let gdef_bytes =
        section(data, h.ofs_globaldefs, h.num_globaldefs, layout.def, "global definition")?;
    let fdef_bytes =
        section(data, h.ofs_fielddefs, h.num_fielddefs, layout.def, "field definition")?;
    let func_bytes = section(data, h.ofs_functions, h.num_functions, layout.function, "function")?;
    let strings = section(data, h.ofs_strings, h.len_strings, 1, "string")?;
    let glob_bytes = section(data, h.ofs_globals, h.num_globals, 4, "globals")?;

    let globals: Box<[u32]> =
        glob_bytes.chunks_exact(4).map(|c| u32_at(c, 0).unwrap_or(0)).collect();
    let limit = u64::from(h.num_globals).saturating_add(GLOBALS_TAIL);

    let mut raw: Vec<RawStatement> =
        stmt_bytes.chunks_exact(layout.statement).map(|r| decode_statement(format, r)).collect();

    if uses_hexen2_calls(format, &raw) {
        notes.push(LoadNote::Hexen2Calls);
        rewrite_hexen2_calls(&mut raw);
    }

    let (statements, qtest_lines) = canonical_statements(&raw, format, limit, &mut notes);
    let file_statements = h.num_statements;

    let global_defs = decode_defs(gdef_bytes, format, layout.def);
    let field_defs = decode_defs(fdef_bytes, format, layout.def);

    let mut param_copies = Vec::new();
    let functions: Box<[Function]> = func_bytes
        .chunks_exact(layout.function)
        .enumerate()
        .map(|(index, rec)| {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            let raw = decode_function(format, rec);
            let f = canonical_function(index, &raw, file_statements, limit, &mut param_copies);
            if let FunctionKind::Invalid(reason) = f.kind {
                notes.push(LoadNote::InvalidFunction { index, reason });
            }
            f
        })
        .collect();

    // Everything below scans, hashes and copies names; bound that work first.
    let names = functions.iter().map(|f| f.name);
    check_name_bytes(
        names.chain(global_defs.iter().map(|d| d.name)).chain(field_defs.iter().map(|d| d.name)),
        strings,
    )?;

    let functions_by_name =
        name_map(functions.iter().enumerate().skip(1).map(|(i, f)| (i, f.name)), strings);
    let globals_by_name =
        name_map(global_defs.iter().enumerate().map(|(i, d)| (i, d.name)), strings);
    let fields_by_name = name_map(field_defs.iter().enumerate().map(|(i, d)| (i, d.name)), strings);

    let mut global_name_by_ofs = HashMap::new();
    for (i, d) in global_defs.iter().enumerate() {
        if !cstr_at(strings, usize_from(d.name)).is_empty() {
            global_name_by_ofs.entry(d.ofs).or_insert(u32::try_from(i).unwrap_or(u32::MAX));
        }
    }

    let mut line_numbers = qtest_lines;
    let mut bodyless = Vec::new();
    if let Some(ext) = ext {
        if ext.ofs_linenums != 0 {
            match section(data, ext.ofs_linenums, h.num_statements, 4, "line number") {
                Ok(bytes) => {
                    line_numbers =
                        Some(bytes.chunks_exact(4).map(|c| u32_at(c, 0).unwrap_or(0)).collect());
                }
                Err(_) => notes.push(LoadNote::LineNumbersIgnored),
            }
        }
        if ext.num_bodylessfuncs != 0 {
            bodyless =
                read_bodyless(data, ext.ofs_bodylessfuncs, ext.num_bodylessfuncs, &mut notes);
        }
        if ext.num_types != 0 {
            notes.push(LoadNote::TypesIgnored);
        }
    }

    let pointer_relocs = global_defs
        .iter()
        .filter(|d| d.ty == Type::Pointer)
        .filter(|d| globals.get(usize_from(d.ofs)).is_some_and(|v| v & 0x8000_0000 != 0))
        .map(|d| d.ofs)
        .collect();

    let def_ofs = |name: &[u8]| {
        let index = *globals_by_name.get(name)?;
        global_defs.get(usize_from(index)).map(|d| d.ofs)
    };
    let special = SpecialGlobals {
        thisprogs: def_ofs(b"thisprogs"),
        fasttrackarrays: def_ofs(b"__ext__fasttrackarrays"),
    };

    Ok(Program {
        format,
        version: h.version,
        crc: h.crc,
        strings: strings.into(),
        globals,
        statements,
        file_statements,
        functions,
        param_copies: param_copies.into(),
        global_defs,
        field_defs,
        entity_fields: h.entity_fields,
        functions_by_name,
        globals_by_name,
        fields_by_name,
        global_name_by_ofs,
        bodyless: bodyless.into(),
        pointer_relocs,
        special,
        line_numbers,
        notes,
    })
}

/// FTE's rule: a program uses the Hexen 2 calling convention if any `CALL1`..`CALL8` passes
/// something in operand `b` (uHexen2 progs always do).
fn uses_hexen2_calls(format: ProgsFormat, raw: &[RawStatement]) -> bool {
    match format {
        ProgsFormat::QTest => false,
        ProgsFormat::UHexen2 => true,
        _ => raw.iter().any(|s| (52..=59).contains(&(s.op & !BREAKPOINT_BIT)) && s.b != 0),
    }
}

/// `CALLn` becomes `CALLnH`, and `RAND*` results default to the return slot.
fn rewrite_hexen2_calls(raw: &mut [RawStatement]) {
    for s in raw {
        let bp = s.op & BREAKPOINT_BIT;
        let op = s.op & !BREAKPOINT_BIT;
        if (52..=59).contains(&op) {
            s.op = bp | op.saturating_add(HEXEN2_CALL_DELTA);
        }
        if (92..=97).contains(&op) && s.c == 0 {
            s.c = 1;
        }
    }
}

/// Decodes, sanitizes and relocates every statement, then appends the out-of-range sentinel.
fn canonical_statements(
    raw: &[RawStatement],
    format: ProgsFormat,
    limit: u64,
    notes: &mut Vec<LoadNote>,
) -> (Box<[Stmt]>, Option<Box<[u32]>>) {
    let count = raw.len();
    let sentinel = u32::try_from(count).unwrap_or(u32::MAX);
    let wide = !format.has_16bit_statements();
    let mut statements = Vec::with_capacity(count.saturating_add(1));
    let mut lines = Vec::new();

    for (index, s) in raw.iter().enumerate() {
        let index_u32 = u32::try_from(index).unwrap_or(u32::MAX);
        if let Some(line) = s.line {
            lines.push(line);
        }
        let flags = if s.op & BREAKPOINT_BIT != 0 { STMT_BREAKPOINT } else { 0 };
        let number = s.op & !BREAKPOINT_BIT;
        let op = Op::from_u32(number);
        let mut ok = op != Op::Bad;
        let mut out = [0u32; 3];
        let mut jump_out = false;

        for ((kind, value), slot) in op.operands().iter().zip([s.a, s.b, s.c]).zip(out.iter_mut()) {
            match kind {
                Operand::Global | Operand::Unused => {
                    match value.checked_mul(4).filter(|_| u64::from(value) < limit) {
                        Some(byte) => *slot = byte,
                        None => ok = false,
                    }
                }
                Operand::GlobalIndex => {
                    if u64::from(value) < limit {
                        *slot = value;
                    } else {
                        ok = false;
                    }
                }
                Operand::Jump => {
                    let offset = if wide {
                        i64::from(value.cast_signed())
                    } else {
                        i64::from((value as u16).cast_signed())
                    };
                    let target = i64::try_from(index).unwrap_or(i64::MAX).saturating_add(offset);
                    match u32::try_from(target) {
                        Ok(t) if usize_from(t) < count => *slot = t,
                        _ => {
                            *slot = sentinel;
                            jump_out = true;
                        }
                    }
                }
                Operand::Immediate => *slot = value,
            }
        }

        let [a, b, c] = out;
        if ok {
            if jump_out {
                notes.push(LoadNote::JumpOutOfRange { index: index_u32 });
            }
            statements.push(Stmt { op, flags, a, b, c });
        } else {
            notes.push(LoadNote::PoisonedStatement { index: index_u32, opcode: number });
            statements.push(Stmt { op: Op::Bad, flags, a: 0, b: 0, c: 0 });
        }
    }
    statements.push(Stmt { op: Op::JumpOutOfRange, flags: 0, a: 0, b: 0, c: 0 });

    let lines = (format == ProgsFormat::QTest).then(|| lines.into_boxed_slice());
    (statements.into_boxed_slice(), lines)
}

fn decode_defs(bytes: &[u8], format: ProgsFormat, size: usize) -> Box<[Def]> {
    bytes
        .chunks_exact(size)
        .map(|r| {
            let d = decode_def(format, r);
            Def {
                ty: Type::from_code(d.ty & !0xC000),
                ofs: d.ofs,
                name: d.name,
                save: d.ty & 0x8000 != 0,
                shared: d.ty & 0x4000 != 0,
            }
        })
        .collect()
}

/// Validates a function record and plans its parameter copies.
fn canonical_function(
    index: u32,
    raw: &RawFunction,
    num_statements: u32,
    limit: u64,
    copies: &mut Vec<ParamCopy>,
) -> Function {
    let copies_start = u32::try_from(copies.len()).unwrap_or(u32::MAX);
    let kind = if index == 0 {
        FunctionKind::Null
    } else if raw.first_statement > 0 {
        let entry = raw.first_statement.unsigned_abs();
        if entry >= num_statements {
            FunctionKind::Invalid(InvalidFunction::EntryOutOfRange)
        } else if plan_param_copies(raw, limit, copies) {
            FunctionKind::QuakeC { entry }
        } else {
            copies.truncate(usize_from(copies_start));
            FunctionKind::Invalid(InvalidFunction::LocalsOutOfRange)
        }
    } else if raw.first_statement == 0 {
        FunctionKind::NamedBuiltin
    } else {
        FunctionKind::Builtin { number: raw.first_statement.unsigned_abs() }
    };
    Function {
        kind,
        parm_start: raw.parm_start,
        locals: raw.locals,
        num_parms: raw.num_parms,
        parm_sizes: raw.parm_sizes,
        copies_start,
        copies_end: u32::try_from(copies.len()).unwrap_or(u32::MAX),
        name: raw.name,
        file: raw.file,
    }
}

/// Parameter `i` is copied word by word from PARM slot `i` (global `4 + 3i`) into consecutive
/// locals starting at `parm_start`. At most eight parameters are passed this way.
fn plan_param_copies(raw: &RawFunction, limit: u64, copies: &mut Vec<ParamCopy>) -> bool {
    let start = u64::from(raw.parm_start);
    if start.saturating_add(u64::from(raw.locals)) > limit {
        return false;
    }
    let num_parms = usize::try_from(raw.num_parms.clamp(0, 8)).unwrap_or(0);
    let mut dst = start;
    for (slot, &size) in raw.parm_sizes.iter().take(num_parms).enumerate() {
        let slot_base = u64::try_from(slot).unwrap_or(0).saturating_mul(3).saturating_add(4);
        for word in 0..u64::from(size) {
            let src = slot_base.saturating_add(word);
            if src >= limit || dst >= limit {
                return false;
            }
            let (Ok(src), Ok(dst_u32)) = (u32::try_from(src), u32::try_from(dst)) else {
                return false;
            };
            let (Some(src), Some(dst_byte)) = (src.checked_mul(4), dst_u32.checked_mul(4)) else {
                return false;
            };
            copies.push(ParamCopy { src, dst: dst_byte });
            dst = dst.saturating_add(1);
        }
    }
    true
}

/// Most bytes the names of a program's functions and definitions may add up to. Names are
/// string-table offsets and may overlap (fteqcc shares common suffixes), so a small file could
/// otherwise make the loader copy and hash quadratically many bytes.
const NAME_BYTES_MAX: usize = 16 << 20;

/// Checks that the names at `offsets` add up to at most [`NAME_BYTES_MAX`] bytes, scanning no
/// more than that.
fn check_name_bytes(offsets: impl Iterator<Item = u32>, strings: &[u8]) -> Result<(), LoadError> {
    let mut left = NAME_BYTES_MAX;
    for ofs in offsets {
        let rest = strings.get(usize_from(ofs)..).unwrap_or_default();
        let window = rest.get(..rest.len().min(left.saturating_add(1))).unwrap_or_default();
        let len = match window.iter().position(|&b| b == 0) {
            Some(n) => n,
            None if window.len() == rest.len() => rest.len(),
            None => return Err(LoadError::NamesTooLarge),
        };
        left = left.checked_sub(len).ok_or(LoadError::NamesTooLarge)?;
    }
    Ok(())
}

/// Maps non-empty names to the index of their first occurrence.
fn name_map(
    entries: impl Iterator<Item = (usize, u32)>,
    strings: &[u8],
) -> HashMap<Box<[u8]>, u32> {
    let mut map = HashMap::new();
    for (index, name) in entries {
        let name = cstr_at(strings, usize_from(name));
        if !name.is_empty() {
            map.entry(name.into()).or_insert(u32::try_from(index).unwrap_or(u32::MAX));
        }
    }
    map
}

/// Reads the NUL-separated names of the bodyless-function section.
fn read_bodyless(data: &[u8], ofs: u32, count: u32, notes: &mut Vec<LoadNote>) -> Vec<Box<[u8]>> {
    let mut names = Vec::new();
    let Some(mut rest) = data.get(usize_from(ofs)..) else {
        notes.push(LoadNote::BodylessIgnored);
        return names;
    };
    for _ in 0..count {
        let Some(end) = rest.iter().position(|&b| b == 0) else {
            notes.push(LoadNote::BodylessIgnored);
            break;
        };
        let (name, tail) = rest.split_at(end);
        names.push(name.into());
        rest = tail.get(1..).unwrap_or_default();
    }
    names
}
