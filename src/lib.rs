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

mod bytes;
pub mod opcode;
pub mod progs;

pub use opcode::Op;
pub use progs::{LoadError, LoadNote, Program, ProgsFormat};
