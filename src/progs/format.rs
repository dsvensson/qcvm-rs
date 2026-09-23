// SPDX-License-Identifier: MIT OR Apache-2.0

//! The on-disk layouts: headers and the raw records of every supported format.

use super::{LoadError, LoadNote, ProgsFormat};
use crate::bytes::{u16_at, u32_at, usize_from};

/// FTE's secondary version for 16-bit version-7 progs.
pub(crate) const FTE16_MAGIC: u32 = 0x021B_1461;
/// FTE's secondary version for 32-bit version-7 progs.
pub(crate) const FTE32_MAGIC: u32 = 0x6516_7402;
/// uHexen2's secondary version (`"UH27"`).
pub(crate) const UHEXEN2_MAGIC: u32 = u32::from_le_bytes(*b"UH27");
/// KK QuakeWorld server's secondary version (`"KKQW"`).
pub(crate) const KK7_MAGIC: u32 = u32::from_le_bytes(*b"KKQW");

/// The version-6 header fields, shared by every format.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Header {
    pub(crate) version: u32,
    pub(crate) crc: u32,
    pub(crate) ofs_statements: u32,
    pub(crate) num_statements: u32,
    pub(crate) ofs_globaldefs: u32,
    pub(crate) num_globaldefs: u32,
    pub(crate) ofs_fielddefs: u32,
    pub(crate) num_fielddefs: u32,
    pub(crate) ofs_functions: u32,
    pub(crate) num_functions: u32,
    pub(crate) ofs_strings: u32,
    pub(crate) len_strings: u32,
    pub(crate) ofs_globals: u32,
    pub(crate) num_globals: u32,
    pub(crate) entity_fields: u32,
}

/// The extra fields of a version-7 header that the loader uses (the header also holds the
/// offsets of the file and type tables, which nothing needs, and the secondary version, which
/// selects the [`ProgsFormat`]).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ExtHeader {
    pub(crate) ofs_linenums: u32,
    pub(crate) ofs_bodylessfuncs: u32,
    pub(crate) num_bodylessfuncs: u32,
    pub(crate) num_types: u32,
    pub(crate) blockscompressed: u32,
}

fn fields<const N: usize>(data: &[u8], start: usize) -> Result<[u32; N], LoadError> {
    let mut out = [0u32; N];
    for (i, slot) in out.iter_mut().enumerate() {
        let off =
            i.checked_mul(4).and_then(|o| o.checked_add(start)).ok_or(LoadError::Truncated)?;
        *slot = u32_at(data, off).ok_or(LoadError::Truncated)?;
    }
    Ok(out)
}

/// Reads the header and determines the format.
pub(crate) fn read_header(
    data: &[u8],
    notes: &mut Vec<LoadNote>,
) -> Result<(Header, Option<ExtHeader>, ProgsFormat), LoadError> {
    let [
        version,
        crc,
        ofs_statements,
        num_statements,
        ofs_globaldefs,
        num_globaldefs,
        ofs_fielddefs,
        num_fielddefs,
        ofs_functions,
        num_functions,
        ofs_strings,
        len_strings,
        ofs_globals,
        num_globals,
        entity_fields,
    ] = fields::<15>(data, 0)?;
    let header = Header {
        version,
        crc,
        ofs_statements,
        num_statements,
        ofs_globaldefs,
        num_globaldefs,
        ofs_fielddefs,
        num_fielddefs,
        ofs_functions,
        num_functions,
        ofs_strings,
        len_strings,
        ofs_globals,
        num_globals,
        entity_fields,
    };
    match version {
        3 => Ok((header, None, ProgsFormat::QTest)),
        6 => Ok((header, None, ProgsFormat::V6)),
        7 => {
            let [
                _ofs_files,
                ofs_linenums,
                ofs_bodylessfuncs,
                num_bodylessfuncs,
                _ofs_types,
                num_types,
                blockscompressed,
                secondary_version,
            ] = fields::<8>(data, 60)?;
            let ext = ExtHeader {
                ofs_linenums,
                ofs_bodylessfuncs,
                num_bodylessfuncs,
                num_types,
                blockscompressed,
            };
            let format = match secondary_version {
                FTE16_MAGIC => ProgsFormat::Fte16,
                FTE32_MAGIC => ProgsFormat::Fte32,
                UHEXEN2_MAGIC => ProgsFormat::UHexen2,
                KK7_MAGIC => ProgsFormat::Kk7,
                other => {
                    notes.push(LoadNote::AssumedKk7 { secondary_version: other });
                    ProgsFormat::Kk7
                }
            };
            if matches!(format, ProgsFormat::Fte16 | ProgsFormat::Fte32) {
                if ext.blockscompressed != 0 {
                    return Err(LoadError::Compressed(ext.blockscompressed));
                }
                Ok((header, Some(ext), format))
            } else {
                // KK7 and uHexen2 are treated as version 6 with wider records; FTE ignores their
                // extension fields.
                Ok((header, None, format))
            }
        }
        other => Err(LoadError::UnsupportedVersion(other)),
    }
}

/// Record sizes in bytes for a format.
pub(crate) struct Layout {
    pub(crate) statement: usize,
    pub(crate) def: usize,
    pub(crate) function: usize,
}

