// SPDX-License-Identifier: MIT OR Apache-2.0

//! Number/string conversions: ftos, vtos, etos, itos, htos, stoi, stoh, ftoi, itof, ftou, utof,
//! stof, stov (docs/spec/strings.md).

use crate::builtins::Builtins;
use crate::error::VmError;
use crate::host::Host;
use crate::stdlib::format::{format_f, hex_to_decimal_string};
use crate::stdlib::util::args_concat;
use crate::vm::Vm;
use crate::vm::num::{f2i, f2u};

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("ftos", ftos::<H>);
    b.set("vtos", vtos::<H>);
    b.set("etos", etos::<H>);
    b.set("itos", itos::<H>);
    b.set("htos", htos::<H>);
    b.set("stoi", stoi::<H>);
    b.set("stoh", stoh::<H>);
    b.set("ftoi", ftoi::<H>);
    b.set("itof", itof::<H>);
    b.set("ftou", ftou::<H>);
    b.set("utof", utof::<H>);
    b.set("stof", stof::<H>);
    b.set("stov", stov::<H>);
}

// ---- number → text ----------------------------------------------------------------------------

/// FTE's `ftos` text for `v`: integers in decimal, other values with just enough decimals to show
/// the float's exact value to about 8 significant digits (`0.1` → `0.100000001`), infinities and
/// NaNs as `1.#INF`/`1.#NAN` with the sign bit shown.
pub(crate) fn ftos_text(v: f32) -> Vec<u8> {
    let int = f2i(v);
    if v == int as f32 {
        return int.to_string().into_bytes();
    }
    let bits = v.to_bits();
    let raw = (bits >> 23) & 0xFF;
    if raw == 0xFF {
        let mut s = if bits >> 31 != 0 { b"-".to_vec() } else { Vec::new() };
        s.extend_from_slice(if bits & 0x007F_FFFF == 0 { b"1.#INF" } else { b"1.#NAN" });
        return s;
    }
    // Decimals: 8 plus the decimal magnitude of the binary exponent (computed in single
    // precision with FTE's constant, 0.30102999957f).
    let e = (raw as i32).wrapping_sub(127);
    let log10_2 = f32::from_bits(0x3E9A_209B);
    let decimals = f2i(e.wrapping_neg() as f32 * log10_2).wrapping_add(8);
    let Ok(decimals) = usize::try_from(decimals) else {
        return format_f(f64::from(v), 0);
    };
    if decimals == 0 {
        return format_f(f64::from(v), 0);
    }
    let mut s = format_f(f64::from(v), decimals);
    // Cut after the last significant digit; zeros before the point are significant.
    let mut keep = 0usize;
    for (i, &c) in s.iter().enumerate() {
        if (b'1'..=b'9').contains(&c) {
            keep = i.saturating_add(1);
        } else if c == b'.' {
            keep = i;
        }
    }
    s.truncate(keep);
    s
}

/// `string ftos(float)`: see [`ftos_text`].
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn ftos<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let s = ftos_text(vm.arg_f32(0));
    vm.ret_str(&s)
}

/// `string vtos(vector)`: `'x y z'` with each component as C's `%f`.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn vtos<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let mut s = vec![b'\''];
    for (k, c) in vm.arg_vec(0).into_iter().enumerate() {
        if k > 0 {
            s.push(b' ');
        }
        s.extend_from_slice(&format_f(f64::from(c), 6));
    }
    s.push(b'\'');
    vm.ret_str(&s)
}

/// `string etos(entity)`: `"entity N"`.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn etos<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let s = format!("entity {}", vm.arg_i32(0));
    vm.ret_str(s.as_bytes())
}

/// `string itos(int)`: signed decimal.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn itos<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let s = vm.arg_i32(0).to_string();
    vm.ret_str(s.as_bytes())
}

/// `string htos(int)`: exactly eight lowercase hex digits.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn htos<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let s = format!("{:08x}", vm.arg_u32(0));
    vm.ret_str(s.as_bytes())
}

// ---- text → number ----------------------------------------------------------------------------

/// C's `isspace` in the C locale.
pub(crate) fn is_c_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

fn digit_value(c: u8) -> Option<u32> {
    char::from(c).to_digit(36)
}

