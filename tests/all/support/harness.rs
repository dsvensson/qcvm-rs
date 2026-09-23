// SPDX-License-Identifier: MIT OR Apache-2.0

//! A harness for testing builtins directly: a progs that declares builtin stubs (numbered as FTE
//! numbers them, or by name), and a host that records what the builtins did.

#![allow(dead_code)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use qcvm::builtins::known_builtins;
use qcvm::{
    Arg, Builtins, CalendarTime, CvarInfo, DumpKind, FuncRef, Host, Numbering, Program,
    ProgsFormat, Ret, Vm, VmConfig, VmError, Warning,
};

use crate::support::asm::Asm;

/// A host recording prints, commands and warnings, with an in-memory cvar table, a file table
/// and an optional fixed clock.
#[derive(Default)]
pub struct BHost {
    pub printed: Vec<u8>,
    pub dprinted: Vec<u8>,
    pub centerprinted: Vec<u8>,
    pub localcmds: Vec<u8>,
    pub dumps: Vec<(DumpKind, Vec<u8>)>,
    pub warnings: Vec<Warning>,
    pub cvars: HashMap<Vec<u8>, Vec<u8>>,
    pub commands: Vec<Vec<u8>>,
    pub files: HashMap<Vec<u8>, Vec<u8>>,
    pub now: Option<CalendarTime>,
    pub sim_time: Option<f64>,
}

impl Host for BHost {
    fn warning(&mut self, w: &Warning) {
        self.warnings.push(w.clone());
    }
    fn print(&mut self, text: &[u8]) {
        self.printed.extend_from_slice(text);
    }
    fn dprint(&mut self, text: &[u8]) {
        self.dprinted.extend_from_slice(text);
    }
    fn centerprint(&mut self, text: &[u8]) {
        self.centerprinted.extend_from_slice(text);
    }
    fn localcmd(&mut self, text: &[u8]) {
        self.localcmds.extend_from_slice(text);
    }
    fn dump(&mut self, kind: DumpKind, text: &[u8]) {
        self.dumps.push((kind, text.to_vec()));
    }
    fn cvar_float(&mut self, name: &[u8]) -> f32 {
        self.cvars
            .get(name)
            .and_then(|v| std::str::from_utf8(v).ok()?.trim().parse().ok())
            .unwrap_or(0.0)
    }
    fn cvar_string(&mut self, name: &[u8]) -> Option<Cow<'_, [u8]>> {
        self.cvars.get(name).map(|v| Cow::Borrowed(v.as_slice()))
    }
    fn cvar_set(&mut self, name: &[u8], value: &[u8]) {
        self.cvars.insert(name.to_vec(), value.to_vec());
    }
    fn cvar_info(&mut self, name: &[u8]) -> Option<CvarInfo> {
        self.cvars.get(name).map(|_| CvarInfo { flags: 1, default: Vec::new(), description: None })
    }
    fn register_cvar(&mut self, name: &[u8], value: &[u8], _flags: u32) -> bool {
        if self.cvars.contains_key(name) {
            return false;
        }
        self.cvars.insert(name.to_vec(), value.to_vec());
        true
    }
    fn cvar_list(&mut self, pattern: &[u8], _anti: &[u8]) -> Vec<Vec<u8>> {
        let mut v: Vec<_> = self.cvars.keys().filter(|k| k.starts_with(pattern)).cloned().collect();
        v.sort();
        v
    }
    fn register_command(&mut self, name: &[u8]) {
        self.commands.push(name.to_vec());
    }
    fn sim_time(&mut self) -> Option<f64> {
        self.sim_time
    }
    fn calendar_time(&mut self, _local: bool) -> Option<CalendarTime> {
        self.now.or_else(qcvm::stdlib::utc_now)
    }
    fn read_file(&mut self, path: &[u8]) -> Option<Vec<u8>> {
        self.files.get(path).cloned()
    }
}

