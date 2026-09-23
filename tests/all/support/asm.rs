// SPDX-License-Identifier: MIT OR Apache-2.0

//! A small assembler that writes progs files in every format `qcvm` loads.
//!
//! Tests build programs instruction by instruction, so opcode behaviour can be checked without a
//! QuakeC compiler. Globals are laid out like fteqcc does: 28 reserved words (null, return, eight
//! parameter slots), then everything else.

use std::collections::HashMap;

use qcvm::{Op, ProgsFormat};

/// `EV_*` type codes.
pub mod ty {
    pub const VOID: u32 = 0;
    pub const STRING: u32 = 1;
    pub const FLOAT: u32 = 2;
    pub const VECTOR: u32 = 3;
    pub const ENTITY: u32 = 4;
    pub const FIELD: u32 = 5;
    pub const FUNCTION: u32 = 6;
    pub const POINTER: u32 = 7;
    pub const INTEGER: u32 = 8;
}

/// Reserved global offsets.
pub const OFS_RETURN: u32 = 1;
pub const OFS_PARM0: u32 = 4;

/// The global word of parameter slot `i`.
pub const fn parm(i: u32) -> u32 {
    OFS_PARM0 + 3 * i
}

#[derive(Clone, Debug)]
struct FunctionRecord {
    first_statement: i32,
    parm_start: u32,
    locals: u32,
    name: u32,
    file: u32,
    num_parms: i32,
    parm_sizes: [u8; 8],
}

#[derive(Clone, Copy, Debug)]
struct DefRecord {
    ty: u32,
    ofs: u32,
    name: u32,
}

/// A function under construction.
#[derive(Clone, Copy, Debug)]
pub struct Func {
    /// Function index (the value a function global holds).
    pub index: u32,
    /// First global word of the parameters and locals.
    pub parm_start: u32,
}

impl Func {
    /// The global word of local (or parameter) word `i`.
    pub fn local(&self, i: u32) -> u32 {
        self.parm_start + i
    }
}

/// Builds a progs image.
#[derive(Clone, Debug)]
pub struct Asm {
    strings: Vec<u8>,
    string_offsets: HashMap<Vec<u8>, u32>,
    globals: Vec<u32>,
    global_defs: Vec<DefRecord>,
    field_defs: Vec<DefRecord>,
    statements: Vec<[u32; 4]>,
    functions: Vec<FunctionRecord>,
    entity_fields: u32,
    bodyless: Vec<Vec<u8>>,
    /// Extra raw header overrides applied by `build` (field index, value).
    pub header_overrides: Vec<(usize, u32)>,
    /// Secondary version to write for [`ProgsFormat::Kk7`] (defaults to `KKQW`).
    pub kk7_magic: u32,
}

impl Default for Asm {
    fn default() -> Self {
        Self::new()
    }
}

impl Asm {
    pub fn new() -> Self {
        let mut asm = Self {
            // Like fteqcc: offset 0 is the null string and offset 1 a non-null empty string.
            strings: vec![0, 0],
            string_offsets: HashMap::new(),
            globals: vec![0; 28],
            global_defs: Vec::new(),
            field_defs: Vec::new(),
            statements: Vec::new(),
            functions: Vec::new(),
            entity_fields: 0,
            bodyless: Vec::new(),
            header_overrides: Vec::new(),
            kk7_magic: u32::from_le_bytes(*b"KKQW"),
        };
        asm.string_offsets.insert(Vec::new(), 1);
        // Function 0 is the null function; statement 0 is the DONE it points at.
        asm.functions.push(FunctionRecord {
            first_statement: 0,
            parm_start: 0,
            locals: 0,
            name: 0,
            file: 0,
            num_parms: 0,
            parm_sizes: [0; 8],
        });
        asm.statements.push([Op::Done as u32, 0, 0, 0]);
        asm
    }

    /// Interns a string and returns its offset in the string table.
    pub fn string(&mut self, s: impl AsRef<[u8]>) -> u32 {
        let s = s.as_ref();
        if let Some(&ofs) = self.string_offsets.get(s) {
            return ofs;
        }
        let ofs = self.strings.len() as u32;
        self.strings.extend_from_slice(s);
        self.strings.push(0);
        self.string_offsets.insert(s.to_vec(), ofs);
        ofs
    }