/// The integer parse shared by C's `strtol` and `strtoul`: leading white space, a sign, an
/// optional `0x` prefix (base 16, or base 0 which also picks octal for a leading `0`), digits.
/// Returns `(negative, magnitude, overflowed)`.
fn parse_c_integer(s: &[u8], base: u32) -> (bool, u64, bool) {
    let mut i = s.iter().take_while(|&&c| is_c_space(c)).count();
    let negative = match s.get(i) {
        Some(b'-') => {
            i = i.saturating_add(1);
            true
        }
        Some(b'+') => {
            i = i.saturating_add(1);
            false
        }
        _ => false,
    };
    let has_hex_prefix = s.get(i) == Some(&b'0')
        && matches!(s.get(i.saturating_add(1)), Some(b'x' | b'X'))
        && s.get(i.saturating_add(2)).and_then(|&c| digit_value(c)).is_some_and(|d| d < 16);
    let base = match base {
        0 if has_hex_prefix => 16,
        0 if s.get(i) == Some(&b'0') => 8,
        0 => 10,
        b => b,
    };
    if base == 16 && has_hex_prefix {
        i = i.saturating_add(2);
    }
    let mut value = 0u64;
    let mut overflow = false;
    for &c in s.get(i..).unwrap_or_default() {
        let Some(d) = digit_value(c).filter(|&d| d < base) else { break };
        match value.checked_mul(u64::from(base)).and_then(|v| v.checked_add(u64::from(d))) {
            Some(v) => value = v,
            None => overflow = true,
        }
    }
    (negative, value, overflow)
}

/// C's `strtol(s, NULL, base)` with a 64-bit `long` (saturating on overflow).
pub(crate) fn strtol(s: &[u8], base: u32) -> i64 {
    let (negative, mag, overflow) = parse_c_integer(s, base);
    match (negative, overflow) {
        (false, true) => i64::MAX,
        (true, true) => i64::MIN,
        (false, false) => i64::try_from(mag).unwrap_or(i64::MAX),
        (true, false) => 0i64.checked_sub_unsigned(mag).unwrap_or(i64::MIN),
    }
}

/// C's `strtoul(s, NULL, base)` with a 64-bit `long` (`ULONG_MAX` on overflow; a minus sign
/// negates in unsigned arithmetic).
pub(crate) fn strtoul(s: &[u8], base: u32) -> u64 {
    let (negative, mag, overflow) = parse_c_integer(s, base);
    if overflow {
        u64::MAX
    } else if negative {
        mag.wrapping_neg()
    } else {
        mag
    }
}

fn starts_with_ci(s: &[u8], prefix: &[u8]) -> bool {
    s.get(..prefix.len()).is_some_and(|p| p.eq_ignore_ascii_case(prefix))
}

fn hex_value(c: u8) -> Option<u8> {
    digit_value(c).filter(|&d| d < 16).map(|d| d as u8)
}

/// The significant hex digits kept when parsing a hex float (160 bits: far more than a double
/// needs, so a sticky digit for the rest rounds correctly).
const HEX_DIGITS_KEPT: usize = 40;

