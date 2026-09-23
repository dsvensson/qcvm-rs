// SPDX-License-Identifier: MIT OR Apache-2.0

//! The virtual machine.

mod autocvars;
mod config;
pub(crate) mod core;
pub(crate) mod heap;
pub(crate) mod interp;
pub(crate) mod memory;
pub(crate) mod multiprogs;
pub(crate) mod num;
pub(crate) mod strings;
pub(crate) mod threads;

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

pub use config::{CharScheme, Charset, FteCompat, Limits, SpawnDefault, VmConfig, VmKind};
pub use interp::StateOp;
pub use strings::GcStats;

use self::core::{
    Callee, Core, Exec, FieldTable, NO_FUNCTION, OFS_PARM0, OFS_RETURN, ProgsState, Rng, SpawnFill,
};
use self::interp::Exit;
use self::memory::Memory;
use self::strings::{StrKind, Strings, classify};
use crate::builtins::Builtins;
use crate::bytes::usize_from;
use crate::error::{Backtrace, ErrorKind, Resource, VmError, WarningKind};
use crate::host::Host;
use crate::progs::{FunctionKind, Program, Type};
use crate::value::{Arg, EntRef, Field, FuncRef, Global, PrNum, Ptr, QcValue, Ret, StrRef, Vec3};

/// Why a global or field lookup failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookupError {
    /// No definition with that name.
    NotFound,
    /// The definition has a type incompatible with the requested value type.
    WrongType(Type),
}

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("no such definition"),
            Self::WrongType(t) => write!(f, "definition has type {t:?}"),
        }
    }
}

impl std::error::Error for LookupError {}

/// Whether execution continues after a builtin returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Return,
}

/// A QuakeC builtin that the VM could not bind (reported by [`Vm::unbound_builtins`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnboundBuiltin {
    /// The function.
    pub function: FuncRef,
    /// Its builtin number (0 for name-resolved builtins).
    pub number: u32,
    /// Its name.
    pub name: Box<[u8]>,
}

/// Information about the builtin being executed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinInfo<'a> {
    /// The function reference QuakeC called.
    pub function: FuncRef,
    /// The builtin number (0 for name-resolved builtins).
    pub number: u32,
    /// The name of the builtin's declaration in the progs.
    pub name: &'a [u8],
}

const fn align_up(v: usize, align: usize) -> Option<usize> {
    match v.checked_add(align.wrapping_sub(1)) {
        Some(x) => Some(x & !align.wrapping_sub(1)),
        None => None,
    }
}

/// A QuakeC virtual machine, generic over the host state its builtins receive.
///
/// See the crate documentation for an overview.
pub struct Vm<H> {
    pub(crate) core: Core,
    main: Arc<Program>,
    builtins: Arc<Builtins<H>>,
    started: Instant,
    realtime: Option<f64>,
}

impl<H: Host> fmt::Debug for Vm<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vm")
            .field("progs", &self.core.progs.len())
            .field("num_edicts", &self.core.mem.num_edicts())
            .field("frames", &self.core.frames.len())
            .finish_non_exhaustive()
    }
}

// A VM moves between threads with its host (engines run it off their render thread): keep it
// `Send` for any `Send` host.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<Vm<crate::NullHost>>();
};

impl<H: Host> Vm<H> {
    /// Creates a VM running `program` with the given builtins.
    ///
    /// # Errors
    /// Fails if the configured limits cannot accommodate the program (for example, its entity
    /// fields do not fit the address space).
    pub fn new(
        program: Arc<Program>,
        builtins: Arc<Builtins<H>>,
        config: VmConfig,
    ) -> Result<Self, VmError> {
        let core = build_core(Arc::clone(&program), &builtins, config)?;
        Ok(Self { core, main: program, builtins, started: Instant::now(), realtime: None })
    }

    /// Restores the VM to its freshly loaded state: globals re-initialised, entities and temp
    /// strings dropped.
    ///
    /// # Errors
    /// Fails only if rebuilding the memory fails (see [`Vm::new`]).
    pub fn reset(&mut self) -> Result<(), VmError> {
        let program = Arc::clone(&self.main);
        self.core = build_core(program, &self.builtins, self.core.config.clone())?;
        Ok(())
    }