    /// Allocates `words` global words initialised to `init` (zero-extended) without a definition.
    pub fn alloc(&mut self, words: u32, init: &[u32]) -> u32 {
        let ofs = self.globals.len() as u32;
        for i in 0..words as usize {
            self.globals.push(init.get(i).copied().unwrap_or(0));
        }
        ofs
    }

    /// Defines a named global and returns its word offset.
    pub fn global(&mut self, name: &str, ty: u32, init: &[u32]) -> u32 {
        let words = match ty {
            ty::VECTOR => 3,
            ty::VOID => 0,
            _ => 1,
        };
        let ofs = self.alloc(words.max(init.len() as u32), init);
        let name = self.string(name);
        self.global_defs.push(DefRecord { ty, ofs, name });
        ofs
    }

    /// Adds a global definition for an existing offset.
    pub fn def_global(&mut self, name: &str, ty: u32, ofs: u32) {
        let name = self.string(name);
        self.global_defs.push(DefRecord { ty, ofs, name });
    }

    /// Defines a global whose name is the string at offset `name` of the string table.
    pub fn def_global_at(&mut self, name: u32, ty: u32, ofs: u32) {
        self.global_defs.push(DefRecord { ty, ofs, name });
    }

    /// An unnamed float constant.
    pub fn float(&mut self, v: f32) -> u32 {
        self.alloc(1, &[v.to_bits()])
    }

    /// An unnamed integer constant.
    pub fn int(&mut self, v: i32) -> u32 {
        self.alloc(1, &[v as u32])
    }

    /// An unnamed vector constant.
    pub fn vector(&mut self, v: [f32; 3]) -> u32 {
        self.alloc(3, &[v[0].to_bits(), v[1].to_bits(), v[2].to_bits()])
    }

    /// Three unnamed words.
    pub fn vector_raw(&mut self, w: [u32; 3]) -> u32 {
        self.alloc(3, &w)
    }

    /// An unnamed string constant.
    pub fn str_const(&mut self, s: &str) -> u32 {
        let ofs = self.string(s);
        self.alloc(1, &[ofs])
    }

    /// Overwrites the initial value of global word `word`.
    pub fn set_global(&mut self, word: u32, value: u32) {
        self.globals[word as usize] = value;
    }

    /// Unnamed scratch globals.
    pub fn temp(&mut self, words: u32) -> u32 {
        self.alloc(words, &[])
    }

    /// Defines an entity field and a field-typed global holding its offset. Returns
    /// `(field word offset, global holding it)`.
    pub fn field(&mut self, name: &str, ty: u32) -> (u32, u32) {
        let words = if ty == ty::VECTOR { 3 } else { 1 };
        let ofs = self.entity_fields;
        self.entity_fields += words;
        let n = self.string(name);
        self.field_defs.push(DefRecord { ty, ofs, name: n });
        if ty == ty::VECTOR {
            for (i, suffix) in ["_x", "_y", "_z"].iter().enumerate() {
                let n = self.string(format!("{name}{suffix}"));
                self.field_defs.push(DefRecord { ty: ty::FLOAT, ofs: ofs + i as u32, name: n });
            }
        }
        let global = self.alloc(1, &[ofs]);
        let gname = self.string(name);
        self.global_defs.push(DefRecord { ty: ty::FIELD, ofs: global, name: gname });
        (ofs, global)
    }

    /// Declares a builtin (`number` 0 = resolved by name) and a function global for it.
    pub fn builtin(&mut self, name: &str, number: u32, num_parms: i32) -> u32 {
        let index = self.functions.len() as u32;
        let n = self.string(name);
        self.functions.push(FunctionRecord {
            first_statement: -(number as i32),
            parm_start: 0,
            locals: 0,
            name: n,
            file: 0,
            num_parms,
            parm_sizes: [0; 8],
        });
        let g = self.alloc(1, &[index]);
        self.global_defs.push(DefRecord { ty: ty::FUNCTION, ofs: g, name: n });
        index
    }

