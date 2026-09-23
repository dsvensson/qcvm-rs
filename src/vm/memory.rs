// SPDX-License-Identifier: MIT OR Apache-2.0

//! VM memory: one byte-addressed space split into three regions.
//!
//! ```text
//! 0          progs strings, globals (+3-word zero tail) and the local stack   region S
//! e_base     entity e at e_base + (e << stride_shift)                          region E
//! h_base     the QuakeC heap                                                   region H
//! ```
//!
//! Pointers and non-tagged string references are byte addresses in this space. Region S starts
//! with the main progs' string table so that string offsets double as pointers, exactly as in
//! FTE. Entity blocks have a fixed power-of-two stride so that fields added later never move
//! data or invalidate pointers.

use crate::bytes::{put_u32, u32_at, usize_from};
use crate::error::{ErrorKind, Resource, WarningKind};
use crate::vm::heap::Heap;

/// Where an access lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Loc {
    /// Offset into region S.
    S(usize),
    /// Offset into region E, and the entity number.
    E(usize, u32),
    /// Offset into region H.
    H(usize),
}

/// State of an entity slot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EntSlot {
    pub(crate) in_use: bool,
    pub(crate) protected: bool,
    /// Host clock time when the slot was freed.
    pub(crate) freetime: f64,
    /// Incremented every time the slot is (re)used by a spawn.
    pub(crate) serial: u32,
}

/// All QuakeC-addressable memory plus entity bookkeeping.
#[derive(Clone, Debug)]
pub(crate) struct Memory {
    /// Region S.
    pub(crate) s: Vec<u8>,
    /// Region E: `num_edicts << stride_shift` bytes.
    pub(crate) e: Vec<u8>,
    pub(crate) e_base: u32,
    pub(crate) stride_shift: u32,
    /// Bytes of each entity block that hold fields (the rest is reserved for fields added later).
    pub(crate) field_bytes: u32,
    pub(crate) max_edicts: u32,
    pub(crate) slots: Vec<EntSlot>,
    /// Region H.
    pub(crate) heap: Heap,
    pub(crate) h_base: u32,
    /// Local stack: byte offset in S and size in words.
    pub(crate) ls_base: u32,
    pub(crate) ls_words: u32,
}

impl Memory {
    /// Number of allocated entity slots (the high-water mark).
    #[inline]
    pub(crate) fn num_edicts(&self) -> u32 {
        u32::try_from(self.slots.len()).unwrap_or(u32::MAX)
    }

    /// Byte offset of entity `e`'s field `word` in region E, if both are valid.
    #[inline]
    pub(crate) fn field_offset(&self, e: u32, word: u32, words: u32) -> Option<usize> {
        if e >= self.num_edicts() {
            return None;
        }
        let end = u64::from(word).checked_add(u64::from(words))?.checked_mul(4)?;
        if end > u64::from(self.field_bytes) {
            return None;
        }
        let base = usize_from(e).checked_shl(self.stride_shift)?;
        base.checked_add(usize_from(word).checked_mul(4)?)
    }

    /// The pointer value of entity `e`'s field `word` (for `ADDRESS`), not validated.
    #[inline]
    pub(crate) fn field_address(&self, e: u32, word: u32) -> u32 {
        self.e_base
            .wrapping_add(e.wrapping_shl(self.stride_shift))
            .wrapping_add(word.wrapping_mul(4))
    }

    /// Locates an `n`-byte access at address `p`.
    #[inline]
    pub(crate) fn locate(&self, p: u32, n: u32) -> Option<Loc> {
        let end = u64::from(p).checked_add(u64::from(n))?;
        if end <= self.s.len() as u64 {
            return Some(Loc::S(usize_from(p)));
        }
        if let Some(off) = p.checked_sub(self.e_base) {
            let e = off >> self.stride_shift;
            let within = off & (1u32 << self.stride_shift).wrapping_sub(1);
            if e < self.num_edicts()
                && u64::from(within).checked_add(u64::from(n))? <= u64::from(self.field_bytes)
            {
                return Some(Loc::E(usize_from(off), e));
            }
        }
        if let Some(off) = p.checked_sub(self.h_base)
            && u64::from(off).checked_add(u64::from(n))? <= self.heap.data.len() as u64
        {
            return Some(Loc::H(usize_from(off)));
        }
        None
    }

    fn region(&self, loc: Loc) -> (&[u8], usize) {
        match loc {
            Loc::S(o) => (&self.s, o),
            Loc::E(o, _) => (&self.e, o),
            Loc::H(o) => (&self.heap.data, o),
        }
    }

    fn region_mut(&mut self, loc: Loc) -> (&mut [u8], usize) {
        match loc {
            Loc::S(o) => (&mut self.s, o),
            Loc::E(o, _) => (&mut self.e, o),
            Loc::H(o) => (&mut self.heap.data, o),
        }
    }