/// A VM with the standard builtins and a progs declaring every builtin FTE knows for the
/// numbering (plus any extra definitions), so tests can call builtins by name.
pub struct Harness {
    pub vm: Vm<BHost>,
    pub host: BHost,
}

impl Harness {
    /// CSQC numbering, default configuration, no extra definitions.
    pub fn new() -> Self {
        Self::with(Numbering::Csqc, VmConfig::default(), |_| {})
    }

    /// `setup` may add globals, fields, functions and `#0` builtin stubs to the progs before the
    /// known builtins are declared.
    pub fn with(numbering: Numbering, config: VmConfig, setup: impl FnOnce(&mut Asm)) -> Self {
        Self::with_builtins(numbering, config, Builtins::standard(numbering), setup)
    }

    pub fn with_builtins(
        numbering: Numbering,
        config: VmConfig,
        builtins: Builtins<BHost>,
        setup: impl FnOnce(&mut Asm),
    ) -> Self {
        let mut asm = Asm::new();
        // The globals many builtins touch.
        for (name, ty) in [("self", 4), ("other", 4), ("time", 2)] {
            asm.global(name, ty, &[]);
        }
        for name in ["v_forward", "v_right", "v_up"] {
            asm.global(name, 3, &[]);
        }
        setup(&mut asm);
        for (name, number) in known_builtins(numbering) {
            asm.builtin(name, number.unwrap_or(0), 8);
        }
        let program = Arc::new(Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap());
        let vm = Vm::new(program, Arc::new(builtins), config).unwrap();
        Self { vm, host: BHost::default() }
    }

    /// A setup that declares extra name-resolved builtins (names FTE's dump does not list).
    pub fn named(extra: &'static [&'static str]) -> impl FnOnce(&mut Asm) {
        move |asm: &mut Asm| {
            for name in extra {
                asm.builtin(name, 0, 8);
            }
        }
    }

    pub fn func(&self, name: &str) -> FuncRef {
        self.vm.find_function(name).unwrap_or_else(|| panic!("no builtin stub {name}"))
    }

    /// Calls builtin (or function) `name` with `args`.
    pub fn call(&mut self, name: &str, args: &[Arg<'_>]) -> Result<Ret, VmError> {
        let f = self.func(name);
        self.vm.call(&mut self.host, f, args)
    }

    /// Calls and returns the result as string text.
    pub fn s(&mut self, name: &str, args: &[Arg<'_>]) -> Vec<u8> {
        let r = self.call(name, args).unwrap_or_else(|e| panic!("{name}: {e}"));
        self.vm.str(r.str_ref()).to_vec()
    }

    /// Calls and returns the result as a float.
    pub fn f(&mut self, name: &str, args: &[Arg<'_>]) -> f32 {
        self.call(name, args).unwrap_or_else(|e| panic!("{name}: {e}")).f32()
    }

    /// Calls and returns the result as a vector.
    pub fn v(&mut self, name: &str, args: &[Arg<'_>]) -> [f32; 3] {
        self.call(name, args).unwrap_or_else(|e| panic!("{name}: {e}")).vec()
    }

    /// Calls and returns the first result word as an integer.
    pub fn i(&mut self, name: &str, args: &[Arg<'_>]) -> i32 {
        self.call(name, args).unwrap_or_else(|e| panic!("{name}: {e}")).i32()
    }
}

/// A string argument (passed as a new temp string).
pub fn s(text: &str) -> Arg<'_> {
    Arg::Bytes(text.as_bytes())
}

/// A byte-string argument.
pub fn b(text: &[u8]) -> Arg<'_> {
    Arg::Bytes(text)
}

/// A float argument.
pub fn f(x: f32) -> Arg<'static> {
    Arg::Float(x)
}

/// An integer argument.
pub fn i(x: i32) -> Arg<'static> {
    Arg::Int(x)
}

/// A vector argument.
pub fn v(x: f32, y: f32, z: f32) -> Arg<'static> {
    Arg::Vector([x, y, z])
}