pub(crate) const fn layout(format: ProgsFormat) -> Layout {
    match format {
        ProgsFormat::QTest => Layout { statement: 12, def: 12, function: 64 },
        ProgsFormat::V6 | ProgsFormat::Fte16 => Layout { statement: 8, def: 8, function: 36 },
        ProgsFormat::Fte32 | ProgsFormat::UHexen2 => {
            Layout { statement: 16, def: 12, function: 36 }
        }
        ProgsFormat::Kk7 => Layout { statement: 16, def: 8, function: 36 },
    }
}

/// Returns the bytes of a section of `count` records of `size` bytes at `ofs`.
pub(crate) fn section<'a>(
    data: &'a [u8],
    ofs: u32,
    count: u32,
    size: usize,
    name: &'static str,
) -> Result<&'a [u8], LoadError> {
    if count == 0 {
        return Ok(&[]);
    }
    let len = usize_from(count).checked_mul(size).ok_or(LoadError::SectionOutOfBounds(name))?;
    let start = usize_from(ofs);
    let end = start.checked_add(len).ok_or(LoadError::SectionOutOfBounds(name))?;
    data.get(start..end).ok_or(LoadError::SectionOutOfBounds(name))
}

/// A statement as stored in the file.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RawStatement {
    pub(crate) op: u32,
    pub(crate) a: u32,
    pub(crate) b: u32,
    pub(crate) c: u32,
    /// QTest stores a source line with every statement.
    pub(crate) line: Option<u32>,
}

pub(crate) fn decode_statement(format: ProgsFormat, rec: &[u8]) -> RawStatement {
    let w16 = |off| u32::from(u16_at(rec, off).unwrap_or(0));
    let w32 = |off| u32_at(rec, off).unwrap_or(0);
    match format {
        ProgsFormat::QTest => {
            RawStatement { op: w16(4), a: w16(6), b: w16(8), c: w16(10), line: Some(w32(0)) }
        }
        ProgsFormat::V6 | ProgsFormat::Fte16 => {
            RawStatement { op: w16(0), a: w16(2), b: w16(4), c: w16(6), line: None }
        }
        ProgsFormat::Fte32 | ProgsFormat::Kk7 => {
            RawStatement { op: w32(0), a: w32(4), b: w32(8), c: w32(12), line: None }
        }
        ProgsFormat::UHexen2 => {
            RawStatement { op: w32(0) >> 16, a: w32(4), b: w32(8), c: w32(12), line: None }
        }
    }
}

/// A definition as stored in the file.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RawDef {
    pub(crate) ty: u32,
    pub(crate) ofs: u32,
    pub(crate) name: u32,
}

pub(crate) fn decode_def(format: ProgsFormat, rec: &[u8]) -> RawDef {
    let w16 = |off| u32::from(u16_at(rec, off).unwrap_or(0));
    let w32 = |off| u32_at(rec, off).unwrap_or(0);
    match format {
        ProgsFormat::QTest => RawDef { ty: w32(0), name: w32(4), ofs: w32(8) },
        ProgsFormat::V6 | ProgsFormat::Fte16 | ProgsFormat::Kk7 => {
            RawDef { ty: w16(0), ofs: w16(2), name: w32(4) }
        }
        ProgsFormat::Fte32 => RawDef { ty: w32(0), ofs: w32(4), name: w32(8) },
        ProgsFormat::UHexen2 => RawDef { ty: w32(0) >> 16, ofs: w32(4), name: w32(8) },
    }
}

/// A function record as stored in the file.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RawFunction {
    pub(crate) first_statement: i32,
    pub(crate) parm_start: u32,
    pub(crate) locals: u32,
    pub(crate) name: u32,
    pub(crate) file: u32,
    pub(crate) num_parms: i32,
    pub(crate) parm_sizes: [u8; 8],
}

pub(crate) fn decode_function(format: ProgsFormat, rec: &[u8]) -> RawFunction {
    let w32 = |off| u32_at(rec, off).unwrap_or(0);
    let mut parm_sizes = [0u8; 8];
    match format {
        ProgsFormat::QTest => {
            // first_statement, unused, locals, profile, name, file, numparms, parm_start,
            // parm_size[8] as 32-bit ints.
            for (i, size) in parm_sizes.iter_mut().enumerate() {
                let off = i.saturating_mul(4).saturating_add(32);
                *size = u8::try_from(w32(off)).unwrap_or(u8::MAX);
            }
            RawFunction {
                first_statement: w32(0).cast_signed(),
                parm_start: w32(28),
                locals: w32(8),
                name: w32(16),
                file: w32(20),
                num_parms: w32(24).cast_signed(),
                parm_sizes,
            }
        }
        _ => {
            if let Some(sizes) = rec.get(28..36) {
                parm_sizes.copy_from_slice(sizes);
            }
            RawFunction {
                first_statement: w32(0).cast_signed(),
                parm_start: w32(4),
                locals: w32(8),
                name: w32(16),
                file: w32(20),
                num_parms: w32(24).cast_signed(),
                parm_sizes,
            }
        }
    }
}