    /// The main program.
    #[must_use]
    pub fn program(&self) -> &Arc<Program> {
        &self.main
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &VmConfig {
        &self.core.config
    }

    /// Changes whether builtin errors are downgraded to warnings (FTE's `developer`).
    pub fn set_developer(&mut self, on: bool) {
        self.core.config.developer = on;
    }

    /// Sets the host clock used for entity-reuse decisions (seconds). Until called, the VM uses
    /// the time since it was created.
    pub fn set_realtime(&mut self, seconds: f64) {
        self.realtime = Some(seconds);
    }

    /// The host clock (see [`Vm::set_realtime`]).
    #[must_use]
    pub fn realtime(&self) -> f64 {
        self.realtime.unwrap_or_else(|| self.started.elapsed().as_secs_f64())
    }

    /// Builtin stubs of the loaded progs that no registered builtin satisfies. Calling one of
    /// these from QuakeC fails with [`ErrorKind::BuiltinNotImplemented`].
    #[must_use]
    pub fn unbound_builtins(&self) -> Vec<UnboundBuiltin> {
        let mut out = Vec::new();
        for (pr, ps) in self.core.progs.iter().enumerate() {
            for (index, callee) in ps.callees.iter().enumerate() {
                if *callee != Callee::Missing {
                    continue;
                }
                let index = u32::try_from(index).unwrap_or(u32::MAX);
                let Some(info) = ps.program.function(index) else { continue };
                let number = match info.kind {
                    FunctionKind::Builtin { number } => number,
                    _ => 0,
                };
                out.push(UnboundBuiltin {
                    function: FuncRef::new(PrNum(u8::try_from(pr).unwrap_or(0)), index),
                    number,
                    name: info.name.into(),
                });
            }
        }
        out
    }

    /// The unbound builtins the loaded progs' code calls ([`Program::called_builtins`]): what a
    /// host must still provide before it can run them. Best effort, like `called_builtins`.
    #[must_use]
    pub fn reachable_unbound_builtins(&self) -> Vec<UnboundBuiltin> {
        let called: Vec<Vec<u32>> =
            self.core.progs.iter().map(|p| p.program.called_builtins()).collect();
        let reachable = |u: &UnboundBuiltin| {
            called
                .get(usize::from(u.function.progs().0))
                .is_some_and(|c| c.binary_search(&u.function.index()).is_ok())
        };
        self.unbound_builtins().into_iter().filter(reachable).collect()
    }

    // ---- lookups ------------------------------------------------------------------------

    /// Looks up a function of the main progs by name, as QuakeC sees it now: a function-typed
    /// global of that name supplies its current value (QuakeC may have redirected it, to a
    /// function of another progs too, or cleared it, which gives `None`); otherwise the function
    /// of that name.
    #[must_use]
    pub fn find_function(&self, name: impl AsRef<[u8]>) -> Option<FuncRef> {
        multiprogs::find_live_function(&self.core, 0, name.as_ref())
    }

    /// A typed handle to a global of the main progs.
    ///
    /// # Errors
    /// Fails if there is no such global or its type does not hold a `T`.
    pub fn global<T: QcValue>(&self, name: impl AsRef<[u8]>) -> Result<Global<T>, LookupError> {
        let ps = self.core.progs.first().ok_or(LookupError::NotFound)?;
        let def = ps.program.global_def(name).ok_or(LookupError::NotFound)?;
        if !T::accepts(def.ty) {
            return Err(LookupError::WrongType(def.ty));
        }
        let addr = def.offset.checked_mul(4).and_then(|o| o.checked_add(ps.gbase));
        Ok(Global::new(addr.ok_or(LookupError::NotFound)?))
    }

    /// A typed handle to an entity field.
    ///
    /// # Errors
    /// Fails if there is no such field or its type does not hold a `T`.
    pub fn field<T: QcValue>(&self, name: impl AsRef<[u8]>) -> Result<Field<T>, LookupError> {
        let entry = self.core.fields.get(name.as_ref()).ok_or(LookupError::NotFound)?;
        if !T::accepts(entry.ty) {
            return Err(LookupError::WrongType(entry.ty));
        }
        Ok(Field::new(entry.ofs))
    }

    /// A typed handle to an entity field, adding the field if the progs does not define it.
    ///
    /// Added fields use space reserved per entity ([`VmConfig::field_reserve_bytes`]).
    ///
    /// # Errors
    /// Fails if the field exists with an incompatible type, or the reserve is exhausted.
    pub fn ensure_field<T: QcValue>(
        &mut self,
        name: impl AsRef<[u8]>,
    ) -> Result<Field<T>, LookupError> {
        let name = name.as_ref();
        match self.field::<T>(name) {
            Err(LookupError::NotFound) => {}
            other => return other,
        }
        let words = u32::try_from(T::WORDS).unwrap_or(3);
        let ofs = self.core.fields.words;
        let new_words = ofs.checked_add(words).ok_or(LookupError::NotFound)?;
        let bytes = new_words.checked_mul(4).ok_or(LookupError::NotFound)?;
        if bytes > 1u32.checked_shl(self.core.mem.stride_shift).unwrap_or(0) {
            return Err(LookupError::NotFound);
        }
        self.core.fields.push(name, T::TYPE, ofs);
        self.core.fields.words = new_words;
        self.core.mem.field_bytes = bytes;
        Ok(Field::new(ofs))
    }

    // ---- calling QuakeC ---------------------------------------------------------------

    /// Calls a QuakeC function (or a builtin) with arguments and returns its result.
    ///
    /// May be called from inside builtins (re-entrantly). Builtins should read their own
    /// arguments before calling back into QuakeC and set their return value afterwards, because
    /// the parameter and return slots are shared.
    ///
    /// # Errors
    /// Any runtime error raised while the function runs; the VM stays usable.
    pub fn call(&mut self, host: &mut H, f: FuncRef, args: &[Arg<'_>]) -> Result<Ret, VmError> {
        if self.core.nesting >= self.core.config.limits.reentry {
            return Err(ErrorKind::Reentrancy.into());
        }
        let pr = f.progs().0;
        let callee = self
            .core
            .progs
            .get(usize::from(pr))
            .and_then(|p| p.callees.get(usize_from(f.index())).copied());
        let top_level = self.core.nesting == 0;
        if top_level {
            self.core.warnings_this_call = 0;
            self.core.suppressed = 0;
        }
        let saved = (self.core.argc, self.core.builtin);
        // QuakeC callees take their arguments from the calling context's PARM slots (entering a
        // function of another progs copies them over); builtins read their own progs' slots.
        let (args_pr, ret_pr) = match callee {
            Some(Callee::Qc) => (self.core.x.prnum, self.core.x.prnum),
            _ => (pr, pr),
        };
        self.write_args(args_pr, args)?;
        self.core.argc = u32::try_from(args.len()).unwrap_or(u32::MAX);
        self.core.nesting = self.core.nesting.saturating_add(1);

        let result = match callee {
            Some(Callee::Qc) => {
                let depth = self.core.frames.len();
                match self.core.enter(pr, f.index(), self.core.x.pc) {
                    Ok(()) => self.execute(host, depth),
                    Err(kind) => Err(VmError::from(kind).with_backtrace(self.core.backtrace())),
                }
            }
            Some(Callee::Builtin(slot)) => {
                let saved_prnum = self.core.x.prnum;
                self.core.x.prnum = pr;
                let r = self.run_builtin(host, slot, f, self.core.frames.len()).map(|_| ());
                self.core.x.prnum = saved_prnum;
                r
            }
            Some(Callee::Null) => Err(ErrorKind::NullFunction.into()),
            Some(Callee::Missing) => {
                Err(VmError::from(missing(&self.core, f)).with_backtrace(self.core.backtrace()))
            }
            Some(Callee::Invalid) | None => Err(ErrorKind::InvalidFunction(f).into()),
        };

        self.core.nesting = self.core.nesting.saturating_sub(1);
        (self.core.argc, self.core.builtin) = saved;
        let ret = self.core.abort_ret.take().unwrap_or_else(|| self.read_return(ret_pr));
        if top_level {
            if self.core.suppressed > 0 {
                let n = self.core.suppressed;
                self.core.warnings.push(crate::error::Warning {
                    kind: WarningKind::Suppressed(n),
                    backtrace: Backtrace::default(),
                });
            }
            self.flush_warnings(host);
            if self.core.strings.wants_collection() {
                self.collect_garbage_now();
            }
        }
        result.map(|()| Ret(ret))
    }

    /// Calls a function with `self` set to `this`, restoring `self` afterwards.
    ///
    /// # Errors
    /// See [`Vm::call`].
    pub fn call_as(
        &mut self,
        host: &mut H,
        this: EntRef,
        f: FuncRef,
        args: &[Arg<'_>],
    ) -> Result<Ret, VmError> {
        match self.global::<EntRef>("self") {
            Ok(self_g) => self.call_as_with(host, self_g, this, f, args),
            Err(_) => self.call(host, f, args),
        }
    }

    /// [`Vm::call_as`] with the `self` global already looked up (hosts that call often resolve
    /// it once).
    ///
    /// # Errors
    /// See [`Vm::call`].
    pub fn call_as_with(
        &mut self,
        host: &mut H,
        self_g: Global<EntRef>,
        this: EntRef,
        f: FuncRef,
        args: &[Arg<'_>],
    ) -> Result<Ret, VmError> {
        let saved = self.get(self_g);
        self.set(self_g, this);
        let r = self.call(host, f, args);
        self.set(self_g, saved);
        r
    }

    /// Calls the function stored in an entity's function field (the `think`/`touch`/`predraw`
    /// pattern) with `self` set to the entity. Returns `Ok(None)` if the field holds no function.
    ///
    /// # Errors
    /// See [`Vm::call`].
    pub fn call_field_fn(
        &mut self,
        host: &mut H,
        e: EntRef,
        field: Field<FuncRef>,
        args: &[Arg<'_>],
    ) -> Result<Option<Ret>, VmError> {
        match self.get_field(e, field) {
            Some(f) if f.index() != 0 => self.call_as(host, e, f, args).map(Some),
            _ => Ok(None),
        }
    }

    fn write_args(&mut self, pr: u8, args: &[Arg<'_>]) -> Result<(), VmError> {
        if args.len() > 8 {
            return Err(ErrorKind::TooManyArguments(args.len()).into());
        }
        let gbase = self.core.progs.get(usize::from(pr)).map_or(0, |p| usize_from(p.gbase));
        for (i, arg) in args.iter().enumerate() {
            let words = match *arg {
                Arg::Float(v) => [v.to_bits(), 0, 0],
                Arg::Vector(v) => v.map(f32::to_bits),
                Arg::Int(v) => [v.cast_unsigned(), 0, 0],
                Arg::Ent(e) => [e.0, 0, 0],
                Arg::Str(s) => [s.0, 0, 0],
                Arg::Bytes(b) => [self.new_temp(b)?, 0, 0],
                Arg::Func(f) => [f.0, 0, 0],
                Arg::Raw(w) => w,
            };
            let at = gbase.wrapping_add(OFS_PARM0).wrapping_add(i.wrapping_mul(12));
            for (k, w) in words.into_iter().enumerate() {
                self.core.mem.set_g(at.wrapping_add(k.wrapping_mul(4)), w);
            }
        }
        Ok(())
    }

    fn read_return(&self, pr: u8) -> [u32; 3] {
        let gbase = self.core.progs.get(usize::from(pr)).map_or(0, |p| usize_from(p.gbase));
        let at = gbase.wrapping_add(OFS_RETURN);
        [0usize, 4, 8].map(|k| self.core.mem.g(at.wrapping_add(k)))
    }

    /// Runs the interpreter until the frame stack is back at `exit_depth`.
    fn execute(&mut self, host: &mut H, exit_depth: usize) -> Result<(), VmError> {
        let saved_entry = std::mem::replace(&mut self.core.entry_depth, exit_depth);
        let result = self.execute_inner(host, exit_depth);
        self.core.entry_depth = saved_entry;
        result
    }

    fn execute_inner(&mut self, host: &mut H, exit_depth: usize) -> Result<(), VmError> {
        let mut budget = self.core.config.limits.runaway;
        loop {
            let exit = interp::run(&mut self.core, exit_depth, &mut budget);
            match exit {
                Exit::Returned => {
                    self.flush_warnings(host);
                    return Ok(());
                }
                Exit::Builtin { slot, func } => {
                    self.flush_warnings(host);
                    if self.run_builtin(host, slot, func, exit_depth)? == Flow::Return {
                        return Ok(());
                    }
                }
                Exit::StateOp(op) => {
                    let handled = match host.state_op(self, op) {
                        Ok(h) => h,
                        Err(e) => return Err(self.fail(e, exit_depth)),
                    };
                    if !handled {
                        self.default_state_op(op);
                    }
                }
                Exit::Fault(kind) => {
                    self.flush_warnings(host);
                    return Err(self.fail(kind.into(), exit_depth));
                }
            }
        }
    }

    /// Calls builtin `slot`, applying FTE's builtin-error rules.
    fn run_builtin(
        &mut self,
        host: &mut H,
        slot: u32,
        func: FuncRef,
        exit_depth: usize,
    ) -> Result<Flow, VmError> {
        let Some(f) = self.builtins.func(slot) else {
            return Err(self.fail(missing(&self.core, func).into(), exit_depth));
        };
        self.core.builtin = func;
        match f(self, host) {
            Ok(()) => Ok(Flow::Continue),
            Err(e) if matches!(e.control(), Some(crate::error::Control::Abort(_))) => {
                // `abort(ret)`: unwind to this engine boundary and return normally from it.
                if let Some(crate::error::Control::Abort(ret)) = e.control() {
                    self.core.abort_ret = Some(ret);
                }
                self.core.unwind(exit_depth);
                Ok(Flow::Return)
            }
            Err(e) if self.core.config.developer && matches!(e.kind(), ErrorKind::Builtin(_)) => {
                // FTE's developer mode: a builtin error is only a warning, with a zero result.
                let msg = e.kind().to_string();
                self.core.warn(WarningKind::Builtin(msg));
                self.ret_raw([0; 3]);
                self.flush_warnings(host);
                Ok(Flow::Continue)
            }
            Err(e) => Err(self.fail(e, exit_depth)),
        }
    }

    /// Adds a backtrace to `e` and unwinds to `depth`.
    fn fail(&mut self, e: VmError, depth: usize) -> VmError {
        let e = e.with_backtrace(self.core.backtrace());
        self.core.unwind(depth);
        e
    }

    fn flush_warnings(&mut self, host: &mut H) {
        for w in std::mem::take(&mut self.core.warnings) {
            host.warning(&w);
        }
    }

    /// FTE's CSQC behaviour for the animation opcodes.
    fn default_state_op(&mut self, op: StateOp) {
        let h = self.core.progs.get(usize::from(self.core.x.prnum)).map(|p| p.state);
        let Some(h) = h else { return };
        let (Some(self_g), Some(time_g)) = (h.self_g, h.time_g) else {
            self.core.warn(WarningKind::Builtin("state opcode needs `self` and `time`".into()));
            return;
        };
        let time = f32::from_bits(self.core.mem.g(usize_from(time_g)));
        let this = self.core.mem.g(usize_from(self_g));
        let step = self.core.config.state_step;
        let set = |core: &mut Core, e: u32, field: Option<u32>, v: u32| {
            if let Some(f) = field
                && let Some(o) = core.mem.field_offset(e, f, 1)
            {
                core.mem.set_ent_word(o, v);
            }
        };
        match op {
            StateOp::State { frame, think } => {
                set(&mut self.core, this, h.nextthink_f, (time + step).to_bits());
                set(&mut self.core, this, h.think_f, think.0);
                set(&mut self.core, this, h.frame_f, frame.to_bits());
            }
            StateOp::CState { first, last, func } | StateOp::CWState { first, last, func } => {
                set(&mut self.core, this, h.nextthink_f, (time + step).to_bits());
                set(&mut self.core, this, h.think_f, func.0);
                if let Some(g) = h.cycle_wrapped_g {
                    self.core.mem.set_g(usize_from(g), 0);
                }
                let frame_f =
                    if matches!(op, StateOp::CState { .. }) { h.frame_f } else { h.weaponframe_f };
                let Some(ff) = frame_f else { return };
                let Some(o) = self.core.mem.field_offset(this, ff, 1) else { return };
                let cur = f32::from_bits(self.core.mem.ent_word(o));
                let (lo, hi, step) =
                    if first > last { (last, first, -1.0) } else { (first, last, 1.0) };
                let next = if cur < lo || cur > hi {
                    first
                } else {
                    let n = cur + step;
                    if n < lo || n > hi {
                        if let Some(g) = h.cycle_wrapped_g {
                            self.core.mem.set_gf(usize_from(g), 1.0);
                        }
                        first
                    } else {
                        n
                    }
                };
                self.core.mem.set_ent_word(o, next.to_bits());
            }
            StateOp::ThinkTime { ent, delay } => {
                let e = if ent.0 < self.core.mem.num_edicts() { ent.0 } else { 0 };
                set(&mut self.core, e, h.nextthink_f, (time + delay).to_bits());
            }
        }
    }

    /// Runs a garbage collection of temp strings now.
    ///
    /// # Errors
    /// Fails while QuakeC is executing (collecting then could free strings that builtins
    /// further up the call stack still hold).
    pub fn collect_garbage(&mut self) -> Result<GcStats, VmError> {
        if self.core.nesting > 0 {
            return Err(VmError::host("cannot collect garbage while QuakeC is running"));
        }
        Ok(self.collect_garbage_now())
    }

    fn collect_garbage_now(&mut self) -> GcStats {
        let mem = &self.core.mem;
        let mut roots = vec![&mem.s[..], &mem.e[..], &mem.heap.data[..]];
        roots.extend(self.core.threads.iter().map(threads::Thread::root_bytes));
        self.core.strings.collect(roots)
    }

    // ---- data access --------------------------------------------------------------------

    /// Reads a global.
    #[must_use]
    pub fn get<T: QcValue>(&self, g: Global<T>) -> T {
        let at = usize_from(g.addr);
        let mut buf = [0u32; 3];
        for (k, w) in buf.iter_mut().enumerate().take(T::WORDS) {
            *w = self.core.mem.g(at.wrapping_add(k.wrapping_mul(4)));
        }
        T::from_words(buf.get(..T::WORDS).unwrap_or(&buf))
    }

    /// Writes a global.
    pub fn set<T: QcValue>(&mut self, g: Global<T>, v: T) {
        let at = usize_from(g.addr);
        for (k, w) in v.to_words().into_iter().take(T::WORDS).enumerate() {
            self.core.mem.set_g(at.wrapping_add(k.wrapping_mul(4)), w);
        }
    }

    /// Reads an entity field. `None` if the entity does not exist.
    #[must_use]
    pub fn get_field<T: QcValue>(&self, e: EntRef, f: Field<T>) -> Option<T> {
        let words = u32::try_from(T::WORDS).ok()?;
        let at = self.core.mem.field_offset(e.0, f.ofs, words)?;
        // Only the value's own words are in range; the rest of the array is not read.
        let mut buf = [0u32; 3];
        for (k, w) in buf.iter_mut().enumerate().take(T::WORDS) {
            *w = self.core.mem.ent_word(at.wrapping_add(k.wrapping_mul(4)));
        }
        Some(T::from_words(buf.get(..T::WORDS).unwrap_or(&buf)))
    }

    /// Writes an entity field (ignoring write protection, which only applies to QuakeC).
    /// Returns `false` if the entity does not exist.
    pub fn set_field<T: QcValue>(&mut self, e: EntRef, f: Field<T>, v: T) -> bool {
        let Ok(words) = u32::try_from(T::WORDS) else { return false };
        let Some(at) = self.core.mem.field_offset(e.0, f.ofs, words) else { return false };
        for (k, w) in v.to_words().into_iter().take(T::WORDS).enumerate() {
            self.core.mem.set_ent_word(at.wrapping_add(k.wrapping_mul(4)), w);
        }
        true
    }

    /// Reads `len` bytes of VM memory at `p`.
    #[must_use]
    pub fn read_mem(&self, p: Ptr, len: usize) -> Option<Vec<u8>> {
        (0..len)
            .map(|i| {
                let at = p.0.checked_add(u32::try_from(i).ok()?)?;
                self.core.mem.read::<1>(at).map(|[b]| b)
            })
            .collect()
    }

    /// Writes bytes into VM memory at `p`. Returns `false` if the range is not writable.
    pub fn write_mem(&mut self, p: Ptr, bytes: &[u8]) -> bool {
        self.core.mem.write(p.0, bytes).is_ok()
    }

    // ---- entities -----------------------------------------------------------------------

    /// Allocates an entity with all fields zeroed, following FTE's reuse policy.
    ///
    /// # Errors
    /// [`ErrorKind::NoFreeEdicts`] when every slot is in use.
    pub fn spawn(&mut self) -> Result<EntRef, VmError> {
        let now = self.realtime();
        let first = self.core.config.first_spawnable;
        let e = self.core.mem.spawn(now, first).map_err(VmError::from)?;
        self.core.apply_spawn_defaults(e);
        Ok(EntRef(e))
    }

    /// Frees an entity. `instant` makes its slot reusable immediately.
    ///
    /// Only the configured fields ([`VmConfig::remove_clears`]) are zeroed; the others stay
    /// readable until the slot is reused, as in FTE.
    pub fn remove(&mut self, e: EntRef, instant: bool) {
        let now = self.realtime();
        let clears = self.core.remove_clears.clone();
        if let Err(w) = self.core.mem.remove(e.0, now, instant, &clears) {
            self.core.warn(w);
        }
    }

    /// Whether entity `e` is allocated and in use.
    #[must_use]
    pub fn is_in_use(&self, e: EntRef) -> bool {
        self.core.mem.in_use(e.0)
    }

    /// Number of entity slots allocated so far (the high-water mark, including the world).
    #[must_use]
    pub fn num_edicts(&self) -> u32 {
        self.core.mem.num_edicts()
    }

    /// Entities in use, in slot order (including the world).
    pub fn entities(&self) -> impl Iterator<Item = EntRef> + '_ {
        (0..self.num_edicts()).filter(|&e| self.core.mem.in_use(e)).map(EntRef)
    }

    /// Protects an entity against writes from QuakeC (FTE makes the world read-only once a CSQC
    /// map is loaded). Returns the previous setting.
    pub fn set_protected(&mut self, e: EntRef, protected: bool) -> bool {
        match self.core.mem.slots.get_mut(usize_from(e.0)) {
            Some(slot) => std::mem::replace(&mut slot.protected, protected),
            None => false,
        }
    }

    /// Whether an entity is protected against QuakeC writes.
    #[must_use]
    pub fn is_protected(&self, e: EntRef) -> bool {
        self.core.mem.protected(e.0)
    }

    /// How many times entity slot `e` has been allocated (`None` beyond the entity table). The
    /// slot keeps the number while free, and the next spawn into it increments it, so
    /// `(e, serial)` names one entity for as long as it lives: a key an engine can hold on to
    /// without being fooled by reuse.
    #[must_use]
    pub fn serial(&self, e: EntRef) -> Option<u32> {
        self.core.mem.slots.get(usize_from(e.0)).map(|s| s.serial)
    }

    // ---- strings ------------------------------------------------------------------------

    /// The text of a string reference; empty for null or invalid references.
    #[must_use]
    pub fn str(&self, s: StrRef) -> &[u8] {
        self.core.str_bytes(s.0).unwrap_or_default()
    }

    /// The text of a string reference, or `None` if it resolves to nothing.
    #[must_use]
    pub fn str_checked(&self, s: StrRef) -> Option<&[u8]> {
        self.core.str_bytes(s.0)
    }

    fn new_temp(&mut self, bytes: &[u8]) -> Result<u32, VmError> {
        self.core.strings.alloc(bytes).map_err(|r| ErrorKind::OutOfMemory(r).into())
    }

    /// Creates a temp string. It lives as long as something in VM memory refers to it (or it is
    /// pinned); references held only by the host become invalid after the next top-level call.
    ///
    /// # Errors
    /// [`ErrorKind::OutOfMemory`] if the temp-string limits are reached.
    pub fn temp(&mut self, bytes: &[u8]) -> Result<StrRef, VmError> {
        self.new_temp(bytes).map(StrRef)
    }

    /// Interns a permanent string (never collected; identical text yields the same reference).
    pub fn intern(&mut self, bytes: &[u8]) -> StrRef {
        StrRef(self.core.strings.intern(bytes))
    }

    /// Keeps a temp string alive until [`Vm::unpin`] (pins nest).
    pub fn pin(&mut self, s: StrRef) {
        self.core.strings.pin(s.0);
    }

    /// Releases a pin taken with [`Vm::pin`].
    pub fn unpin(&mut self, s: StrRef) {
        self.core.strings.unpin(s.0);
    }

    /// Number of live temp strings.
    #[must_use]
    pub fn temp_strings(&self) -> usize {
        self.core.strings.live()
    }

    // ---- builtin support ----------------------------------------------------------------

    /// Number of arguments passed to the current builtin.
    #[must_use]
    pub fn argc(&self) -> usize {
        usize_from(self.core.argc)
    }

    /// The builtin being executed.
    #[must_use]
    pub fn builtin(&self) -> BuiltinInfo<'_> {
        let f = self.core.builtin;
        let info = self
            .core
            .progs
            .get(usize::from(f.progs().0))
            .and_then(|p| p.program.function(f.index()));
        BuiltinInfo {
            function: f,
            number: match info.map(|i| i.kind) {
                Some(FunctionKind::Builtin { number }) => number,
                _ => 0,
            },
            name: info.map(|i| i.name).unwrap_or_default(),
        }
    }

    fn parm(&self, i: usize, k: usize) -> u32 {
        let at = self
            .core
            .gbase()
            .wrapping_add(OFS_PARM0)
            .wrapping_add(i.wrapping_mul(12))
            .wrapping_add(k.wrapping_mul(4));
        if i >= 8 { 0 } else { self.core.mem.g(at) }
    }

    /// Argument `i` (0–7) as raw words.
    #[must_use]
    pub fn arg_raw(&self, i: usize) -> [u32; 3] {
        [self.parm(i, 0), self.parm(i, 1), self.parm(i, 2)]
    }

    /// Argument `i` as a float.
    #[must_use]
    pub fn arg_f32(&self, i: usize) -> f32 {
        f32::from_bits(self.parm(i, 0))
    }

    /// Argument `i` as an integer.
    #[must_use]
    pub fn arg_i32(&self, i: usize) -> i32 {
        self.parm(i, 0).cast_signed()
    }

    /// Argument `i` as an unsigned integer (or any raw word).
    #[must_use]
    pub fn arg_u32(&self, i: usize) -> u32 {
        self.parm(i, 0)
    }

    /// Argument `i` as a vector.
    #[must_use]
    pub fn arg_vec(&self, i: usize) -> Vec3 {
        self.arg_raw(i).map(f32::from_bits)
    }

    /// Argument `i` as an entity.
    #[must_use]
    pub fn arg_ent(&self, i: usize) -> EntRef {
        EntRef(self.parm(i, 0))
    }

    /// Argument `i` as a function.
    #[must_use]
    pub fn arg_func(&self, i: usize) -> FuncRef {
        FuncRef(self.parm(i, 0))
    }

    /// Argument `i` as a string reference.
    #[must_use]
    pub fn arg_str_ref(&self, i: usize) -> StrRef {
        StrRef(self.parm(i, 0))
    }

    /// Argument `i` as a pointer.
    #[must_use]
    pub fn arg_ptr(&self, i: usize) -> Ptr {
        Ptr(self.parm(i, 0))
    }

    /// Argument `i` as string text (empty for null or invalid references).
    #[must_use]
    pub fn arg_str(&self, i: usize) -> &[u8] {
        self.str(self.arg_str_ref(i))
    }

    /// Sets the return value to raw words.
    pub fn ret_raw(&mut self, words: [u32; 3]) {
        let at = self.core.gbase().wrapping_add(OFS_RETURN);
        for (k, w) in words.into_iter().enumerate() {
            self.core.mem.set_g(at.wrapping_add(k.wrapping_mul(4)), w);
        }
    }

    /// Returns a float.
    pub fn ret_f32(&mut self, v: f32) {
        self.ret_raw([v.to_bits(), 0, 0]);
    }

    /// Returns an integer.
    pub fn ret_i32(&mut self, v: i32) {
        self.ret_raw([v.cast_unsigned(), 0, 0]);
    }

    /// Returns a vector.
    pub fn ret_vec(&mut self, v: Vec3) {
        self.ret_raw(v.map(f32::to_bits));
    }

    /// Returns an entity.
    pub fn ret_ent(&mut self, e: EntRef) {
        self.ret_raw([e.0, 0, 0]);
    }

    /// Returns a function.
    pub fn ret_func(&mut self, f: FuncRef) {
        self.ret_raw([f.0, 0, 0]);
    }

    /// Returns a string reference.
    pub fn ret_str_ref(&mut self, s: StrRef) {
        self.ret_raw([s.0, 0, 0]);
    }

    /// Returns new text as a temp string.
    ///
    /// # Errors
    /// [`ErrorKind::OutOfMemory`] if the temp-string limits are reached.
    pub fn ret_str(&mut self, bytes: &[u8]) -> Result<(), VmError> {
        let r = self.new_temp(bytes)?;
        self.ret_raw([r, 0, 0]);
        Ok(())
    }

    /// Records a warning (reported to [`Host::warning`] with a backtrace).
    pub fn warn(&mut self, msg: impl Into<String>) {
        self.core.warn(WarningKind::Builtin(msg.into()));
    }

    /// The current QuakeC call stack, innermost first.
    #[must_use]
    pub fn backtrace(&self) -> Backtrace {
        self.core.backtrace()
    }

    /// Whether `f` names a builtin this VM implements (`checkbuiltin`).
    #[must_use]
    pub fn is_builtin_bound(&self, f: FuncRef) -> bool {
        self.core
            .progs
            .get(usize::from(f.progs().0))
            .and_then(|p| p.callees.get(usize_from(f.index())))
            .is_some_and(|c| matches!(c, Callee::Builtin(_)))
    }

    /// Whether a string reference designates a temp string.
    #[must_use]
    pub fn is_temp(&self, s: StrRef) -> bool {
        matches!(classify(s.0), StrKind::Temp(_))
    }
}

/// The error for calling a builtin that is not bound.
fn missing(core: &Core, f: FuncRef) -> ErrorKind {
    let info = core.progs.get(usize::from(f.progs().0)).and_then(|p| p.program.function(f.index()));
    match info {
        Some(i) => ErrorKind::BuiltinNotImplemented {
            number: match i.kind {
                FunctionKind::Builtin { number } => number,
                _ => 0,
            },
            name: i.name.into(),
        },
        None => ErrorKind::InvalidFunction(f),
    }
}

/// Binds each function record of `program` to a callee.
pub(crate) fn bind<H: Host>(program: &Program, builtins: &Builtins<H>) -> Box<[Callee]> {
    program
        .functions()
        .map(|f| match f.kind {
            FunctionKind::Null => Callee::Null,
            FunctionKind::QuakeC { .. } => Callee::Qc,
            FunctionKind::Builtin { number } => {
                builtins.bind_number(number).map_or(Callee::Missing, Callee::Builtin)
            }
            FunctionKind::NamedBuiltin => {
                builtins.bind_name(f.name).map_or(Callee::Missing, Callee::Builtin)
            }
            FunctionKind::Invalid(_) => Callee::Invalid,
        })
        .collect()
}

/// Lays out memory for the main progs and initialises it.
fn build_core<H: Host>(
    program: Arc<Program>,
    builtins: &Builtins<H>,
    config: VmConfig,
) -> Result<Core, VmError> {
    let oom = |r| VmError::from(ErrorKind::OutOfMemory(r));
    let limits = &config.limits;

    // Region S: strings | globals + 3-word tail | local stack (+4 bytes, as FTE).
    let strings_len = program.strings.len();
    let gbase =
        align_up(strings_len.saturating_add(1), 4).ok_or_else(|| oom(Resource::ProgsArea))?;
    let globals_bytes = usize_from(program.num_globals()).saturating_add(3).saturating_mul(4);
    let ls_base = gbase.saturating_add(globals_bytes);
    let ls_words = limits.local_stack_words.max(16);
    let s_len = ls_base.saturating_add(usize_from(ls_words).saturating_mul(4)).saturating_add(4);
    let s_reserve = align_up(s_len, 1 << 20)
        .and_then(|v| v.checked_add(usize_from(limits.progs_area_bytes)))
        .ok_or_else(|| oom(Resource::ProgsArea))?;

    // Region E: a power-of-two stride with room for fields added later.
    let field_bytes = program.entity_fields.checked_mul(4).ok_or_else(|| oom(Resource::Fields))?;
    let stride = field_bytes
        .checked_add(config.field_reserve_bytes.max(field_bytes / 2))
        .and_then(|v| v.max(16).checked_next_power_of_two())
        .ok_or_else(|| oom(Resource::Fields))?;
    let stride_shift = stride.trailing_zeros();

    // Region H after the largest possible entity region; everything below 2^31 so pointers are
    // never mistaken for tagged string references.
    const LIMIT: u64 = 0x8000_0000;
    let e_base = u64::try_from(s_reserve).map_err(|_| oom(Resource::ProgsArea))?;
    let heap = u64::from(limits.heap_bytes);
    let fit = LIMIT.saturating_sub(e_base).saturating_sub(heap).saturating_sub(16) >> stride_shift;
    let max_edicts = u64::from(limits.max_edicts).min(fit);
    if max_edicts < 1 {
        return Err(oom(Resource::Entities));
    }
    let h_base = e_base
        .saturating_add(max_edicts << stride_shift)
        .checked_add(15)
        .map(|v| v & !15)
        .ok_or_else(|| oom(Resource::Heap))?;

    let mut s = Vec::new();
    s.try_reserve_exact(s_len).map_err(|_| oom(Resource::ProgsArea))?;
    s.resize(s_len, 0);
    if let Some(dst) = s.get_mut(..strings_len) {
        dst.copy_from_slice(&program.strings);
    }
    for (i, &w) in program.globals.iter().enumerate() {
        crate::bytes::put_u32(&mut s, gbase.saturating_add(i.saturating_mul(4)), w);
    }
    let gbase_u32 = u32::try_from(gbase).map_err(|_| oom(Resource::ProgsArea))?;

    let mut mem = Memory {
        s,
        e: Vec::new(),
        e_base: u32::try_from(e_base).map_err(|_| oom(Resource::ProgsArea))?,
        stride_shift,
        field_bytes,
        max_edicts: u32::try_from(max_edicts).unwrap_or(u32::MAX),
        slots: Vec::new(),
        heap: heap::Heap::new(limits.heap_bytes),
        h_base: u32::try_from(h_base).map_err(|_| oom(Resource::Heap))?,
        ls_base: u32::try_from(ls_base).map_err(|_| oom(Resource::ProgsArea))?,
        ls_words,
    };
    mem.init_world()?;

    // Load-time global fixups.
    for &word in program.pointer_relocs.iter() {
        let at = gbase.saturating_add(usize_from(word).saturating_mul(4));
        let v = mem.g(at);
        mem.set_g(at, (v & 0x7FFF_FFFF).wrapping_add(gbase_u32));
    }
    if let Some(word) = program.special.thisprogs {
        mem.set_g(gbase.saturating_add(usize_from(word).saturating_mul(4)), 0);
    }
    if let Some(word) = program.special.fasttrackarrays {
        let at = gbase.saturating_add(usize_from(word).saturating_mul(4));
        let one = match program.global_def("__ext__fasttrackarrays").map(|d| d.ty) {
            Some(Type::Float) => 1.0f32.to_bits(),
            _ => 1,
        };
        mem.set_g(at, one);
    }

    let mut fields = FieldTable { words: program.entity_fields, ..FieldTable::default() };
    for d in program.field_defs() {
        fields.push(d.name, d.ty, d.offset);
    }

    let state = multiprogs::state_handles(&program, gbase_u32, &fields);
    let field_ofs = |name: &str| fields.get(name.as_bytes()).map(|f| f.ofs);
    let remove_clears =
        config.remove_clears.iter().filter_map(|n| field_ofs(n)).collect::<Vec<_>>();
    let spawn_defaults = config
        .spawn_defaults
        .iter()
        .filter_map(|d| {
            let global = d.global.as_ref().and_then(|g| program.global_def(g)).map(|def| {
                usize_from(gbase_u32).saturating_add(usize_from(def.offset).saturating_mul(4))
            });
            Some(SpawnFill { field: field_ofs(&d.field)?, global, value: d.value.to_bits() })
        })
        .collect::<Vec<_>>();

    let callees = bind(&program, builtins);
    let progs = vec![ProgsState { program, gbase: gbase_u32, callees, state, shared: Vec::new() }];

    let mut core = Core {
        mem,
        strings: Strings::new(limits.temp_strings, limits.temp_string_bytes),
        progs,
        fields,
        frames: Vec::new(),
        x: Exec { func: NO_FUNCTION, ..Exec::default() },
        argc: 0,
        builtin: FuncRef::NULL,
        nesting: 0,
        rng: Rng::new(config.seed),
        warnings: Vec::new(),
        warnings_this_call: 0,
        suppressed: 0,
        trace: false,
        remove_clears,
        spawn_defaults,
        std: crate::stdlib::StdState::default(),
        abort_ret: None,
        shared: multiprogs::SharedTable::default(),
        entry_depth: 0,
        threads: Vec::new(),
        config,
    };
    multiprogs::register_shared(&mut core, 0);
    core.apply_spawn_defaults(0);
    Ok(core)
}
