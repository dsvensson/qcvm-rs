// SPDX-License-Identifier: MIT OR Apache-2.0

//! A re-entrant, reusable QuakeC virtual machine.
//!
//! `qcvm` loads progs compiled by `fteqcc` (every format FTE accepts), executes every opcode FTE
//! implements, and ships the builtins that do not need an engine. Hosts register their own
//! builtins, call QuakeC functions, and may be called back from QuakeC while doing so.
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use qcvm::{Arg, Builtins, Host, Numbering, Program, Vm, VmConfig, VmError};
//!
//! #[derive(Default)]
//! struct Client {
//!     frames: u32,
//! }
//!
//! impl Host for Client {
//!     fn print(&mut self, text: &[u8]) {
//!         print!("{}", String::from_utf8_lossy(text));
//!     }
//! }
//!
//! /// `float() framecount = #500;`
//! fn framecount(vm: &mut Vm<Client>, host: &mut Client) -> Result<(), VmError> {
//!     vm.ret_f32(host.frames as f32);
//!     Ok(())
//! }
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let program = Arc::new(Program::parse(&std::fs::read("csprogs.dat")?)?);
//! let mut builtins = Builtins::standard(Numbering::Csqc);
//! builtins.set_numbered(500, "framecount", framecount);
//! let mut vm = Vm::new(program, Arc::new(builtins), VmConfig::csqc())?;
//! let mut client = Client::default();
//!
//! let time = vm.global::<f32>("time")?;
//! vm.set(time, 1.5);
//! let init = vm.find_function("CSQC_Init").ok_or("no CSQC_Init")?;
//! vm.call(&mut client, init, &[Arg::Float(0.0), Arg::Bytes(b"qualia"), Arg::Float(1.0)])?;
//! # Ok(())
//! # }
//! ```
//!
//! See `docs/design.md` in the repository for the architecture and `docs/spec` for the
//! behaviour being implemented.

#![forbid(unsafe_code)]

pub mod builtins;
mod bytes;
pub mod csqc;
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
