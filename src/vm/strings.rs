// SPDX-License-Identifier: MIT OR Apache-2.0

//! Dynamic strings: garbage-collected temp strings and host-interned strings.
//!
//! String references are 32-bit values classified by their top two bits: `00`/`01` address VM
//! memory (the progs string tables live at the start of it), `10` is a temp string
//! (`0x8000_0000 | slot`) and `11` a host-interned string (`0xC000_0000 | index`).
//!
//! Temp strings live outside VM memory in a slot table. They are collected by a conservative
//! mark-and-sweep that treats every aligned word of VM memory whose top bits are `10` as a
//! reference, run only when no QuakeC is executing. So a temp string lives exactly as long as
//! something in QuakeC-visible memory refers to it.

use std::collections::HashMap;

use crate::bytes::usize_from;
use crate::error::Resource;

/// Tag of temp-string references.
pub(crate) const TEMP_TAG: u32 = 0x8000_0000;
/// Tag of host-interned string references.
pub(crate) const STATIC_TAG: u32 = 0xC000_0000;
/// Mask for the slot/index part of a tagged reference.
pub(crate) const INDEX_MASK: u32 = 0x3FFF_FFFF;
/// Mask selecting the tag bits.
pub(crate) const TAG_MASK: u32 = 0xC000_0000;

/// Temp strings grow when QuakeC writes past their end, up to this size.
pub(crate) const MAX_TEMP_GROWTH: usize = 1 << 20;

/// Initial temp-slot table size.
const INITIAL_SLOTS: usize = 1024;

/// What a string reference designates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StrKind {
    /// A byte address in VM memory.
    Linear(u32),
    /// A temp-string slot.
    Temp(u32),
    /// A host-interned string.
    Static(u32),
}

pub(crate) const fn classify(r: u32) -> StrKind {
    match r & TAG_MASK {
        TEMP_TAG => StrKind::Temp(r & INDEX_MASK),
        STATIC_TAG => StrKind::Static(r & INDEX_MASK),
        _ => StrKind::Linear(r),
    }
}

/// Garbage-collection statistics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GcStats {
    /// Temp strings alive after the collection.
    pub live: usize,
    /// Temp strings freed.
    pub freed: usize,
}

/// The temp-string slot table plus host-interned strings.
#[derive(Clone, Debug)]
pub(crate) struct Strings {
    /// Slot contents: the string bytes followed by a NUL, padded to a multiple of 4.
    slots: Vec<Option<Vec<u8>>>,
    /// Soft capacity; collection triggers when half of it is live.
    capacity: usize,
    cursor: usize,
    live: usize,
    bytes: usize,
    max_slots: usize,
    max_bytes: usize,
    statics: Vec<Box<[u8]>>,
    static_index: HashMap<Box<[u8]>, u32>,
    pins: HashMap<u32, u32>,
}

impl Strings {
    pub(crate) fn new(max_slots: usize, max_bytes: usize) -> Self {
        Self {
            slots: Vec::new(),
            capacity: INITIAL_SLOTS.min(max_slots.max(1)),
            cursor: 0,
            live: 0,
            bytes: 0,
            max_slots: max_slots.min(usize_from(INDEX_MASK)),
            max_bytes,
            statics: Vec::new(),
            static_index: HashMap::new(),
            pins: HashMap::new(),
        }
    }

    /// Number of live temp strings.
    pub(crate) fn live(&self) -> usize {
        self.live
    }

    /// Stores `text` (without terminator) as a new temp string.
    pub(crate) fn alloc(&mut self, text: &[u8]) -> Result<u32, Resource> {
        let mut data = Vec::new();
        let padded = text.len().checked_add(4).map(|n| n & !3).ok_or(Resource::TempStrings)?;
        data.try_reserve_exact(padded).map_err(|_| Resource::TempStrings)?;
        data.extend_from_slice(text);
        data.resize(padded, 0);
        self.alloc_raw(data)
    }

