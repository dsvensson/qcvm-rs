// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime errors, warnings and backtraces.

use std::fmt;

use crate::value::FuncRef;

/// A resource with a configured limit (see [`Limits`](crate::Limits)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Resource {
    /// Entities.
    Entities,
    /// Temp strings (count or bytes).
    TempStrings,
    /// The QuakeC heap (`memalloc`).
    Heap,
    /// Field capacity per entity.
    Fields,
    /// Address space for additional progs.
    ProgsArea,
    /// Loaded progs.
    Progs,
    /// Sleeping threads.
    Threads,
    /// String buffers.
    StringBuffers,
    /// Hash tables.
    HashTables,
}

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// An invalid opcode was executed (also statements the loader poisoned).
    BadOpcode(u16),
    /// A jump led outside the program.
    JumpOutOfRange,
    /// Called the null function.
    NullFunction,
    /// Called a function reference that does not exist or whose record is malformed.
    InvalidFunction(FuncRef),
    /// Called a builtin this VM does not implement.
    BuiltinNotImplemented {
        /// The builtin number (0 for name-resolved builtins).
        number: u32,
        /// The builtin's name.
        name: Box<[u8]>,
    },
    /// The QuakeC call stack is full.
    CallDepth,
    /// The local-variable stack is full.
    LocalStack,
    /// Too many nested calls from builtins back into QuakeC.
    Reentrancy,
    /// A host call passed more than the 8 arguments QuakeC functions can take.
    TooManyArguments(usize),
    /// The instruction budget ran out (an infinite loop, most likely).
    Runaway,
    /// Read through an invalid pointer.
    BadPointerRead(u32),
    /// Wrote through an invalid pointer.
    BadPointerWrite(u32),
    /// Wrote through a null pointer.
    NullPointerWrite,
    /// An array index was out of bounds.
    ArrayIndex(i64),
    /// A `BOUNDCHECK` failed.
    BoundCheck {
        /// The value checked.
        value: i32,
        /// Inclusive lower bound.
        low: u32,
        /// Exclusive upper bound.
        high: u32,
    },
    /// `PUSH` requested more local-stack space than is available.
    PushedTooMuch,
    /// `GADDRESS`, which FTE never implemented.
    GAddress,
    /// `CASERANGE` inside a string switch.
    StringCaseRange,
    /// No free entity slot.
    NoFreeEdicts,
    /// A resource limit was reached.
    OutOfMemory(Resource),
    /// QuakeC called `error()` (or `objerror()`); the payload is its message.
    QcError(Box<[u8]>),
    /// A builtin failed.
    Builtin(String),
    /// A host-side error raised from a builtin.
    Host(String),
    /// The VM is in an inconsistent state after a builtin panicked; call `reset`.
    Poisoned,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadOpcode(op) => write!(f, "bad opcode {op}"),
            Self::JumpOutOfRange => f.write_str("jump out of range"),
            Self::NullFunction => f.write_str("NULL function"),
            Self::InvalidFunction(r) => write!(f, "invalid function {:#x}", r.0),
            Self::BuiltinNotImplemented { number, name } => {
                write!(f, "Builtin {number}:{} not implemented", String::from_utf8_lossy(name))
            }
            Self::CallDepth => f.write_str("stack overflow"),
            Self::LocalStack => f.write_str("local stack overflow"),
            Self::Reentrancy => f.write_str("too many nested calls into QuakeC"),
            Self::TooManyArguments(n) => write!(f, "{n} arguments passed; QuakeC takes at most 8"),
            Self::Runaway => f.write_str("runaway loop error"),
            Self::BadPointerRead(p) => write!(f, "bad pointer read ({p:#x})"),
            Self::BadPointerWrite(p) => write!(f, "bad pointer write ({p:#x})"),
            Self::NullPointerWrite => f.write_str("null pointer write"),
            Self::ArrayIndex(i) => write!(f, "array index {i} out of bounds"),
            Self::BoundCheck { value, low, high } => {
                write!(f, "array index {value} out of bounds [{low}, {high})")
            }
            Self::PushedTooMuch => f.write_str("pushed too much"),
            Self::GAddress => f.write_str("GADDRESS is not implemented"),
            Self::StringCaseRange => f.write_str("string CASERANGE is not supported"),
            Self::NoFreeEdicts => f.write_str("no free edicts"),
            Self::OutOfMemory(r) => write!(f, "out of memory ({r:?})"),
            Self::QcError(msg) => write!(f, "{}", String::from_utf8_lossy(msg)),
            Self::Builtin(msg) | Self::Host(msg) => f.write_str(msg),
            Self::Poisoned => f.write_str("VM poisoned by an earlier panic; reset it"),
        }
    }
}

