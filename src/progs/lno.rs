// SPDX-License-Identifier: MIT OR Apache-2.0

//! fteqcc's `.lno` files: per-statement source line numbers, written next to the `.dat`.
//!
//! Layout: the magic `"LNOF"`, version 1, the program's global-definition, global, field-definition
//! and statement counts (all little-endian `u32`), then one `u32` line number per statement.

use super::{LoadError, Program};
use crate::bytes::{u32_at, usize_from};

const MAGIC: u32 = u32::from_le_bytes(*b"LNOF");
const HEADER_WORDS: usize = 6;

pub(super) fn attach(mut program: Program, lno: &[u8]) -> Result<Program, LoadError> {
    let word =
        |i: usize| u32_at(lno, i.saturating_mul(4)).ok_or(LoadError::LineNumbers("truncated"));
    if word(0)? != MAGIC {
        return Err(LoadError::LineNumbers("not a .lno file"));
    }
    if word(1)? != 1 {
        return Err(LoadError::LineNumbers("unsupported .lno version"));
    }
    let expect = [
        program.global_defs.len(),
        program.globals.len(),
        program.field_defs.len(),
        usize_from(program.file_statements),
    ];
    for (i, want) in expect.into_iter().enumerate() {
        if usize_from(word(i.saturating_add(2))?) != want {
            return Err(LoadError::LineNumbers("counts do not match the program"));
        }
    }
    let lines = lno
        .get(HEADER_WORDS.saturating_mul(4)..)
        .unwrap_or_default()
        .chunks_exact(4)
        .take(usize_from(program.file_statements))
        .map(|c| u32_at(c, 0).unwrap_or(0))
        .collect::<Box<[u32]>>();
    if lines.len() != usize_from(program.file_statements) {
        return Err(LoadError::LineNumbers("truncated"));
    }
    program.line_numbers = Some(lines);
    Ok(program)
}
