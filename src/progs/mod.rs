// SPDX-License-Identifier: MIT OR Apache-2.0

//! Loading compiled QuakeC programs.
//!
//! [`Program::parse`] accepts every format FTE accepts and converts it into one canonical,
//! validated, immutable form that any number of VMs can share through an [`Arc`](std::sync::Arc).

mod disasm;
mod format;
mod lno;
mod load;

use std::collections::HashMap;
use std::fmt;

pub use disasm::Disassembly;

use crate::bytes::{cstr_at, usize_from};
use crate::opcode::Op;

/// The on-disk format a program was loaded from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProgsFormat {
    /// Version 3, from the QTest release of Quake.
    QTest,
    /// Version 6, the standard Quake format.
    V6,
    /// FTE's version 7 with 16-bit statements and definitions.
    Fte16,
    /// FTE's version 7 with 32-bit statements and definitions.
    Fte32,
    /// KK QuakeWorld server's version 7 (32-bit statements, 16-bit definitions). FTE also assumes
    /// this layout for version-7 files with an unknown secondary version.
    Kk7,
    /// uHexen2's version 7 (32-bit records, opcode and type in the upper 16 bits).
    UHexen2,
}

impl ProgsFormat {
    /// Whether statement operands are 16-bit.
    #[must_use]
    pub const fn has_16bit_statements(self) -> bool {
        matches!(self, Self::QTest | Self::V6 | Self::Fte16)
    }
}

/// The type of a global or field definition (FTE's `EV_*` codes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    /// `EV_VOID` (0).
    Void,
    /// `EV_STRING` (1).
    String,
    /// `EV_FLOAT` (2).
    Float,
    /// `EV_VECTOR` (3).
    Vector,
    /// `EV_ENTITY` (4).
    Entity,
    /// `EV_FIELD` (5).
    Field,
    /// `EV_FUNCTION` (6).
    Function,
    /// `EV_POINTER` (7).
    Pointer,
    /// `EV_INTEGER` (8).
    Integer,
    /// `EV_UINT` (9).
    UInt,
    /// `EV_INT64` (10).
    Int64,
    /// `EV_UINT64` (11).
    UInt64,
    /// `EV_DOUBLE` (12).
    Double,
    /// Any other (compiler-internal) type code.
    Other(u32),
}

impl Type {
    /// Converts an `EV_*` code (without the definition flag bits).
    #[must_use]
    pub const fn from_code(code: u32) -> Self {
        match code {
            0 => Self::Void,
            1 => Self::String,
            2 => Self::Float,
            3 => Self::Vector,
            4 => Self::Entity,
            5 => Self::Field,
            6 => Self::Function,
            7 => Self::Pointer,
            8 => Self::Integer,
            9 => Self::UInt,
            10 => Self::Int64,
            11 => Self::UInt64,
            12 => Self::Double,
            other => Self::Other(other),
        }
    }

    /// The `EV_*` code.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::Void => 0,
            Self::String => 1,
            Self::Float => 2,
            Self::Vector => 3,
            Self::Entity => 4,
            Self::Field => 5,
            Self::Function => 6,
            Self::Pointer => 7,
            Self::Integer => 8,
            Self::UInt => 9,
            Self::Int64 => 10,
            Self::UInt64 => 11,
            Self::Double => 12,
            Self::Other(code) => code,
        }
    }

    /// Size of a value of this type in 32-bit words, where known.
    #[must_use]
    pub const fn words(self) -> Option<u32> {
        match self {
            Self::Void => Some(0),
            Self::Vector => Some(3),
            Self::Int64 | Self::UInt64 | Self::Double => Some(2),
            Self::Other(_) => None,
            _ => Some(1),
        }
    }
}

/// A global or field definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Def {
    pub(crate) ty: Type,
    /// Word offset: into the globals (global defs) or into an entity (field defs).
    pub(crate) ofs: u32,
    /// Offset of the name in the string table.
    pub(crate) name: u32,
    /// `0x8000`: saved in savegames.
    pub(crate) save: bool,
    /// `0x4000`: shared between progs.
    pub(crate) shared: bool,
}

