// SPDX-License-Identifier: MIT OR Apache-2.0

//! Panic-free little-endian access to byte slices.

/// Reads a little-endian `u16` at byte offset `off`.
#[inline(always)]
pub(crate) fn u16_at(data: &[u8], off: usize) -> Option<u16> {
    data.get(off..)?.first_chunk::<2>().copied().map(u16::from_le_bytes)
}

/// Reads a little-endian `u32` at byte offset `off`.
#[inline(always)]
pub(crate) fn u32_at(data: &[u8], off: usize) -> Option<u32> {
    let chunk = data.get(off..off.wrapping_add(4))?;
    chunk.first_chunk::<4>().copied().map(u32::from_le_bytes)
}

/// Writes a little-endian `u32` at byte offset `off`. Returns `false` if out of range.
#[inline(always)]
pub(crate) fn put_u32(data: &mut [u8], off: usize, value: u32) -> bool {
    match data.get_mut(off..off.wrapping_add(4)).and_then(|s| s.first_chunk_mut::<4>()) {
        Some(chunk) => {
            *chunk = value.to_le_bytes();
            true
        }
        None => false,
    }
}

/// The NUL-terminated byte string starting at `off` (without the terminator). Stops at the end of
/// `data` if there is no terminator; out-of-range offsets give an empty string.
pub(crate) fn cstr_at(data: &[u8], off: usize) -> &[u8] {
    let tail = data.get(off..).unwrap_or_default();
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    tail.get(..end).unwrap_or_default()
}

/// Converts a `u32` to `usize` (lossless on every supported target).
#[inline(always)]
pub(crate) const fn usize_from(v: u32) -> usize {
    v as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_bounds_checked() {
        let d = [1u8, 2, 3, 4, 5];
        assert_eq!(u16_at(&d, 0), Some(0x0201));
        assert_eq!(u32_at(&d, 1), Some(0x0504_0302));
        assert_eq!(u32_at(&d, 2), None);
        assert_eq!(u32_at(&d, usize::MAX), None);
    }

    #[test]
    fn cstr() {
        let d = b"\0ab\0cd";
        assert_eq!(cstr_at(d, 0), b"");
        assert_eq!(cstr_at(d, 1), b"ab");
        assert_eq!(cstr_at(d, 4), b"cd");
        assert_eq!(cstr_at(d, 99), b"");
    }

    #[test]
    fn put() {
        let mut d = [0u8; 6];
        assert!(put_u32(&mut d, 2, 0x0403_0201));
        assert_eq!(d, [0, 0, 1, 2, 3, 4]);
        assert!(!put_u32(&mut d, 3, 1));
    }
}