/// Parses the part of a C99 hex float after `0x`: `(value, bytes consumed)`, or `None` without
/// any hex digit.
fn parse_hex_float(s: &[u8]) -> Option<(f64, usize)> {
    let mut i = 0usize;
    let mut digits = Vec::new();
    let mut sticky = false;
    let mut scale: i64 = 0; // binary exponent contributed by digit positions
    let mut any = false;
    let mut seen_point = false;
    loop {
        match s.get(i).copied() {
            Some(b'.') if !seen_point => seen_point = true,
            Some(c) => {
                let Some(h) = hex_value(c) else { break };
                any = true;
                if digits.is_empty() && h == 0 {
                    // Leading zeros only shift fractional digits.
                } else if digits.len() < HEX_DIGITS_KEPT {
                    digits.push(h);
                } else {
                    sticky |= h != 0;
                    scale = scale.saturating_add(4);
                }
                if seen_point {
                    scale = scale.saturating_sub(4);
                }
            }
            None => break,
        }
        i = i.saturating_add(1);
    }
    if !any {
        return None;
    }
    if matches!(s.get(i), Some(b'p' | b'P')) {
        let mut j = i.saturating_add(1);
        let negative = match s.get(j) {
            Some(b'-') => {
                j = j.saturating_add(1);
                true
            }
            Some(b'+') => {
                j = j.saturating_add(1);
                false
            }
            _ => false,
        };
        if s.get(j).is_some_and(u8::is_ascii_digit) {
            let mut e: i64 = 0;
            while let Some(&c) = s.get(j).filter(|c| c.is_ascii_digit()) {
                e = e.saturating_mul(10).saturating_add(i64::from(c.wrapping_sub(b'0')));
                j = j.saturating_add(1);
            }
            scale = if negative { scale.saturating_sub(e) } else { scale.saturating_add(e) };
            i = j;
        }
    }
    if digits.is_empty() {
        return Some((0.0, i));
    }
    if sticky {
        digits.push(1);
        scale = scale.saturating_sub(4);
    }
    let bits = i64::try_from(digits.len()).unwrap_or(i64::MAX).saturating_mul(4);
    let top = scale.saturating_add(bits);
    let value = if top > 1100 {
        f64::INFINITY
    } else if top < -1200 {
        0.0
    } else {
        hex_to_decimal_string(&digits, scale).parse::<f64>().unwrap_or(0.0)
    };
    Some((value, i))
}

/// C's `strtod` in the C locale: `(value, bytes consumed)`; nothing consumed means no number.
///
/// Accepts leading white space, a sign, decimal numbers with an optional exponent, C99 hex
/// floats, `inf`/`infinity` and `nan`/`nan(chars)` (case-insensitive). Decimal conversion is
/// correctly rounded.
pub(crate) fn strtod(s: &[u8]) -> (f64, usize) {
    let mut i = s.iter().take_while(|&&c| is_c_space(c)).count();
    let negative = match s.get(i) {
        Some(b'-') => {
            i = i.saturating_add(1);
            true
        }
        Some(b'+') => {
            i = i.saturating_add(1);
            false
        }
        _ => false,
    };
    let sign = |v: f64| if negative { -v } else { v };
    let rest = s.get(i..).unwrap_or_default();
    if starts_with_ci(rest, b"inf") {
        let n = if starts_with_ci(rest, b"infinity") { 8 } else { 3 };
        return (sign(f64::INFINITY), i.saturating_add(n));
    }
    if starts_with_ci(rest, b"nan") {
        let mut n = 3usize;
        if rest.get(3) == Some(&b'(') {
            let inner = rest
                .get(4..)
                .unwrap_or_default()
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_')
                .count();
            if rest.get(inner.saturating_add(4)) == Some(&b')') {
                n = inner.saturating_add(5);
            }
        }
        return (sign(f64::NAN), i.saturating_add(n));
    }
    if rest.first() == Some(&b'0') && matches!(rest.get(1), Some(b'x' | b'X')) {
        return match parse_hex_float(rest.get(2..).unwrap_or_default()) {
            Some((v, n)) => (sign(v), i.saturating_add(2).saturating_add(n)),
            None => (sign(0.0), i.saturating_add(1)),
        };
    }
    // Decimal: digits, optional point and digits, optional exponent.
    let mut mantissa = String::new();
    let mut frac_digits: i64 = 0;
    let mut any = false;
    let mut seen_point = false;
    let mut j = i;
    loop {
        match s.get(j).copied() {
            Some(b'.') if !seen_point => seen_point = true,
            Some(c) if c.is_ascii_digit() => {
                any = true;
                if !(mantissa.is_empty() && c == b'0') {
                    mantissa.push(char::from(c));
                }
                if seen_point {
                    frac_digits = frac_digits.saturating_add(1);
                }
            }
            _ => break,
        }
        j = j.saturating_add(1);
    }
    if !any {
        return (0.0, 0);
    }
    let mut exp: i64 = 0;
    if matches!(s.get(j), Some(b'e' | b'E')) {
        let mut k = j.saturating_add(1);
        let negative_exp = match s.get(k) {
            Some(b'-') => {
                k = k.saturating_add(1);
                true
            }
            Some(b'+') => {
                k = k.saturating_add(1);
                false
            }
            _ => false,
        };
        if s.get(k).is_some_and(u8::is_ascii_digit) {
            while let Some(&c) = s.get(k).filter(|c| c.is_ascii_digit()) {
                exp = exp.saturating_mul(10).saturating_add(i64::from(c.wrapping_sub(b'0')));
                k = k.saturating_add(1);
            }
            if negative_exp {
                exp = exp.saturating_neg();
            }
            j = k;
        }
    }
    let value = if mantissa.is_empty() {
        0.0
    } else {
        let e = exp.saturating_sub(frac_digits).clamp(-1_000_000_000, 1_000_000_000);
        mantissa.push('e');
        mantissa.push_str(&e.to_string());
        mantissa.parse::<f64>().unwrap_or(0.0)
    };
    (sign(value), j)
}