    /// Stores already padded, NUL-terminated data as a new temp string.
    pub(crate) fn alloc_raw(&mut self, data: Vec<u8>) -> Result<u32, Resource> {
        let bytes = self.bytes.checked_add(data.len()).ok_or(Resource::TempStrings)?;
        if bytes > self.max_bytes {
            return Err(Resource::TempStrings);
        }
        let slot = self.free_slot()?;
        if let Some(entry) = self.slots.get_mut(slot) {
            *entry = Some(data);
        }
        self.live = self.live.saturating_add(1);
        self.bytes = bytes;
        self.cursor = slot.saturating_add(1);
        u32::try_from(slot).map(|s| s | TEMP_TAG).map_err(|_| Resource::TempStrings)
    }

    fn free_slot(&mut self) -> Result<usize, Resource> {
        if self.live >= self.capacity {
            // Full: grow like FTE (2 × max + 1024), within the hard limit.
            let grown = self.capacity.saturating_mul(2).saturating_add(INITIAL_SLOTS);
            let grown = grown.min(self.max_slots);
            if grown <= self.capacity {
                return Err(Resource::TempStrings);
            }
            self.capacity = grown;
        }
        if self.slots.len() < self.capacity {
            let want = self.capacity.saturating_sub(self.slots.len());
            self.slots.try_reserve(want).map_err(|_| Resource::TempStrings)?;
            self.slots.resize(self.capacity, None);
        }
        let n = self.capacity;
        let start = self.cursor.checked_rem(n).unwrap_or(0);
        (start..n)
            .chain(0..start)
            .find(|&i| self.slots.get(i).is_some_and(Option::is_none))
            .ok_or(Resource::TempStrings)
    }

    /// The raw slot data (with terminator and padding).
    pub(crate) fn temp_raw(&self, slot: u32) -> Option<&[u8]> {
        self.slots.get(usize_from(slot))?.as_deref()
    }

    /// Mutable slot data, grown with zeros to at least `len` bytes (up to [`MAX_TEMP_GROWTH`]).
    pub(crate) fn temp_raw_mut(&mut self, slot: u32, len: usize) -> Option<&mut [u8]> {
        let data = self.slots.get_mut(usize_from(slot))?.as_mut()?;
        if data.len() < len {
            if len > MAX_TEMP_GROWTH {
                return None;
            }
            let grow = len.checked_add(3)? & !3;
            let extra = grow.saturating_sub(data.len());
            let bytes = self.bytes.checked_add(extra)?;
            if bytes > self.max_bytes {
                return None;
            }
            data.try_reserve(extra).ok()?;
            data.resize(grow, 0);
            self.bytes = bytes;
        }
        Some(data)
    }

    /// Interns a permanent string and returns its reference.
    pub(crate) fn intern(&mut self, text: &[u8]) -> u32 {
        if let Some(&index) = self.static_index.get(text) {
            return index | STATIC_TAG;
        }
        let index = u32::try_from(self.statics.len()).unwrap_or(INDEX_MASK) & INDEX_MASK;
        self.statics.push(text.into());
        self.static_index.insert(text.into(), index);
        index | STATIC_TAG
    }

    pub(crate) fn static_str(&self, index: u32) -> Option<&[u8]> {
        self.statics.get(usize_from(index)).map(AsRef::as_ref)
    }

    /// Keeps a temp string alive across collections until unpinned.
    pub(crate) fn pin(&mut self, r: u32) {
        if let StrKind::Temp(slot) = classify(r) {
            let count = self.pins.entry(slot).or_insert(0);
            *count = count.saturating_add(1);
        }
    }