    /// Reads `N` bytes at `p`.
    #[inline]
    pub(crate) fn read<const N: usize>(&self, p: u32) -> Option<[u8; N]> {
        let loc = self.locate(p, u32::try_from(N).ok()?)?;
        let (data, o) = self.region(loc);
        data.get(o..)?.first_chunk::<N>().copied()
    }

    /// Reads a word at `p`.
    #[inline]
    pub(crate) fn read_u32(&self, p: u32) -> Option<u32> {
        self.read::<4>(p).map(u32::from_le_bytes)
    }

    /// Checks a write of `n` bytes at `p`.
    #[inline]
    pub(crate) fn check_write(&self, p: u32, n: u32) -> Result<Loc, WriteError> {
        if p == 0 {
            return Err(WriteError::Null);
        }
        let loc = self.locate(p, n).ok_or(WriteError::Invalid(p))?;
        if let Loc::E(_, e) = loc
            && self.slots.get(usize_from(e)).is_some_and(|s| s.protected)
        {
            return Err(WriteError::Protected(e));
        }
        Ok(loc)
    }

    /// Writes bytes at `p`.
    #[inline]
    pub(crate) fn write(&mut self, p: u32, bytes: &[u8]) -> Result<(), WriteError> {
        let n = u32::try_from(bytes.len()).map_err(|_| WriteError::Invalid(p))?;
        let loc = self.check_write(p, n)?;
        let (data, o) = self.region_mut(loc);
        let end = o.checked_add(bytes.len()).ok_or(WriteError::Invalid(p))?;
        data.get_mut(o..end).ok_or(WriteError::Invalid(p))?.copy_from_slice(bytes);
        Ok(())
    }

    /// Writes a word at `p`.
    #[inline]
    pub(crate) fn write_u32(&mut self, p: u32, v: u32) -> Result<(), WriteError> {
        self.write(p, &v.to_le_bytes())
    }

    /// The NUL-terminated bytes at linear address `p` (up to the end of the region if
    /// unterminated), or `None` if `p` is not readable.
    pub(crate) fn cstr(&self, p: u32) -> Option<&[u8]> {
        let loc = self.locate(p, 1)?;
        let tail = match loc {
            Loc::S(o) => self.s.get(o..)?,
            Loc::E(o, e) => {
                // A string inside an entity ends with that entity's fields.
                let start = usize_from(e).checked_shl(self.stride_shift)?;
                let end = start.checked_add(usize_from(self.field_bytes))?;
                self.e.get(o..end)?
            }
            Loc::H(o) => self.heap.data.get(o..)?,
        };
        Some(crate::vm::strings::until_nul(tail))
    }

    /// Reads a word from region S (globals and the local stack) by byte offset.
    #[inline(always)]
    pub(crate) fn g(&self, off: usize) -> u32 {
        u32_at(&self.s, off).unwrap_or(0)
    }

    /// Writes a word to region S by byte offset. Out-of-range writes are dropped (the loader
    /// guarantees every global operand is in range).
    #[inline(always)]
    pub(crate) fn set_g(&mut self, off: usize, v: u32) {
        put_u32(&mut self.s, off, v);
    }

    /// Reads a float from region S.
    #[inline(always)]
    pub(crate) fn gf(&self, off: usize) -> f32 {
        f32::from_bits(self.g(off))
    }

    /// Writes a float to region S.
    #[inline(always)]
    pub(crate) fn set_gf(&mut self, off: usize, v: f32) {
        self.set_g(off, v.to_bits());
    }

    /// Writes a vector to region S, component by component.
    #[inline(always)]
    pub(crate) fn set_gv(&mut self, off: usize, v: [f32; 3]) {
        self.set_gf(off, v[0]);
        self.set_gf(off.wrapping_add(4), v[1]);
        self.set_gf(off.wrapping_add(8), v[2]);
    }

    /// Copies `words` words within region S, in increasing order (overlap-exact like FTE).
    #[inline]
    pub(crate) fn copy_g(&mut self, src: usize, dst: usize, words: usize) {
        for i in 0..words {
            let o = i.wrapping_mul(4);
            let v = self.g(src.wrapping_add(o));
            self.set_g(dst.wrapping_add(o), v);
        }
    }

    /// Moves a block of bytes within region S (memmove). Returns `false` if out of range.
    pub(crate) fn move_s(&mut self, src: usize, dst: usize, len: usize) -> bool {
        let (Some(src_end), Some(dst_end)) = (src.checked_add(len), dst.checked_add(len)) else {
            return false;
        };
        if src_end > self.s.len() || dst_end > self.s.len() {
            return false;
        }
        self.s.copy_within(src..src_end, dst);
        true
    }

    /// Reads entity field words.
    #[inline]
    pub(crate) fn ent_word(&self, off: usize) -> u32 {
        u32_at(&self.e, off).unwrap_or(0)
    }

    #[inline]
    pub(crate) fn set_ent_word(&mut self, off: usize, v: u32) {
        put_u32(&mut self.e, off, v);
    }