    /// Starts a QuakeC function whose code begins at the next emitted statement.
    ///
    /// `parm_sizes` gives the size of each parameter; `extra_locals` words follow them.
    pub fn function(&mut self, name: &str, parm_sizes: &[u8], extra_locals: u32) -> Func {
        let index = self.functions.len() as u32;
        let params: u32 = parm_sizes.iter().map(|&s| u32::from(s)).sum();
        let locals = params + extra_locals;
        let parm_start = self.alloc(locals, &[]);
        let n = self.string(name);
        let file = self.string("test.qc");
        let mut sizes = [0u8; 8];
        for (d, s) in sizes.iter_mut().zip(parm_sizes) {
            *d = *s;
        }
        self.functions.push(FunctionRecord {
            first_statement: self.statements.len() as i32,
            parm_start,
            locals,
            name: n,
            file,
            num_parms: parm_sizes.len() as i32,
            parm_sizes: sizes,
        });
        let g = self.alloc(1, &[index]);
        self.global_defs.push(DefRecord { ty: ty::FUNCTION, ofs: g, name: n });
        Func { index, parm_start }
    }

    /// Overrides fields of an already declared function record.
    pub fn patch_function(
        &mut self,
        index: u32,
        first_statement: i32,
        parm_start: u32,
        locals: u32,
    ) {
        let f = &mut self.functions[index as usize];
        f.first_statement = first_statement;
        f.parm_start = parm_start;
        f.locals = locals;
    }

    /// Emits a statement with raw operands and returns its index.
    pub fn emit(&mut self, op: Op, a: u32, b: u32, c: u32) -> u32 {
        self.emit_raw(op as u32, a, b, c)
    }

    /// Emits a statement with a raw opcode number.
    pub fn emit_raw(&mut self, op: u32, a: u32, b: u32, c: u32) -> u32 {
        self.statements.push([op, a, b, c]);
        self.statements.len() as u32 - 1
    }

    /// The index the next statement will get.
    pub fn here(&self) -> u32 {
        self.statements.len() as u32
    }

    /// Relative offset from statement `from` to `to`, as stored in a jump operand.
    pub fn rel(from: u32, to: u32) -> u32 {
        (to as i32 - from as i32) as u32
    }

    /// Rewrites operand `which` (0 = a, 1 = b, 2 = c) of statement `stmt`.
    pub fn patch(&mut self, stmt: u32, which: usize, value: u32) {
        self.statements[stmt as usize][which + 1] = value;
    }

    /// Makes the jump operand `which` of statement `stmt` point at `target`.
    pub fn patch_jump(&mut self, stmt: u32, which: usize, target: u32) {
        self.patch(stmt, which, Self::rel(stmt, target));
    }

    /// Adds a name to the bodyless-function section (FTE formats only).
    pub fn bodyless(&mut self, name: &str) {
        self.bodyless.push(name.as_bytes().to_vec());
    }

    /// Number of global words so far.
    pub fn num_globals(&self) -> u32 {
        self.globals.len() as u32
    }