    pub(crate) fn unpin(&mut self, r: u32) {
        if let StrKind::Temp(slot) = classify(r)
            && let Some(count) = self.pins.get_mut(&slot)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.pins.remove(&slot);
            }
        }
    }

    /// Whether a collection is due (FTE's rule: half the table live and the cursor past half).
    pub(crate) fn wants_collection(&self) -> bool {
        let half = self.capacity / 2;
        self.live >= half && self.cursor >= half
    }

    /// Collects every temp string not referenced from `roots` (aligned words with top bits `10`)
    /// or pinned.
    pub(crate) fn collect<'a>(&mut self, roots: impl IntoIterator<Item = &'a [u8]>) -> GcStats {
        let mut marks = vec![false; self.slots.len()];
        for region in roots {
            for word in region.chunks_exact(4) {
                let w = u32::from_le_bytes([
                    word.first().copied().unwrap_or(0),
                    word.get(1).copied().unwrap_or(0),
                    word.get(2).copied().unwrap_or(0),
                    word.get(3).copied().unwrap_or(0),
                ]);
                if w & TAG_MASK == TEMP_TAG
                    && let Some(m) = marks.get_mut(usize_from(w & INDEX_MASK))
                {
                    *m = true;
                }
            }
        }
        for &slot in self.pins.keys() {
            if let Some(m) = marks.get_mut(usize_from(slot)) {
                *m = true;
            }
        }
        let mut freed = 0usize;
        for (slot, marked) in self.slots.iter_mut().zip(&marks) {
            if !marked && let Some(data) = slot.take() {
                self.bytes = self.bytes.saturating_sub(data.len());
                freed = freed.saturating_add(1);
            }
        }
        self.live = self.live.saturating_sub(freed);
        self.cursor = 0;
        if self.live >= self.capacity / 2 {
            self.capacity = self.capacity.saturating_mul(2).min(self.max_slots.max(1));
        }
        GcStats { live: self.live, freed }
    }
}

/// The text of NUL-terminated data (up to the first NUL).
pub(crate) fn until_nul(data: &[u8]) -> &[u8] {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    data.get(..end).unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn alloc_resolve_and_collect() {
        let mut s = Strings::new(1 << 20, 1 << 24);
        let a = s.alloc(b"hello").unwrap();
        let b = s.alloc(b"").unwrap();
        assert_eq!(classify(a), StrKind::Temp(0));
        assert_ne!(b, 0, "empty temp strings are not null");
        assert_eq!(until_nul(s.temp_raw(a & INDEX_MASK).unwrap()), b"hello");
        assert_eq!(s.temp_raw(a & INDEX_MASK).unwrap().len(), 8);
        let root = a.to_le_bytes();
        let stats = s.collect([&root[..]]);
        assert_eq!(stats, GcStats { live: 1, freed: 1 });
        assert!(s.temp_raw(b & INDEX_MASK).is_none());
        assert!(s.temp_raw(a & INDEX_MASK).is_some());
    }

    #[test]
    fn pins_survive_collection() {
        let mut s = Strings::new(1 << 20, 1 << 24);
        let a = s.alloc(b"x").unwrap();
        s.pin(a);
        assert_eq!(s.collect([]).freed, 0);
        s.unpin(a);
        assert_eq!(s.collect([]).freed, 1);
    }

    #[test]
    fn limits() {
        let mut s = Strings::new(2, 1 << 20);
        assert!(s.alloc(b"a").is_ok());
        assert!(s.alloc(b"b").is_ok());
        assert_eq!(s.alloc(b"c"), Err(Resource::TempStrings));
        let mut s = Strings::new(100, 16);
        assert!(s.alloc(b"0123456789").is_ok());
        assert_eq!(s.alloc(b"0123456789"), Err(Resource::TempStrings));
    }

    #[test]
    fn interning() {
        let mut s = Strings::new(8, 64);
        let a = s.intern(b"model");
        assert_eq!(s.intern(b"model"), a);
        assert_eq!(classify(a), StrKind::Static(0));
        assert_eq!(s.static_str(0), Some(&b"model"[..]));
    }

    #[test]
    fn temps_grow_when_written() {
        let mut s = Strings::new(8, 1 << 21);
        let a = s.alloc(b"ab").unwrap() & INDEX_MASK;
        let data = s.temp_raw_mut(a, 10).unwrap();
        assert_eq!(data.len(), 12);
        assert!(s.temp_raw_mut(a, MAX_TEMP_GROWTH + 1).is_none());
    }
}
