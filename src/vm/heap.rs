// SPDX-License-Identifier: MIT OR Apache-2.0

//! The QuakeC heap (`memalloc` and friends): a first-fit allocator over one byte region.
//!
//! Block bookkeeping lives outside the region, so QuakeC writes cannot corrupt it.

use std::collections::BTreeMap;

use crate::bytes::usize_from;

/// Allocation granularity (and alignment) in bytes.
const ALIGN: u32 = 16;

fn round_up(n: u32) -> Option<u32> {
    n.max(1).checked_add(ALIGN - 1).map(|v| v & !(ALIGN - 1))
}

/// A byte region with first-fit block allocation.
#[derive(Clone, Debug, Default)]
pub(crate) struct Heap {
    pub(crate) data: Vec<u8>,
    max: u32,
    /// Allocated blocks: offset → (rounded size, requested size).
    used: BTreeMap<u32, (u32, u32)>,
    /// Free blocks inside `data`, coalesced: offset → size.
    free: BTreeMap<u32, u32>,
}

impl Heap {
    pub(crate) fn new(max: u32) -> Self {
        Self { max: max & !(ALIGN - 1), ..Self::default() }
    }

    /// Allocates `n` zeroed bytes and returns the block offset.
    pub(crate) fn alloc(&mut self, n: u32) -> Option<u32> {
        let size = round_up(n)?;
        let found = self.free.iter().find(|&(_, &len)| len >= size).map(|(&o, &l)| (o, l));
        let offset = if let Some((offset, len)) = found {
            self.free.remove(&offset);
            if len > size {
                self.free.insert(offset.checked_add(size)?, len.wrapping_sub(size));
            }
            offset
        } else {
            let offset = u32::try_from(self.data.len()).ok()?;
            let end = offset.checked_add(size)?;
            if end > self.max {
                return None;
            }
            self.data.try_reserve(usize_from(size)).ok()?;
            self.data.resize(usize_from(end), 0);
            offset
        };
        let start = usize_from(offset);
        if let Some(block) = self.data.get_mut(start..start.checked_add(usize_from(size))?) {
            block.fill(0);
        }
        self.used.insert(offset, (size, n));
        Some(offset)
    }

    /// Frees the block at `offset`. Returns `false` if no block starts there.
    pub(crate) fn free(&mut self, offset: u32) -> bool {
        let Some((size, _)) = self.used.remove(&offset) else {
            return false;
        };
        let mut start = offset;
        let mut len = size;
        // Merge with the following free block.
        if let Some(end) = start.checked_add(len)
            && let Some(next) = self.free.remove(&end)
        {
            len = len.saturating_add(next);
        }
        // Merge with the preceding free block.
        if let Some((&prev, &prev_len)) = self.free.range(..start).next_back()
            && prev.checked_add(prev_len) == Some(start)
        {
            self.free.remove(&prev);
            start = prev;
            len = len.saturating_add(prev_len);
        }
        self.free.insert(start, len);
        true
    }

    /// The size originally requested for the block at `offset`.
    pub(crate) fn block_size(&self, offset: u32) -> Option<u32> {
        self.used.get(&offset).map(|&(_, n)| n)
    }

    /// Resizes a block, preserving its contents (a new zeroed block if `offset` is `None`).
    pub(crate) fn realloc(&mut self, offset: Option<u32>, n: u32) -> Option<u32> {
        let Some(old) = offset else { return self.alloc(n) };
        let old_size = self.block_size(old)?;
        let new = self.alloc(n)?;
        let keep = usize_from(old_size.min(n));
        let src = usize_from(old);
        if let Some(end) = src.checked_add(keep)
            && end <= self.data.len()
            && usize_from(new).checked_add(keep).is_some_and(|e| e <= self.data.len())
        {
            self.data.copy_within(src..end, usize_from(new));
        }
        self.free(old);
        Some(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_fit_reuses_and_coalesces() {
        let mut h = Heap::new(1 << 20);
        let a = h.alloc(10).unwrap_or(u32::MAX);
        let b = h.alloc(20).unwrap_or(u32::MAX);
        let c = h.alloc(30).unwrap_or(u32::MAX);
        assert_eq!((a, b, c), (0, 16, 48));
        assert!(h.free(b));
        assert!(!h.free(b));
        assert_eq!(h.alloc(5), Some(16));
        assert!(h.free(16));
        assert!(h.free(a));
        // a and b coalesced into one 48-byte block.
        assert_eq!(h.alloc(40), Some(0));
        assert_eq!(h.block_size(0), Some(40));
    }

    #[test]
    fn allocations_are_zeroed_and_bounded() {
        let mut h = Heap::new(64);
        let a = h.alloc(16).unwrap_or(u32::MAX);
        h.data.iter_mut().for_each(|b| *b = 0xAA);
        assert!(h.free(a));
        let b = h.alloc(16).unwrap_or(u32::MAX);
        assert_eq!(h.data.get(..16), Some(&[0u8; 16][..]));
        assert_eq!(b, a);
        assert!(h.alloc(64).is_none());
        assert!(h.alloc(48).is_some());
    }

    #[test]
    fn realloc_keeps_contents() {
        let mut h = Heap::new(1 << 16);
        let a = h.alloc(4).unwrap_or(u32::MAX);
        if let Some(bytes) = h.data.get_mut(..4) {
            bytes.copy_from_slice(&[1, 2, 3, 4]);
        }
        let b = h.realloc(Some(a), 100).unwrap_or(u32::MAX);
        let start = usize_from(b);
        assert_eq!(h.data.get(start..start + 4), Some(&[1u8, 2, 3, 4][..]));
        assert_eq!(h.block_size(a), None);
    }
}