/// A read-only view of a global or field definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefInfo<'a> {
    /// The symbol name.
    pub name: &'a [u8],
    /// The type.
    pub ty: Type,
    /// Word offset into the globals (for globals) or into an entity (for fields).
    pub offset: u32,
    /// Whether the definition is marked for savegames.
    pub save: bool,
    /// Whether the definition is marked as shared between progs.
    pub shared: bool,
}

/// Why a function record was rejected. Calling such a function faults; loading still succeeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InvalidFunction {
    /// The first statement lies outside the statement table.
    EntryOutOfRange,
    /// The locals range or a parameter copy lies outside the globals.
    LocalsOutOfRange,
}

/// What a function record describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FunctionKind {
    /// Function 0, the null function.
    Null,
    /// QuakeC code starting at the given statement.
    QuakeC {
        /// Index of the first statement.
        entry: u32,
    },
    /// A builtin with a fixed number (`#N`).
    Builtin {
        /// The builtin number.
        number: u32,
    },
    /// A builtin resolved by the function's own name (`#0`).
    NamedBuiltin,
    /// A malformed record.
    Invalid(InvalidFunction),
}

/// One parameter word to copy from a PARM slot into a function's locals, as byte offsets
/// relative to the start of the globals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParamCopy {
    pub(crate) src: u32,
    pub(crate) dst: u32,
}

/// A function record in canonical form.
#[derive(Clone, Debug)]
pub(crate) struct Function {
    pub(crate) kind: FunctionKind,
    /// First global word of the parameters and locals.
    pub(crate) parm_start: u32,
    /// Number of words of parameters and locals.
    pub(crate) locals: u32,
    pub(crate) num_parms: i32,
    pub(crate) parm_sizes: [u8; 8],
    /// Range into [`Program::param_copies`].
    pub(crate) copies_start: u32,
    pub(crate) copies_end: u32,
    pub(crate) name: u32,
    pub(crate) file: u32,
}

/// A read-only view of a function record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FunctionInfo<'a> {
    /// Index in the function table.
    pub index: u32,
    /// The function name.
    pub name: &'a [u8],
    /// The source file name, if the compiler kept it.
    pub file: &'a [u8],
    /// What the record describes.
    pub kind: FunctionKind,
    /// First global word of the parameters and locals.
    pub parm_start: u32,
    /// Number of words of parameters and locals.
    pub locals: u32,
    /// Declared number of parameters (for builtins: introspection only).
    pub num_parms: i32,
    /// Size in words of each parameter.
    pub parm_sizes: [u8; 8],
}

/// Statement flag: a debugger breakpoint is set on this statement.
pub(crate) const STMT_BREAKPOINT: u16 = 1;

/// A statement in canonical form.
///
/// Global operands are byte offsets relative to the start of the program's globals; jump operands
/// are absolute statement indices; `GlobalIndex` and `Immediate` operands are raw values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stmt {
    pub(crate) op: Op,
    pub(crate) flags: u16,
    pub(crate) a: u32,
    pub(crate) b: u32,
    pub(crate) c: u32,
}

/// Something noteworthy the loader did or found. Loading still succeeded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadNote {
    /// A version-7 file with an unrecognised secondary version was loaded as KK7, as FTE does.
    AssumedKk7 {
        /// The secondary version found in the header.
        secondary_version: u32,
    },
    /// The program uses the Hexen 2 calling convention: `CALLn` became `CALLnH`.
    Hexen2Calls,
    /// A statement has an unknown opcode or an operand outside the globals. It faults if run.
    PoisonedStatement {
        /// Statement index.
        index: u32,
        /// The raw opcode number.
        opcode: u32,
    },
    /// A statement jumps outside the statement table. The jump faults if taken.
    JumpOutOfRange {
        /// Statement index.
        index: u32,
    },
    /// A function record is malformed. Calling it faults.
    InvalidFunction {
        /// Function index.
        index: u32,
        /// What is wrong with it.
        reason: InvalidFunction,
    },
    /// The file has a types section, which is ignored.
    TypesIgnored,
    /// The file's line-number section lies outside the file and was ignored.
    LineNumbersIgnored,
    /// The file's bodyless-function section lies outside the file and was ignored.
    BodylessIgnored,
}

