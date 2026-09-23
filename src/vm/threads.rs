// SPDX-License-Identifier: MIT OR Apache-2.0

//! QuakeC threads: FTE's `sleep` and `fork`.
//!
//! A thread is a snapshot of the QuakeC call stack from the nearest engine boundary up to the
//! suspending builtin: the chain of activations, the local-stack segment they use (saved locals
//! and pushed memory) and the current values of their locals. Resuming rebuilds the same frames
//! (so the local-stack layout is identical), writes the captured words back and continues after
//! the suspending call. Threads only resume when no QuakeC is running.

use crate::bytes::usize_from;
use crate::error::{ErrorKind, Resource, VmError};
use crate::host::Host;
use crate::vm::Vm;
use crate::vm::core::{Core, OFS_RETURN, SwitchKind};

/// One function activation of a sleeping thread.
#[derive(Clone, Copy, Debug)]
struct Activation {
    prnum: u8,
    func: u32,
    /// Statement to continue at.
    pc: u32,
    pushed: u32,
    switch_ref: u32,
    switch_kind: SwitchKind,
}

/// A suspended QuakeC thread.
#[derive(Clone, Debug)]
pub(crate) struct Thread {
    /// Value of the `time` global at which the thread wakes.
    wake: f32,
    /// Root first.
    chain: Vec<Activation>,
    /// Captured local-stack bytes, then the captured locals of each function in `locals`.
    data: Vec<u8>,
    stack_len: usize,
    /// (absolute S offset, byte length) of each captured locals range, in `data` order.
    locals: Vec<(usize, usize)>,
    /// What the suspending call returns when the thread resumes.
    resume: [u32; 3],
    /// `self` and `other`: global offset, entity, spawn serial.
    selves: [Option<(usize, u32, u32)>; 2],
}

impl Thread {
    /// Everything the thread holds that may reference temp strings (a GC root).
    pub(crate) fn root_bytes(&self) -> &[u8] {
        &self.data
    }
}

fn global_offset(core: &Core, prnum: u8, name: &str) -> Option<usize> {
    let ps = core.progs.get(usize::from(prnum))?;
    let d = ps.program.global_def(name)?;
    usize_from(ps.gbase).checked_add(usize_from(d.offset).checked_mul(4)?)
}

/// Captures the running QuakeC thread (from the nearest engine boundary). Returns `None` if no
/// QuakeC function is running.
fn snapshot(core: &Core, wake: f32, resume: [u32; 3]) -> Option<Thread> {
    let depth = core.entry_depth;
    let root_frame = core.frames.get(depth)?;
    let mut chain: Vec<Activation> = core
        .frames
        .get(depth.checked_add(1)?..)
        .unwrap_or_default()
        .iter()
        .map(|f| Activation {
            prnum: f.prnum,
            func: f.func,
            pc: f.resume_pc,
            pushed: f.pushed,
            switch_ref: f.switch_ref,
            switch_kind: f.switch_kind,
        })
        .collect();
    chain.push(Activation {
        prnum: core.x.prnum,
        func: core.x.func,
        pc: core.x.pc,
        pushed: core.x.pushed,
        switch_ref: core.x.switch_ref,
        switch_kind: core.x.switch_kind,
    });

    let ls_base = usize_from(core.mem.ls_base);
    let start = ls_base.checked_add(usize_from(root_frame.locals_at).checked_mul(4)?)?;
    let end = ls_base
        .checked_add(usize_from(core.x.ls_top.checked_add(core.x.pushed)?).checked_mul(4)?)?;
    let mut data = core.mem.s.get(start..end)?.to_vec();
    let stack_len = data.len();

    let mut locals = Vec::new();
    let mut seen = Vec::new();
    for a in &chain {
        if seen.contains(&(a.prnum, a.func)) {
            continue;
        }
        seen.push((a.prnum, a.func));
        let ps = core.progs.get(usize::from(a.prnum))?;
        let f = ps.program.func(a.func)?;
        let at = usize_from(ps.gbase).checked_add(usize_from(f.parm_start).checked_mul(4)?)?;
        let len = usize_from(f.locals).checked_mul(4)?;
        data.extend_from_slice(core.mem.s.get(at..at.checked_add(len)?)?);
        locals.push((at, len));
    }

    let top = chain.last()?.prnum;
    let mut selves = [None, None];
    for (slot, name) in selves.iter_mut().zip(["self", "other"]) {
        if let Some(at) = global_offset(core, top, name) {
            let e = core.mem.g(at);
            let serial = core.mem.slots.get(usize_from(e)).map_or(0, |s| s.serial);
            *slot = Some((at, e, serial));
        }
    }
    Some(Thread { wake, chain, data, stack_len, locals, resume, selves })
}