/// One frame of a QuakeC backtrace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BacktraceFrame {
    /// The function.
    pub function: FuncRef,
    /// The function name.
    pub name: Box<[u8]>,
    /// The source file, if known.
    pub file: Box<[u8]>,
    /// The statement being executed.
    pub statement: u32,
    /// The source line, if line numbers are loaded.
    pub line: Option<u32>,
}

impl fmt::Display for BacktraceFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.name))?;
        if !self.file.is_empty() {
            write!(f, " ({}", String::from_utf8_lossy(&self.file))?;
            if let Some(line) = self.line {
                write!(f, ":{line}")?;
            }
            f.write_str(")")?;
        }
        write!(f, " @ statement {}", self.statement)
    }
}

/// A QuakeC backtrace, innermost frame first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Backtrace(pub Vec<BacktraceFrame>);

impl fmt::Display for Backtrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for frame in &self.0 {
            writeln!(f, "  {frame}")?;
        }
        Ok(())
    }
}

/// An error raised while running QuakeC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmError(Box<Inner>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Inner {
    kind: ErrorKind,
    backtrace: Backtrace,
}

impl VmError {
    /// Creates an error without a backtrace (the VM adds one when it propagates out of QuakeC).
    #[must_use]
    pub fn new(kind: ErrorKind) -> Self {
        Self(Box::new(Inner { kind, backtrace: Backtrace::default() }))
    }

    /// A builtin failure with a message.
    #[must_use]
    pub fn builtin(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Builtin(msg.into()))
    }

    /// A host-side failure with a message.
    #[must_use]
    pub fn host(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::Host(msg.into()))
    }

    /// What went wrong.
    #[must_use]
    pub fn kind(&self) -> &ErrorKind {
        &self.0.kind
    }

    /// The QuakeC call stack at the point of failure, innermost first.
    #[must_use]
    pub fn backtrace(&self) -> &Backtrace {
        &self.0.backtrace
    }

    pub(crate) fn with_backtrace(mut self, backtrace: Backtrace) -> Self {
        if self.0.backtrace.0.is_empty() {
            self.0.backtrace = backtrace;
        }
        self
    }
}

impl From<ErrorKind> for VmError {
    fn from(kind: ErrorKind) -> Self {
        Self::new(kind)
    }
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.kind)?;
        if !self.0.backtrace.0.is_empty() {
            write!(f, "\n{}", self.0.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for VmError {}

/// A non-fatal problem; execution continued with a fallback value.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WarningKind {
    /// An entity number outside the allocated entities.
    BadEntity(u32),
    /// A field offset outside the entity.
    BadField(i32),
    /// A write to a protected (read-only) entity was skipped.
    ReadOnlyEntity(u32),
    /// A string reference that resolves to nothing.
    BadString(u32),
    /// A warning raised by a builtin.
    Builtin(String),
    /// `n` further warnings were suppressed during this call.
    Suppressed(u32),
}

impl fmt::Display for WarningKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadEntity(e) => write!(f, "bad entity index {e}"),
            Self::BadField(o) => write!(f, "bad field offset {o}"),
            Self::ReadOnlyEntity(e) => write!(f, "write to protected entity {e} skipped"),
            Self::BadString(r) => write!(f, "invalid string reference {r:#x}"),
            Self::Builtin(msg) => f.write_str(msg),
            Self::Suppressed(n) => write!(f, "{n} more warnings suppressed"),
        }
    }
}

/// A warning with the QuakeC backtrace where it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// What happened.
    pub kind: WarningKind,
    /// Where it happened, innermost frame first.
    pub backtrace: Backtrace,
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        if let Some(top) = self.backtrace.0.first() {
            write!(f, " in {top}")?;
        }
        Ok(())
    }
}