/// C's `atof` (C locale).
pub(crate) fn atof(s: &[u8]) -> f64 {
    strtod(s).0
}

/// `int stoi(string)`: C's `strtol` with base 0 (a `0x` prefix selects hex, a leading `0`
/// octal), truncated to 32 bits. FTE uses `atoi` (decimal only) although its documentation
/// promises the prefixes; qcvm follows the documentation.
///
/// # Errors
/// Never.
pub fn stoi<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = strtol(vm.arg_str(0), 0) as i32;
    vm.ret_i32(v);
    Ok(())
}

/// `int stoh(string)`: C's `strtoul` with base 16, truncated to 32 bits.
///
/// # Errors
/// Never.
pub fn stoh<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = strtoul(vm.arg_str(0), 16) as u32;
    vm.ret_raw([v, 0, 0]);
    Ok(())
}

/// `int ftoi(float)`: truncation toward zero (x86: out of range or NaN gives `INT_MIN`).
///
/// # Errors
/// Never.
pub fn ftoi<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = f2i(vm.arg_f32(0));
    vm.ret_i32(v);
    Ok(())
}

/// `__uint ftou(float)`: C's float → `unsigned` conversion as x86-64 compilers emit it.
///
/// # Errors
/// Never.
pub fn ftou<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = f2u(vm.arg_f32(0));
    vm.ret_raw([v, 0, 0]);
    Ok(())
}

/// The bit-field form of `itof`/`utof`: `(value >> shift) & mask(count)` (count 32 = no mask;
/// shift and count are taken modulo 32 like x86 shifts), converted to float.
fn bitfield_to_float<H: Host>(vm: &Vm<H>, value: u32) -> f32 {
    let shift = f2u(vm.arg_f32(1));
    let count = if vm.argc() > 2 { f2u(vm.arg_f32(2)) } else { 24 };
    let mut v = value.wrapping_shr(shift);
    if count != 32 {
        v &= 1u32.wrapping_shl(count).wrapping_sub(1);
    }
    v as f32
}

/// `float itof(int value, optional float shift, float count = 24)`: the integer as a float, or
/// with two or more arguments the bit field `count` bits wide at `shift`.
///
/// # Errors
/// Never.
pub fn itof<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = if vm.argc() > 1 { bitfield_to_float(vm, vm.arg_u32(0)) } else { vm.arg_i32(0) as f32 };
    vm.ret_f32(v);
    Ok(())
}

/// `float utof(__uint value, optional float shift, float count = 24)`: like [`itof`] for an
/// unsigned integer.
///
/// # Errors
/// Never.
pub fn utof<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = if vm.argc() > 1 { bitfield_to_float(vm, vm.arg_u32(0)) } else { vm.arg_u32(0) as f32 };
    vm.ret_f32(v);
    Ok(())
}

/// `float stof(string)`: C's `atof` (as a double, then rounded to float).
///
/// # Errors
/// Never.
pub fn stof<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = atof(vm.arg_str(0)) as f32;
    vm.ret_f32(v);
    Ok(())
}

/// FTE's `stov` parse: an optional leading `'`, then up to three numbers separated by spaces or
/// tabs; parsing stops at a `'` or at something that is not a number.
pub(crate) fn parse_vector(s: &[u8]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    let mut i = usize::from(s.first() == Some(&b'\''));
    for slot in &mut out {
        while matches!(s.get(i), Some(b' ' | b'\t')) {
            i = i.saturating_add(1);
        }
        let rest = s.get(i..).unwrap_or_default();
        *slot = atof(rest) as f32;
        let c = rest.first().copied().unwrap_or(0);
        if *slot == 0.0 && c != b'-' && c != b'+' && !c.is_ascii_digit() {
            break;
        }
        while s.get(i).is_some_and(|c| !matches!(c, b' ' | b'\t' | b'\'')) {
            i = i.saturating_add(1);
        }
        if s.get(i) == Some(&b'\'') {
            break;
        }
    }
    out
}