impl<H: Host> Vm<H> {
    /// The value of the `time` global of the main progs (threads wake by it).
    fn qc_time(&self) -> f32 {
        global_offset(&self.core, 0, "time").map_or(0.0, |at| self.core.mem.gf(at))
    }

    /// Suspends a copy of the running QuakeC thread until `time` advances by `delay`; resuming
    /// makes the suspending call return `resume`. Returns `false` if no QuakeC is running.
    pub(crate) fn suspend(&mut self, delay: f32, resume: [u32; 3]) -> Result<bool, VmError> {
        if self.core.threads.len() >= usize_from(self.core.config.limits.threads) {
            return Err(ErrorKind::OutOfMemory(Resource::Threads).into());
        }
        let wake = self.qc_time() + delay;
        match snapshot(&self.core, wake, resume) {
            Some(thread) => {
                self.core.threads.push(thread);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Number of sleeping QuakeC threads.
    #[must_use]
    pub fn sleeping_threads(&self) -> usize {
        self.core.threads.len()
    }

    /// Resumes every sleeping thread whose wake time has come (by the `time` global), in wake
    /// order, and returns how many ran. Call it once per frame, when no QuakeC is running.
    ///
    /// # Errors
    /// Fails if called while QuakeC is running, or with the first error a resumed thread raised
    /// (threads not yet resumed stay queued).
    pub fn run_threads(&mut self, host: &mut H) -> Result<usize, VmError> {
        if self.core.nesting > 0 {
            return Err(VmError::host("run_threads called while QuakeC is running"));
        }
        let now = self.qc_time();
        let mut due = Vec::new();
        let mut i = 0;
        while let Some(t) = self.core.threads.get(i) {
            if t.wake <= now {
                due.push(self.core.threads.remove(i));
            } else {
                i = i.saturating_add(1);
            }
        }
        due.sort_by(|a, b| a.wake.total_cmp(&b.wake));
        let mut ran = 0usize;
        let mut queue = due.into_iter();
        while let Some(thread) = queue.next() {
            self.core.nesting = self.core.nesting.saturating_add(1);
            let result = self.resume(host, &thread);
            self.core.nesting = self.core.nesting.saturating_sub(1);
            self.flush_warnings(host);
            if let Err(e) = result {
                self.core.threads.extend(queue);
                return Err(e);
            }
            ran = ran.saturating_add(1);
        }
        if self.core.strings.wants_collection() {
            self.collect_garbage_now();
        }
        Ok(ran)
    }

    fn resume(&mut self, host: &mut H, thread: &Thread) -> Result<(), VmError> {
        let depth = self.core.frames.len();
        let mut resume_pc = self.core.x.pc;
        for a in &thread.chain {
            if let Err(kind) = self.core.enter(a.prnum, a.func, resume_pc) {
                self.core.unwind(depth);
                return Err(kind.into());
            }
            self.core.x.pushed = a.pushed;
            self.core.x.switch_ref = a.switch_ref;
            self.core.x.switch_kind = a.switch_kind;
            resume_pc = a.pc;
        }
        self.core.x.pc = resume_pc;

        // The rebuilt frames have the captured layout; restore the captured words.
        let ls_base = usize_from(self.core.mem.ls_base);
        let root_locals = self.core.frames.get(depth).map_or(0, |f| f.locals_at);
        let start = ls_base.saturating_add(usize_from(root_locals).saturating_mul(4));
        let (stack, locals) = thread.data.split_at(thread.stack_len.min(thread.data.len()));
        if let Some(dst) = self.core.mem.s.get_mut(start..start.saturating_add(stack.len())) {
            dst.copy_from_slice(stack);
        }
        let mut rest = locals;
        for &(at, len) in &thread.locals {
            let (chunk, tail) = rest.split_at(len.min(rest.len()));
            if let Some(dst) = self.core.mem.s.get_mut(at..at.saturating_add(chunk.len())) {
                dst.copy_from_slice(chunk);
            }
            rest = tail;
        }
        let gbase = self.core.gbase();
        for (k, w) in thread.resume.into_iter().enumerate() {
            self.core.mem.set_g(gbase.wrapping_add(OFS_RETURN).wrapping_add(k.wrapping_mul(4)), w);
        }
        for (at, e, serial) in thread.selves.iter().flatten().copied() {
            let alive = self
                .core
                .mem
                .slots
                .get(usize_from(e))
                .is_some_and(|s| s.in_use && s.serial == serial);
            self.core.mem.set_g(at, if alive { e } else { 0 });
        }
        let result = self.execute(host, depth);
        // A resumed thread has no caller to return to: drop the result `abort` (or `sleep`
        // suspending it again) left for one, so the next host call reads its own.
        self.core.abort_ret = None;
        result
    }
}
