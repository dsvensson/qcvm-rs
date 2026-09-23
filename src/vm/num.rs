// SPDX-License-Identifier: MIT OR Apache-2.0

//! Numeric conversions with FTE-on-x86 semantics, identical on every platform.
//!
//! C leaves out-of-range float→integer conversions undefined; FTE on x86 produces the "integer
//! indefinite" value (`INT_MIN`). Rust's `as` saturates instead, so every conversion the VM
//! performs goes through these helpers to stay deterministic and FTE-compatible.

/// `(int)f`: truncation toward zero; NaN and out-of-range values give `i32::MIN`.
#[inline]
pub(crate) fn f2i(f: f32) -> i32 {
    // 2^31 as f32 is exact; anything at or above it (or below -2^31) is out of range.
    if f.is_nan() || !(-2_147_483_648.0..2_147_483_648.0).contains(&f) {
        i32::MIN
    } else {
        f as i32
    }
}

/// `(long long)d`: truncation toward zero; NaN and out-of-range values give `i64::MIN`.
#[inline]
pub(crate) fn d2i64(d: f64) -> i64 {
    if d.is_nan() || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&d) {
        i64::MIN
    } else {
        d as i64
    }
}

/// `(unsigned)f` as x86-64 compilers emit it: a 64-bit signed conversion, then the low 32 bits.
#[inline]
pub(crate) fn f2u(f: f32) -> u32 {
    d2i64(f64::from(f)) as u32
}

/// `(unsigned long long)d` as x86-64 compilers emit it.
#[inline]
pub(crate) fn d2u64(d: f64) -> u64 {
    const TWO63: f64 = 9_223_372_036_854_775_808.0;
    if d.is_nan() {
        1 << 63
    } else if d < TWO63 {
        d2i64(d).cast_unsigned()
    } else {
        d2i64(d - TWO63).cast_unsigned() ^ (1 << 63)
    }
}

/// FTE's float truth for `NOT_F`, `AND_F`, `OR_F`, `IF_F`: the value with its sign bit masked
/// off is non-zero (so `-0.0` is false and denormals are true).
#[inline(always)]
pub(crate) const fn float_true(bits: u32) -> bool {
    bits & 0x7FFF_FFFF != 0
}

/// Converts a C-style boolean to the float QuakeC comparisons produce.
#[inline(always)]
pub(crate) const fn fbool(b: bool) -> u32 {
    if b { 0x3F80_0000 } else { 0 }
}

/// Converts a boolean to the integer the typed comparisons produce.
#[inline(always)]
pub(crate) const fn ibool(b: bool) -> u32 {
    b as u32
}

/// Joins two words (low word first) into a 64-bit value.
#[inline(always)]
pub(crate) const fn join64(lo: u32, hi: u32) -> u64 {
    (lo as u64) | ((hi as u64) << 32)
}

/// Splits a 64-bit value into words (low word first).
#[inline(always)]
pub(crate) const fn split64(v: u64) -> (u32, u32) {
    (v as u32, (v >> 32) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_to_int_matches_x86() {
        assert_eq!(f2i(3.9), 3);
        assert_eq!(f2i(-3.9), -3);
        assert_eq!(f2i(f32::NAN), i32::MIN);
        assert_eq!(f2i(2_147_483_648.0), i32::MIN);
        assert_eq!(f2i(-2_147_483_648.0), i32::MIN);
        assert_eq!(f2i(3e9), i32::MIN);
        assert_eq!(f2i(f32::INFINITY), i32::MIN);
        assert_eq!(f2i(-2_147_483_500.0), -2_147_483_520);
    }

    #[test]
    fn unsigned_conversions() {
        assert_eq!(f2u(-1.0), u32::MAX);
        assert_eq!(f2u(4_294_967_040.0), 4_294_967_040);
        assert_eq!(f2u(f32::NAN), 0);
        assert_eq!(d2u64(-1.0), u64::MAX);
        assert_eq!(d2u64(1e19), 10_000_000_000_000_000_000);
        assert_eq!(d2u64(1e20), 0);
        assert_eq!(d2u64(f64::NAN), 1 << 63);
    }

    #[test]
    fn truth() {
        assert!(!float_true((-0.0f32).to_bits()));
        assert!(float_true(1));
        assert!(float_true(1.0f32.to_bits()));
        assert_eq!(fbool(true), 1.0f32.to_bits());
    }
}