impl fmt::Display for LoadNote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AssumedKk7 { secondary_version } => write!(
                f,
                "unknown secondary version {secondary_version:#010x}; assuming the KK7 layout"
            ),
            Self::Hexen2Calls => f.write_str("Hexen 2 calling convention detected"),
            Self::PoisonedStatement { index, opcode } => {
                write!(f, "statement {index} (opcode {opcode}) is invalid and will fault if run")
            }
            Self::JumpOutOfRange { index } => {
                write!(f, "statement {index} jumps outside the program")
            }
            Self::InvalidFunction { index, reason } => {
                write!(f, "function {index} is invalid ({reason:?})")
            }
            Self::TypesIgnored => f.write_str("types section ignored"),
            Self::LineNumbersIgnored => f.write_str("line-number section out of bounds; ignored"),
            Self::BodylessIgnored => {
                f.write_str("bodyless-function section out of bounds; ignored")
            }
        }
    }
}

/// Why a program could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The data is too short to hold a header.
    Truncated,
    /// The version number is not one FTE accepts.
    UnsupportedVersion(u32),
    /// Some sections are compressed (FTE refuses these too).
    Compressed(u32),
    /// A section lies outside the file.
    SectionOutOfBounds(&'static str),
    /// A `.lno` file is malformed or belongs to a different program.
    LineNumbers(&'static str),
    /// The names of the definitions and functions add up to more than 16 MiB (names may share
    /// bytes of the string table, so this is not bounded by the file size).
    NamesTooLarge,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("progs header truncated"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported progs version {v}"),
            Self::Compressed(mask) => write!(f, "progs uses compressed sections ({mask:#x})"),
            Self::SectionOutOfBounds(s) => write!(f, "progs {s} section lies outside the file"),
            Self::LineNumbers(why) => write!(f, "line numbers rejected: {why}"),
            Self::NamesTooLarge => f.write_str("progs definition names are too large"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Global definitions with these names are patched when a VM loads the program.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SpecialGlobals {
    /// `thisprogs` (float): set to the progs number.
    pub(crate) thisprogs: Option<u32>,
    /// `__ext__fasttrackarrays`: set to 1 when present.
    pub(crate) fasttrackarrays: Option<u32>,
}

/// A loaded, validated QuakeC program.
///
/// Immutable after loading; share it between VMs with an [`Arc`](std::sync::Arc).
#[derive(Clone, Debug)]
pub struct Program {
    pub(crate) format: ProgsFormat,
    pub(crate) version: u32,
    pub(crate) crc: u32,
    /// The string table exactly as stored in the file.
    pub(crate) strings: Box<[u8]>,
    /// Initial global values.
    pub(crate) globals: Box<[u32]>,
    /// Canonical statements, followed by one [`Op::JumpOutOfRange`] sentinel.
    pub(crate) statements: Box<[Stmt]>,
    /// Number of statements in the file (without the sentinel).
    pub(crate) file_statements: u32,
    pub(crate) functions: Box<[Function]>,
    pub(crate) param_copies: Box<[ParamCopy]>,
    pub(crate) global_defs: Box<[Def]>,
    pub(crate) field_defs: Box<[Def]>,
    /// Words per entity, from the header.
    pub(crate) entity_fields: u32,
    pub(crate) functions_by_name: HashMap<Box<[u8]>, u32>,
    pub(crate) globals_by_name: HashMap<Box<[u8]>, u32>,
    pub(crate) fields_by_name: HashMap<Box<[u8]>, u32>,
    /// First global definition at each global offset (for disassembly and backtraces).
    pub(crate) global_name_by_ofs: HashMap<u32, u32>,
    /// Names of functions this program expects another progs to provide.
    pub(crate) bodyless: Box<[Box<[u8]>]>,
    /// Global words holding pointer relocations (bit 31 set in a pointer-typed global).
    pub(crate) pointer_relocs: Box<[u32]>,
    pub(crate) special: SpecialGlobals,
    /// Source line for each file statement, if known.
    pub(crate) line_numbers: Option<Box<[u32]>>,
    pub(crate) notes: Vec<LoadNote>,
}

impl Program {
    /// Parses a compiled program (`progs.dat`, `csprogs.dat`, `menu.dat`, …).
    ///
    /// # Errors
    /// Fails if the data is not a program in a format FTE accepts, or a section lies outside the
    /// data. Problems inside the code (bad opcodes, wild jumps, malformed functions) do not fail
    /// the load; they are reported by [`Program::load_notes`] and fault only when executed.
    pub fn parse(data: &[u8]) -> Result<Self, LoadError> {
        load::parse(data)
    }

    /// Attaches source line numbers from a `.lno` file written by fteqcc.
    ///
    /// # Errors
    /// Fails if the data is not a `.lno` file or was produced for a different program.
    pub fn with_line_numbers(self, lno: &[u8]) -> Result<Self, LoadError> {
        lno::attach(self, lno)
    }

    /// The file format.
    #[must_use]
    pub fn format(&self) -> ProgsFormat {
        self.format
    }

    /// The header version number (3, 6 or 7).
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// The header CRC, identifying the system definitions the program was compiled against.
    #[must_use]
    pub fn crc(&self) -> u32 {
        self.crc
    }

    /// What the loader rewrote or found suspicious.
    #[must_use]
    pub fn load_notes(&self) -> &[LoadNote] {
        &self.notes
    }

    /// Number of statements.
    #[must_use]
    pub fn num_statements(&self) -> u32 {
        self.file_statements
    }

    /// Number of global words.
    #[must_use]
    pub fn num_globals(&self) -> u32 {
        u32::try_from(self.globals.len()).unwrap_or(u32::MAX)
    }

    /// Number of functions, including the null function 0.
    #[must_use]
    pub fn num_functions(&self) -> u32 {
        u32::try_from(self.functions.len()).unwrap_or(u32::MAX)
    }

    /// Words per entity declared by the program.
    #[must_use]
    pub fn entity_fields(&self) -> u32 {
        self.entity_fields
    }

    /// The string table.
    #[must_use]
    pub fn strings(&self) -> &[u8] {
        &self.strings
    }

    /// The NUL-terminated string at `offset` in the string table (empty if out of range).
    #[must_use]
    pub fn string_at(&self, offset: u32) -> &[u8] {
        cstr_at(&self.strings, usize_from(offset))
    }

    /// The initial value of global word `index`.
    #[must_use]
    pub fn initial_global(&self, index: u32) -> Option<u32> {
        self.globals.get(usize_from(index)).copied()
    }

    /// Looks up a function by name.
    ///
    /// Like FTE, a function-typed *global* with that name takes precedence over the function
    /// table's names, so that `var`-declared entry points resolve to their current value.
    #[must_use]
    pub fn function_index(&self, name: impl AsRef<[u8]>) -> Option<u32> {
        let name = name.as_ref();
        if let Some(def) = self.global_def_raw(name)
            && def.ty == Type::Function
            && let Some(value) = self.initial_global(def.ofs)
            && value != 0
            && self.functions.get(usize_from(value)).is_some()
        {
            return Some(value);
        }
        self.functions_by_name.get(name).copied()
    }

    /// The function record at `index`.
    #[must_use]
    pub fn function(&self, index: u32) -> Option<FunctionInfo<'_>> {
        let f = self.functions.get(usize_from(index))?;
        Some(FunctionInfo {
            index,
            name: self.string_at(f.name),
            file: self.string_at(f.file),
            kind: f.kind,
            parm_start: f.parm_start,
            locals: f.locals,
            num_parms: f.num_parms,
            parm_sizes: f.parm_sizes,
        })
    }

    /// All function records, starting with the null function.
    pub fn functions(&self) -> impl Iterator<Item = FunctionInfo<'_>> + '_ {
        (0..self.num_functions()).filter_map(|i| self.function(i))
    }

    fn def_info(&self, def: &Def) -> DefInfo<'_> {
        DefInfo {
            name: self.string_at(def.name),
            ty: def.ty,
            offset: def.ofs,
            save: def.save,
            shared: def.shared,
        }
    }

    pub(crate) fn global_def_raw(&self, name: &[u8]) -> Option<&Def> {
        let index = *self.globals_by_name.get(name)?;
        self.global_defs.get(usize_from(index))
    }

    pub(crate) fn field_def_raw(&self, name: &[u8]) -> Option<&Def> {
        let index = *self.fields_by_name.get(name)?;
        self.field_defs.get(usize_from(index))
    }

    /// Looks up a global definition by name (the first one wins if a name repeats).
    #[must_use]
    pub fn global_def(&self, name: impl AsRef<[u8]>) -> Option<DefInfo<'_>> {
        self.global_def_raw(name.as_ref()).map(|d| self.def_info(d))
    }

    /// Looks up a field definition by name (the first one wins if a name repeats).
    #[must_use]
    pub fn field_def(&self, name: impl AsRef<[u8]>) -> Option<DefInfo<'_>> {
        self.field_def_raw(name.as_ref()).map(|d| self.def_info(d))
    }

    /// All global definitions, in file order.
    pub fn global_defs(&self) -> impl Iterator<Item = DefInfo<'_>> + '_ {
        self.global_defs.iter().map(|d| self.def_info(d))
    }

    /// All field definitions, in file order.
    pub fn field_defs(&self) -> impl Iterator<Item = DefInfo<'_>> + '_ {
        self.field_defs.iter().map(|d| self.def_info(d))
    }

    /// The name of the first global defined at word `offset`, if any.
    #[must_use]
    pub fn global_name_at(&self, offset: u32) -> Option<&[u8]> {
        let def = self.global_defs.get(usize_from(*self.global_name_by_ofs.get(&offset)?))?;
        Some(self.string_at(def.name))
    }

    /// Globals named `autocvar_<name>`: cvars the program expects the host to keep in sync.
    pub fn autocvars(&self) -> impl Iterator<Item = AutoCvar<'_>> + '_ {
        self.global_defs().filter_map(|d| {
            let cvar = d.name.strip_prefix(b"autocvar_")?;
            let words = d.ty.words().unwrap_or(1);
            let start = usize_from(d.offset);
            let default = self.globals.get(start..start.checked_add(usize_from(words))?)?;
            Some(AutoCvar { name: cvar, global: d, default })
        })
    }

    /// The builtins this program's QuakeC code calls: the function operand of every `CALL*` whose
    /// global starts out holding a builtin, as sorted, distinct function indices.
    ///
    /// A debug `.dat` declares far more builtins than it calls, so this is the set a host has to
    /// provide to run the program. It is a best effort: a call through a function variable that
    /// only receives a builtin while the program runs is not seen.
    #[must_use]
    pub fn called_builtins(&self) -> Vec<u32> {
        let mut called: Vec<u32> = self
            .statements
            .iter()
            .filter(|st| st.op.call_argc().is_some())
            .filter_map(|st| self.initial_global(st.a / 4))
            .map(|v| v & 0x00FF_FFFF)
            .filter(|&i| {
                self.functions.get(usize_from(i)).is_some_and(|f| {
                    matches!(f.kind, FunctionKind::Builtin { .. } | FunctionKind::NamedBuiltin)
                })
            })
            .collect();
        called.sort_unstable();
        called.dedup();
        called
    }

    /// Names of the functions this program expects another loaded progs to define.
    pub fn bodyless_functions(&self) -> impl Iterator<Item = &[u8]> + '_ {
        self.bodyless.iter().map(AsRef::as_ref)
    }

    /// The source line of statement `index`, if line numbers are known.
    #[must_use]
    pub fn source_line(&self, index: u32) -> Option<u32> {
        self.line_numbers.as_ref()?.get(usize_from(index)).copied()
    }

    /// A printable disassembly of function `index`.
    #[must_use]
    pub fn disassemble(&self, index: u32) -> Disassembly<'_> {
        Disassembly::new(self, index)
    }

    pub(crate) fn func(&self, index: u32) -> Option<&Function> {
        self.functions.get(usize_from(index))
    }

    pub(crate) fn copies(&self, f: &Function) -> &[ParamCopy] {
        self.param_copies
            .get(usize_from(f.copies_start)..usize_from(f.copies_end))
            .unwrap_or_default()
    }
}

/// A global that mirrors a cvar (`autocvar_<name>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoCvar<'a> {
    /// The cvar name (without the `autocvar_` prefix).
    pub name: &'a [u8],
    /// The global definition.
    pub global: DefInfo<'a>,
    /// The default value's words as compiled into the program.
    pub default: &'a [u32],
}