    // ---- entities -------------------------------------------------------------------------

    /// Whether entity `e` is allocated and in use.
    pub(crate) fn in_use(&self, e: u32) -> bool {
        self.slots.get(usize_from(e)).is_some_and(|s| s.in_use)
    }

    /// Whether entity `e` is protected against QuakeC writes.
    pub(crate) fn protected(&self, e: u32) -> bool {
        self.slots.get(usize_from(e)).is_some_and(|s| s.protected)
    }

    /// Adds entity slots up to and including `e`, zeroed.
    fn grow_to(&mut self, e: u32) -> Result<(), ErrorKind> {
        let slots = usize_from(e).checked_add(1).ok_or(ErrorKind::NoFreeEdicts)?;
        let bytes = slots.checked_shl(self.stride_shift).ok_or(ErrorKind::NoFreeEdicts)?;
        if bytes > self.e.len() {
            self.e
                .try_reserve(bytes.saturating_sub(self.e.len()))
                .map_err(|_| ErrorKind::OutOfMemory(Resource::Entities))?;
            self.e.resize(bytes, 0);
        }
        while self.slots.len() < slots {
            self.slots.push(EntSlot { in_use: false, protected: false, freetime: 0.0, serial: 0 });
        }
        // The interpreter's field fast paths rely on this: the slice bound checks the entity.
        debug_assert_eq!(self.e.len(), self.slots.len().wrapping_shl(self.stride_shift));
        Ok(())
    }

    /// Zeroes all of entity `e`'s fields.
    pub(crate) fn clear_entity(&mut self, e: u32) {
        let start = usize_from(e).checked_shl(self.stride_shift).unwrap_or(usize::MAX);
        let end = start.saturating_add(1usize << self.stride_shift);
        if let Some(block) = self.e.get_mut(start..end) {
            block.fill(0);
        }
    }

    /// Creates the world entity (entity 0).
    pub(crate) fn init_world(&mut self) -> Result<(), ErrorKind> {
        self.grow_to(0)?;
        self.clear_entity(0);
        if let Some(slot) = self.slots.get_mut(0) {
            *slot = EntSlot { in_use: true, protected: false, freetime: 0.0, serial: 1 };
        }
        Ok(())
    }

    /// Allocates an entity like FTE: the first free slot freed more than half a second ago (or
    /// during the first two seconds), else a new slot, else any free slot. Fields are zeroed.
    pub(crate) fn spawn(&mut self, now: f64, first: u32) -> Result<u32, ErrorKind> {
        let reusable = |s: &EntSlot| !s.in_use && (s.freetime < 2.0 || now - s.freetime > 0.5);
        let start = usize_from(first).min(self.slots.len());
        let pick =
            self.slots.iter().enumerate().skip(start).find(|(_, s)| reusable(s)).map(|(i, _)| i);
        // Growing starts at the first spawnable slot, so reserved slots are never handed out.
        let grow = self.num_edicts().max(first);
        let e = match pick {
            Some(i) => u32::try_from(i).map_err(|_| ErrorKind::NoFreeEdicts)?,
            None if grow < self.max_edicts => {
                self.grow_to(grow)?;
                grow
            }
            None => self
                .slots
                .iter()
                .enumerate()
                .skip(start)
                .find(|(_, s)| !s.in_use)
                .and_then(|(i, _)| u32::try_from(i).ok())
                .ok_or(ErrorKind::NoFreeEdicts)?,
        };
        self.clear_entity(e);
        if let Some(slot) = self.slots.get_mut(usize_from(e)) {
            slot.in_use = true;
            slot.protected = false;
            slot.serial = slot.serial.wrapping_add(1);
        }
        Ok(e)
    }

    /// Frees entity `e`, zeroing the given field words. `instant` makes the slot reusable at
    /// once.
    pub(crate) fn remove(
        &mut self,
        e: u32,
        now: f64,
        instant: bool,
        clear: &[u32],
    ) -> Result<(), WarningKind> {
        if e == 0 {
            return Err(WarningKind::Builtin("Unable to remove the world".into()));
        }
        let Some(slot) = self.slots.get(usize_from(e)).copied() else {
            return Err(WarningKind::BadEntity(e));
        };
        if !slot.in_use {
            return Err(WarningKind::Builtin(format!("entity {e} is already free")));
        }
        if slot.protected {
            return Err(WarningKind::ReadOnlyEntity(e));
        }
        for &word in clear {
            if let Some(off) = self.field_offset(e, word, 1) {
                self.set_ent_word(off, 0);
            }
        }
        if let Some(slot) = self.slots.get_mut(usize_from(e)) {
            slot.in_use = false;
            slot.freetime = if instant { 0.0 } else { now };
        }
        Ok(())
    }
}

/// Why a pointer write failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriteError {
    /// Address 0.
    Null,
    /// Not writable memory.
    Invalid(u32),
    /// A protected entity (the write is skipped with a warning).
    Protected(u32),
}
