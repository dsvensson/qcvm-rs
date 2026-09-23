// SPDX-License-Identifier: MIT OR Apache-2.0

//! The interface between a VM and the program embedding it.

use std::borrow::Cow;
use std::sync::Arc;

use crate::error::{VmError, Warning};
use crate::progs::Program;
use crate::vm::{StateOp, Vm};

/// Metadata about a cvar, for `cvar_type`, `cvar_defstring` and `cvar_description`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CvarInfo {
    /// FTE's `cvar_type` bits: 1 exists, 2 archived, 4 private, 8 engine-created,
    /// 16 has a description, 32 read-only.
    pub flags: u32,
    /// The default value.
    pub default: Vec<u8>,
    /// The description, if any.
    pub description: Option<Vec<u8>>,
}

/// A broken-down calendar time, for `strftime` and `calltimeofday`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CalendarTime {
    /// Year, e.g. 2026.
    pub year: i32,
    /// Month, 0–11.
    pub month: u32,
    /// Day of the month, 1–31.
    pub day: u32,
    /// Hour, 0–23.
    pub hour: u32,
    /// Minute, 0–59.
    pub minute: u32,
    /// Second, 0–60.
    pub second: u32,
    /// Day of the week, 0 = Sunday.
    pub weekday: u32,
    /// Day of the year, 0–365.
    pub yearday: u32,
    /// Offset from UTC in seconds (0 for UTC).
    pub utc_offset: i32,
    /// Time-zone abbreviation, if known.
    pub zone: Option<&'static str>,
}

/// Where a textual dump requested by QuakeC should go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DumpKind {
    /// `coredump()`: all globals and entities.
    CoreDump,
    /// `eprint(e)`: one entity.
    Entity,
    /// The entity dump printed by `objerror`.
    ObjError,
    /// A stack trace (`stackdump`, `traceon`).
    Trace,
}

/// Services the embedding program provides to a VM.
///
/// Every method has a default, so a minimal host is `impl Host for MyState {}`. The defaults make
/// the VM self-contained: printing goes nowhere, cvars read as unset, clocks come from the
/// operating system. Host-specific builtins receive `&mut Self` alongside the VM, which is where
/// their state lives.
pub trait Host: Sized {
    /// A non-fatal problem in QuakeC (bad entity, bad string reference, …). Default: ignored.
    fn warning(&mut self, warning: &Warning) {
        let _ = warning;
    }

    /// Text from `print` (and `bprint`-style builtins). Default: discarded.
    fn print(&mut self, text: &[u8]) {
        let _ = text;
    }

    /// Text from `dprint`, printed only in developer mode. Default: discarded.
    fn dprint(&mut self, text: &[u8]) {
        let _ = text;
    }

    /// Text from `cprint` (centre print). Default: discarded.
    fn centerprint(&mut self, text: &[u8]) {
        let _ = text;
    }

    /// Text for the console command buffer (`localcmd`). Default: discarded.
    fn localcmd(&mut self, text: &[u8]) {
        let _ = text;
    }

    /// A textual dump (`coredump`, `eprint`, `objerror`, stack traces). Default: discarded.
    fn dump(&mut self, kind: DumpKind, text: &[u8]) {
        let _ = (kind, text);
    }

    /// The numeric value of a cvar (`cvar`). Default: 0.
    fn cvar_float(&mut self, name: &[u8]) -> f32 {
        let _ = name;
        0.0
    }

    /// The string value of a cvar (`cvar_string`), or `None` if it does not exist.
    /// Default: `None`.
    fn cvar_string(&mut self, name: &[u8]) -> Option<Cow<'_, [u8]>> {
        let _ = name;
        None
    }

    /// Sets a cvar (`cvar_set`). Default: ignored.
    fn cvar_set(&mut self, name: &[u8], value: &[u8]) {
        let _ = (name, value);
    }

    /// Metadata about a cvar, or `None` if it does not exist. Default: `None`.
    fn cvar_info(&mut self, name: &[u8]) -> Option<CvarInfo> {
        let _ = name;
        None
    }

    /// Creates a cvar if it does not exist (`registercvar`); returns whether it was created.
    /// Default: `false`.
    fn register_cvar(&mut self, name: &[u8], value: &[u8], flags: u32) -> bool {
        let _ = (name, value, flags);
        false
    }

    /// Names of cvars matching `pattern` (a wildcard pattern if it contains `*` or `?`, else a
    /// prefix) and not matching `antipattern` (`buf_cvarlist`). Default: none.
    fn cvar_list(&mut self, pattern: &[u8], antipattern: &[u8]) -> Vec<Vec<u8>> {
        let _ = (pattern, antipattern);
        Vec::new()
    }

    /// Whether any archived cvar changed since the config was saved (`cvars_haveunsaved`).
    /// Default: `false`.
    fn cvars_have_unsaved(&mut self) -> bool {
        false
    }

    /// Whether the host supports an extension (`checkextension`). Default: the extensions the
    /// VM's standard builtins implement completely.
    fn check_extension(&mut self, name: &[u8]) -> bool {
        crate::stdlib::has_extension(name)
    }

    /// Whether a console command exists (`checkcommand`): 1 command, 2 alias, 3 cvar, 0 none.
    /// Default: 0.
    fn check_command(&mut self, name: &[u8]) -> u32 {
        let _ = name;
        0
    }

    /// Registers a console command routed to QuakeC (`registercommand`). Default: ignored.
    fn register_command(&mut self, name: &[u8]) {
        let _ = name;
    }

    /// `isdemo()`: 0 not playing a demo, 1 a demo, 2 an MVD. Default: 0.
    fn is_demo(&mut self) -> f32 {
        0.0
    }

    /// `isserver()`: whether a local server runs. Default: `false`.
    fn is_server(&mut self) -> bool {
        false
    }

    /// Client simulation time for `gettime(5)`. Default: `None` (the VM's real-time clock).
    fn sim_time(&mut self) -> Option<f64> {
        None
    }

    /// The current calendar time, local if `local` is set, else UTC (`strftime`,
    /// `calltimeofday`). Default: UTC from the system clock, also for local time.
    fn calendar_time(&mut self, local: bool) -> Option<CalendarTime> {
        let _ = local;
        crate::stdlib::utc_now()
    }

    /// Reads a file for QuakeC (`buf_loadfile`). Default: `None`.
    fn read_file(&mut self, path: &[u8]) -> Option<Vec<u8>> {
        let _ = path;
        None
    }

    /// Loads another progs for `addprogs`. Default: `None`.
    fn load_progs(&mut self, name: &[u8]) -> Option<Arc<Program>> {
        let _ = name;
        None
    }

    /// Performs an animation opcode (`STATE`, `CSTATE`, `CWSTATE`, `THINKTIME`).
    ///
    /// Return `Ok(false)` to let the VM apply FTE's default behaviour (which updates `self`'s
    /// `frame`, `think` and `nextthink` fields). Default: `Ok(false)`.
    ///
    /// # Errors
    /// An error aborts the QuakeC call that executed the opcode.
    fn state_op(&mut self, vm: &mut Vm<Self>, op: StateOp) -> Result<bool, VmError> {
        let _ = (vm, op);
        Ok(false)
    }

    /// Called after `spawn` (from QuakeC or the host) allocated an entity. Default: nothing.
    fn on_spawn(&mut self, vm: &mut Vm<Self>, e: crate::value::EntRef) {
        let _ = (vm, e);
    }
}

/// A host that provides nothing (every hook keeps its default).
#[derive(Clone, Copy, Debug, Default)]
pub struct NullHost;

impl Host for NullHost {}
