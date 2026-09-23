// SPDX-License-Identifier: MIT OR Apache-2.0

//! A human-readable listing of a function's statements.

use std::fmt;

use super::{FunctionKind, Program};
use crate::bytes::usize_from;
use crate::opcode::Operand;

/// The disassembly of one function; format it with `{}`.
#[derive(Clone, Copy, Debug)]
pub struct Disassembly<'a> {
    program: &'a Program,
    index: u32,
}

impl<'a> Disassembly<'a> {
    pub(super) fn new(program: &'a Program, index: u32) -> Self {
        Self { program, index }
    }

    /// The statement range of a QuakeC function: from its entry up to the next function's entry.
    fn range(&self, entry: u32) -> (u32, u32) {
        let end = self
            .program
            .functions
            .iter()
            .filter_map(|f| match f.kind {
                FunctionKind::QuakeC { entry: e } if e > entry => Some(e),
                _ => None,
            })
            .min()
            .unwrap_or(self.program.file_statements);
        (entry, end)
    }

    fn global(&self, f: &mut fmt::Formatter<'_>, byte_offset: u32) -> fmt::Result {
        let word = byte_offset / 4;
        match self.program.global_name_at(word) {
            Some(name) if !name.is_empty() && name != b"IMMEDIATE" => {
                write!(f, "{}", String::from_utf8_lossy(name))
            }
            _ => {
                let value = self.program.initial_global(word).unwrap_or(0);
                write!(f, "g{word}")?;
                if value != 0 {
                    write!(f, "(={value:#x})")?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Display for Disassembly<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(info) = self.program.function(self.index) else {
            return writeln!(f, "; no function {}", self.index);
        };
        write!(f, "; function {} {}", self.index, String::from_utf8_lossy(info.name))?;
        if !info.file.is_empty() {
            write!(f, " ({})", String::from_utf8_lossy(info.file))?;
        }
        writeln!(f)?;
        let FunctionKind::QuakeC { entry } = info.kind else {
            return writeln!(f, ";   {:?}", info.kind);
        };
        writeln!(
            f,
            ";   parm_start {} locals {} parms {} sizes {:?}",
            info.parm_start,
            info.locals,
            info.num_parms,
            info.parm_sizes.get(..usize::try_from(info.num_parms.clamp(0, 8)).unwrap_or(0))
        )?;
        let (start, end) = self.range(entry);
        for index in start..end {
            let Some(stmt) = self.program.statements.get(usize_from(index)) else {
                break;
            };
            write!(f, "{index:6}: {:<14}", stmt.op.name())?;
            let mut first = true;
            for (kind, value) in stmt.op.operands().iter().zip([stmt.a, stmt.b, stmt.c]) {
                if *kind == Operand::Unused {
                    continue;
                }
                f.write_str(if first { " " } else { ", " })?;
                first = false;
                match kind {
                    Operand::Global => self.global(f, value)?,
                    Operand::GlobalIndex => write!(f, "&g{value}")?,
                    Operand::Jump => write!(f, "-> {value}")?,
                    Operand::Immediate | Operand::Unused => write!(f, "{value}")?,
                }
            }
            if let Some(line) = self.program.source_line(index) {
                write!(f, "    ; line {line}")?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}
