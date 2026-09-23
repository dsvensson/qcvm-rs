// SPDX-License-Identifier: MIT OR Apache-2.0

//! A re-entrant, reusable QuakeC virtual machine.
//!
//! `qcvm` loads progs compiled by `fteqcc` (every format FTE accepts), executes every opcode FTE
//! implements, and ships the builtins that do not need an engine. Hosts register their own
//! builtins, call QuakeC functions, and may be called back from QuakeC while doing so.
//!
//! See `docs/design.md` in the repository for the architecture and `docs/spec` for the
//! behaviour being implemented.

#![forbid(unsafe_code)]

pub mod builtins;
mod bytes;
pub mod error;
pub mod host;
pub mod opcode;
pub mod progs;
pub mod stdlib;
pub mod value;
pub mod vm;

pub use builtins::{BuiltinFn, Builtins, Numbering};
pub use error::{ErrorKind, VmError, Warning, WarningKind};
pub use host::{CalendarTime, CvarInfo, DumpKind, Host, NullHost};
pub use opcode::Op;
pub use progs::{LoadError, LoadNote, Program, ProgsFormat};
pub use value::{
    Arg, EntRef, Field, FieldOfs, FuncRef, Global, PrNum, Ptr, QcValue, Ret, StrRef, Vec3,
};
pub use vm::{Limits, LookupError, StateOp, Vm, VmConfig, VmKind};