/// `vector stov(string...)`: parses `'x y z'` (see [`parse_vector`]); the arguments are
/// concatenated.
///
/// # Errors
/// Never.
pub fn stov<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = parse_vector(&args_concat(vm, 0));
    vm.ret_vec(v);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn strtod_edge_cases() {
        assert_eq!(strtod(b"  12.5xyz"), (12.5, 6));
        assert_eq!(strtod(b".5"), (0.5, 2));
        assert_eq!(strtod(b"5."), (5.0, 2));
        assert_eq!(strtod(b"1e3"), (1000.0, 3));
        assert_eq!(strtod(b"1e"), (1.0, 1));
        assert_eq!(strtod(b"1e+"), (1.0, 1));
        assert_eq!(strtod(b"-"), (0.0, 0));
        assert_eq!(strtod(b"."), (0.0, 0));
        assert_eq!(strtod(b"0x10"), (16.0, 4));
        assert_eq!(strtod(b"0x1p4"), (16.0, 5));
        assert_eq!(strtod(b"0x1.8p1"), (3.0, 7));
        assert_eq!(strtod(b"0x.8"), (0.5, 4));
        assert_eq!(strtod(b"0x"), (0.0, 1));
        assert_eq!(strtod(b"0xg"), (0.0, 1));
        assert_eq!(strtod(b"0x1p"), (1.0, 3));
        assert_eq!(strtod(b"-0x1p-1074").0, -5e-324);
        assert_eq!(strtod(b"0x1p-1080").0, 0.0);
        assert_eq!(strtod(b"0x1p99999999999999").0, f64::INFINITY);
        assert_eq!(strtod(b"0x1.fffffffffffff8p0").0, 2.0);
        assert_eq!(
            strtod(b"0x1.fffffffffffff7ffffffffffffffffffffffffffff1p0").0,
            1.9999999999999998
        );
        assert_eq!(strtod(b"INF"), (f64::INFINITY, 3));
        assert_eq!(strtod(b"-Infinity!"), (f64::NEG_INFINITY, 9));
        let (n, len) = strtod(b"nan(abc)x");
        assert!(n.is_nan() && n.is_sign_positive() && len == 8);
        let (n, len) = strtod(b"-nan(");
        assert!(n.is_nan() && n.is_sign_negative() && len == 4);
        assert_eq!(strtod(b"1e-99999999999999999999").0, 0.0);
        assert_eq!(strtod(b"1e99999999999999999999").0, f64::INFINITY);
        assert_eq!(strtod(b"000000000000000000000000000001e-5").0, 1e-5);
        assert_eq!(strtod(b"2.4703282292062328e-324").0, 5e-324);
        assert_eq!(strtod(b"-0").0.to_bits(), (-0.0f64).to_bits());
    }

    #[test]
    fn c_integers() {
        assert_eq!(strtol(b"  -12abc", 0), -12);
        assert_eq!(strtol(b"0x1f", 0), 31);
        assert_eq!(strtol(b"010", 0), 8);
        assert_eq!(strtol(b"08", 0), 0);
        assert_eq!(strtol(b"0x", 0), 0);
        assert_eq!(strtol(b"99999999999999999999", 10), i64::MAX);
        assert_eq!(strtol(b"-99999999999999999999", 10), i64::MIN);
        assert_eq!(strtol(b"-9223372036854775808", 10), i64::MIN);
        assert_eq!(strtoul(b"-1", 16), u64::MAX);
        assert_eq!(strtoul(b"0x1F", 16), 31);
        assert_eq!(strtoul(b"1ffffffffffffffff", 16), u64::MAX);
    }

    #[test]
    fn ftos_exact() {
        assert_eq!(ftos_text(0.1), b"0.100000001");
        assert_eq!(ftos_text(-0.0), b"0");
        assert_eq!(ftos_text(f32::NAN), b"1.#NAN");
        assert_eq!(ftos_text(-f32::NAN), b"-1.#NAN");
    }
}