    /// Writes the program in `format`.
    pub fn build(&self, format: ProgsFormat) -> Vec<u8> {
        let v7 = matches!(
            format,
            ProgsFormat::Fte16 | ProgsFormat::Fte32 | ProgsFormat::Kk7 | ProgsFormat::UHexen2
        );
        let header_len = if v7 { 92 } else { 60 };
        let mut out = vec![0u8; header_len];

        let put = |out: &mut Vec<u8>, bytes: &[u8]| {
            let ofs = out.len() as u32;
            out.extend_from_slice(bytes);
            ofs
        };

        let ofs_strings = put(&mut out, &self.strings);
        while out.len() % 4 != 0 {
            out.push(0);
        }

        let mut stmts = Vec::new();
        for (i, s) in self.statements.iter().enumerate() {
            let [op, a, b, c] = *s;
            match format {
                ProgsFormat::QTest => {
                    stmts.extend_from_slice(&(i as u32 + 1).to_le_bytes());
                    for w in [op, a, b, c] {
                        stmts.extend_from_slice(&(w as u16).to_le_bytes());
                    }
                }
                ProgsFormat::V6 | ProgsFormat::Fte16 => {
                    for w in [op, a, b, c] {
                        stmts.extend_from_slice(&(w as u16).to_le_bytes());
                    }
                }
                ProgsFormat::Fte32 | ProgsFormat::Kk7 => {
                    // 16-bit relative jumps were sign-extended into u32 by the caller.
                    for w in [op, a, b, c] {
                        stmts.extend_from_slice(&w.to_le_bytes());
                    }
                }
                ProgsFormat::UHexen2 => {
                    for w in [op << 16, a, b, c] {
                        stmts.extend_from_slice(&w.to_le_bytes());
                    }
                }
            }
        }
        let ofs_statements = put(&mut out, &stmts);

        let defs = |list: &[DefRecord]| {
            let mut v = Vec::new();
            for d in list {
                match format {
                    ProgsFormat::QTest => {
                        for w in [d.ty, d.name, d.ofs] {
                            v.extend_from_slice(&w.to_le_bytes());
                        }
                    }
                    ProgsFormat::V6 | ProgsFormat::Fte16 | ProgsFormat::Kk7 => {
                        v.extend_from_slice(&(d.ty as u16).to_le_bytes());
                        v.extend_from_slice(&(d.ofs as u16).to_le_bytes());
                        v.extend_from_slice(&d.name.to_le_bytes());
                    }
                    ProgsFormat::Fte32 => {
                        for w in [d.ty, d.ofs, d.name] {
                            v.extend_from_slice(&w.to_le_bytes());
                        }
                    }
                    ProgsFormat::UHexen2 => {
                        for w in [d.ty << 16, d.ofs, d.name] {
                            v.extend_from_slice(&w.to_le_bytes());
                        }
                    }
                }
            }
            v
        };
        let gdefs = defs(&self.global_defs);
        let fdefs = defs(&self.field_defs);
        let ofs_globaldefs = put(&mut out, &gdefs);
        let ofs_fielddefs = put(&mut out, &fdefs);

        let mut funcs = Vec::new();
        for f in &self.functions {
            if format == ProgsFormat::QTest {
                for w in [
                    f.first_statement as u32,
                    0,
                    f.locals,
                    0,
                    f.name,
                    f.file,
                    f.num_parms as u32,
                    f.parm_start,
                ] {
                    funcs.extend_from_slice(&w.to_le_bytes());
                }
                for s in f.parm_sizes {
                    funcs.extend_from_slice(&u32::from(s).to_le_bytes());
                }
            } else {
                for w in [
                    f.first_statement as u32,
                    f.parm_start,
                    f.locals,
                    0,
                    f.name,
                    f.file,
                    f.num_parms as u32,
                ] {
                    funcs.extend_from_slice(&w.to_le_bytes());
                }
                funcs.extend_from_slice(&f.parm_sizes);
            }
        }
        let ofs_functions = put(&mut out, &funcs);

        let mut globals = Vec::new();
        for g in &self.globals {
            globals.extend_from_slice(&g.to_le_bytes());
        }
        let ofs_globals = put(&mut out, &globals);

        let mut ofs_bodyless = 0;
        if !self.bodyless.is_empty() {
            let mut names = Vec::new();
            for n in &self.bodyless {
                names.extend_from_slice(n);
                names.push(0);
            }
            ofs_bodyless = put(&mut out, &names);
        }

        let version = match format {
            ProgsFormat::QTest => 3,
            ProgsFormat::V6 => 6,
            _ => 7,
        };
        let mut header = vec![
            version,
            0x1234,
            ofs_statements,
            self.statements.len() as u32,
            ofs_globaldefs,
            self.global_defs.len() as u32,
            ofs_fielddefs,
            self.field_defs.len() as u32,
            ofs_functions,
            self.functions.len() as u32,
            ofs_strings,
            self.strings.len() as u32,
            ofs_globals,
            self.globals.len() as u32,
            self.entity_fields,
        ];
        if v7 {
            let magic = match format {
                ProgsFormat::Fte16 => 0x021B_1461,
                ProgsFormat::Fte32 => 0x6516_7402,
                ProgsFormat::UHexen2 => u32::from_le_bytes(*b"UH27"),
                _ => self.kk7_magic,
            };
            header.extend_from_slice(&[
                0,
                0,
                ofs_bodyless,
                self.bodyless.len() as u32,
                0,
                0,
                0,
                magic,
            ]);
        }
        for (i, v) in &self.header_overrides {
            header[*i] = *v;
        }
        for (i, w) in header.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        out
    }
}
