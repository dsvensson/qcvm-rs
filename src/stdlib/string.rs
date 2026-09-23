// SPDX-License-Identifier: MIT OR Apache-2.0

//! String builtins: strlen, strcat, substring, strconv, strpad, strreplace, strncmp, info strings,
//! uri escaping, decolorizing, … (docs/spec/strings.md).
//!
//! Offsets and lengths count bytes unless the VM's `utf8` charset setting (FTE's `utf8_enable`)
//! is on, in which case the builtins FTE makes UTF-8 aware count characters of the configured
//! scheme instead.

use std::cmp::Ordering;

use crate::builtins::Builtins;
use crate::error::{ErrorKind, Resource, VmError};
use crate::host::Host;
use crate::stdlib::charset::{self, REPLACEMENT};
use crate::stdlib::format::quote_string;
use crate::stdlib::util::{arg_int, args_concat, opt_f32};
use crate::value::StrRef;
use crate::vm::num::f2i;
use crate::vm::strings::{StrKind, classify};
use crate::vm::{CharScheme, Charset, Vm};

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("strlen", strlen::<H>);
    b.set("memstrsize", memstrsize::<H>);
    b.set("strcat", strcat::<H>);
    b.set("strzone", strcat::<H>);
    b.set("strunzone", strunzone::<H>);
    b.set("substring", substring::<H>);
    b.set("strstrofs", strstrofs::<H>);
    b.set("str2chr", str2chr::<H>);
    b.set("chr2str", chr2str::<H>);
    b.set("strconv", strconv::<H>);
    b.set("strpad", strpad::<H>);
    b.set("strtrim", strtrim::<H>);
    b.set("strreplace", strreplace::<H>);
    b.set("strireplace", strireplace::<H>);
    b.set("strncmp", strncmp::<H>);
    b.set("strcmp", strncmp::<H>);
    b.set("strcasecmp", strcasecmp::<H>);
    b.set("strncasecmp", strcasecmp::<H>);
    b.set("strtolower", strtolower::<H>);
    b.set("strtoupper", strtoupper::<H>);
    b.set("strdecolorize", strdecolorize::<H>);
    b.set("strlennocol", strlennocol::<H>);
    b.set("infoadd", infoadd::<H>);
    b.set("infoget", infoget::<H>);
    b.set("uri_escape", uri_escape::<H>);
    b.set("uri_unescape", uri_unescape::<H>);
    b.set("argescape", argescape::<H>);
    b.set("instr", instr::<H>);
    b.set("validstring", validstring::<H>);
    b.set("altstr_count", altstr_count::<H>);
    b.set("altstr_prepare", altstr_prepare::<H>);
    b.set("altstr_get", altstr_get::<H>);
    b.set("altstr_set", altstr_set::<H>);
}

fn charset<H: Host>(vm: &Vm<H>) -> Charset {
    vm.config().charset
}

/// A byte count or offset as a QuakeC float.
fn count_f32(n: usize) -> f32 {
    n as f32
}

/// FTE's `unicode_byteofsfromcharofs` with a C `int` offset: negative offsets (huge as unsigned)
/// run to the end.
fn byte_ofs(s: &[u8], chars: i32, scheme: CharScheme) -> usize {
    match usize::try_from(chars) {
        Ok(n) => charset::byte_offset(s, n, scheme),
        Err(_) => s.len(),
    }
}

// ---- length, concatenation, substrings --------------------------------------------------------

/// `float strlen(string)`: the length in bytes, or in characters with `utf8` on.
///
/// # Errors
/// Never.
pub fn strlen<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let cs = charset(vm);
    let s = vm.arg_str(0);
    let n = if cs.utf8 { charset::char_count(s, cs.scheme) } else { s.len() };
    vm.ret_f32(count_f32(n));
    Ok(())
}

/// `float memstrsize(string)`: the length in bytes, whatever the charset settings.
///
/// # Errors
/// Never.
pub fn memstrsize<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = vm.arg_str(0).len();
    vm.ret_f32(count_f32(n));
    Ok(())
}

/// `string strcat(string...)` (also `strzone`): all arguments joined, without a length limit.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn strcat<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    // Refuse before building a result the temp strings have no room for.
    let total = (0..vm.argc().min(8)).fold(0usize, |n, i| n.saturating_add(vm.arg_str(i).len()));
    if !vm.core.strings.fits(total) {
        return Err(ErrorKind::OutOfMemory(Resource::TempStrings).into());
    }
    let mut out = Vec::with_capacity(total);
    for i in 0..vm.argc().min(8) {
        out.extend_from_slice(vm.arg_str(i));
    }
    vm.ret_str(&out)
}

/// `void strunzone(string)`: does nothing (temp strings are garbage collected).
///
/// # Errors
/// Never.
pub fn strunzone<H: Host>(_vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    Ok(())
}

/// `string substring(string s, float start, float length)`: negative `start` counts from the
/// end, negative `length` leaves that many characters minus one off the end (`-1` = to the end).
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn substring<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let cs = charset(vm);
    let s = vm.arg_str(0);
    let mut start = arg_int(vm, 1);
    let mut length = arg_int(vm, 2);
    let slen = i32::try_from(if cs.utf8 { charset::char_count(s, cs.scheme) } else { s.len() })
        .unwrap_or(i32::MAX);
    if start < 0 {
        start = slen.wrapping_add(start);
    }
    if length < 0 {
        length = slen.wrapping_sub(start).wrapping_add(length.wrapping_add(1));
    }
    if start < 0 {
        start = 0;
    }
    let out = if start >= slen || length <= 0 {
        Vec::new()
    } else {
        let length = length.min(slen.wrapping_sub(start));
        let (from, n) = if cs.utf8 {
            let from = byte_ofs(s, start, cs.scheme);
            let rest = s.get(from..).unwrap_or_default();
            (from, byte_ofs(rest, length, cs.scheme))
        } else {
            (usize::try_from(start).unwrap_or(0), usize::try_from(length).unwrap_or(0))
        };
        s.get(from..).unwrap_or_default().iter().take(n).copied().collect()
    };
    vm.ret_str(&out)
}

/// The first occurrence of `needle` in `haystack` (an empty needle matches at 0). Linear time
/// for long needles (Knuth–Morris–Pratt), like glibc's `strstr`, so untrusted progs cannot make
/// a search quadratic.
pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() <= 16 {
        return haystack.windows(needle.len()).position(|w| w == needle);
    }
    let at = |k: usize| needle.get(k).copied();
    // fail[i]: length of the longest proper border of needle[..=i].
    let mut fail = vec![0usize; needle.len()];
    let mut k = 0usize;
    for (i, &c) in needle.iter().enumerate().skip(1) {
        while k > 0 && at(k) != Some(c) {
            k = fail.get(k.saturating_sub(1)).copied().unwrap_or(0);
        }
        if at(k) == Some(c) {
            k = k.saturating_add(1);
        }
        if let Some(f) = fail.get_mut(i) {
            *f = k;
        }
    }
    let mut k = 0usize;
    for (i, &c) in haystack.iter().enumerate() {
        while k > 0 && at(k) != Some(c) {
            k = fail.get(k.saturating_sub(1)).copied().unwrap_or(0);
        }
        if at(k) == Some(c) {
            k = k.saturating_add(1);
        }
        if k == needle.len() {
            return Some(i.saturating_add(1).saturating_sub(k));
        }
    }
    None
}

/// `float strstrofs(string s, string sub, optional float start)`: the offset of the first `sub`
/// at or after `start`, or -1.
///
/// # Errors
/// Never.
pub fn strstrofs<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let cs = charset(vm);
    let s = vm.arg_str(0);
    let sub = vm.arg_str(1);
    let first = f2i(opt_f32(vm, 2, 0.0));
    let first = if cs.utf8 {
        i64::try_from(byte_ofs(s, first, cs.scheme)).unwrap_or(0)
    } else {
        i64::from(first)
    };
    let len = i64::try_from(s.len()).unwrap_or(i64::MAX);
    let r = if first != 0 && (first < 0 || first > len) {
        -1.0
    } else {
        let from = usize::try_from(first).unwrap_or(0);
        match find(s.get(from..).unwrap_or_default(), sub) {
            Some(k) => {
                let at = from.saturating_add(k);
                count_f32(if cs.utf8 { charset::char_offset(s, at, cs.scheme) } else { at })
            }
            None => -1.0,
        }
    };
    vm.ret_f32(r);
    Ok(())
}

/// `float str2chr(string s, optional float index)`: the byte (or, with `utf8` on, the
/// character) at `index`; negative indices count from the end; out of range gives 0.
///
/// # Errors
/// Never.
pub fn str2chr<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let cs = charset(vm);
    let s = vm.arg_str(0);
    let mut ofs = f2i(opt_f32(vm, 1, 0.0));
    if cs.utf8 {
        if ofs < 0 {
            let n = i32::try_from(charset::char_count(s, cs.scheme)).unwrap_or(i32::MAX);
            ofs = n.wrapping_add(ofs);
        }
        ofs = i32::try_from(byte_ofs(s, ofs, cs.scheme)).unwrap_or(i32::MAX);
    } else if ofs < 0 {
        ofs = i32::try_from(s.len()).unwrap_or(i32::MAX).wrapping_add(ofs);
    }
    let len = i32::try_from(s.len()).unwrap_or(i32::MAX);
    let r = if ofs != 0 && (ofs < 0 || ofs > len) {
        0
    } else {
        let rest = s.get(usize::try_from(ofs).unwrap_or(0)..).unwrap_or_default();
        match rest.first() {
            None => 0,
            Some(&b) if !cs.utf8 => u32::from(b),
            Some(_) => charset::decode(rest, cs.scheme).0,
        }
    };
    vm.ret_f32(r as f32);
    Ok(())
}

/// `string chr2str(float...)`: one character per argument. Without `utf8`, codes up to 255 are
/// raw bytes; larger codes are encoded with the charset scheme using FTE's `^U`/`^{}` markup
/// where the scheme cannot represent them. A code of 0 ends the string.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn chr2str<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let cs = charset(vm);
    let mut out = Vec::new();
    for i in 0..vm.argc().min(8) {
        let ch = f2i(vm.arg_f32(i));
        if cs.utf8 || ch > 0xFF {
            charset::encode(&mut out, ch.cast_unsigned(), cs.scheme, ch > 0xFF);
        } else {
            out.push(ch as u8);
        }
    }
    if let Some(nul) = out.iter().position(|&b| b == 0) {
        out.truncate(nul);
    }
    vm.ret_str(&out)
}

// ---- conversions ------------------------------------------------------------------------------

/// Length limit of FTE's fixed conversion buffers (`strconv` input, `strpad`, info strings).
const TEMPBUF: usize = 4095;

fn conv_digit(b: u8, base: u8, mode: i32) -> u8 {
    let d = b.wrapping_sub(base);
    let new_base = match mode {
        1 => b'0',
        2 => b'0' + 0x80,
        3 => b'0' - 30,
        4 => b'0' + 0x80 - 30,
        _ => base,
    };
    d.wrapping_add(new_base)
}

fn conv_alpha(b: u8, case_base: u8, colour_base: u8, ccase: i32, redalpha: i32, i: usize) -> u8 {
    let letter = b.wrapping_sub(case_base).wrapping_sub(colour_base);
    let colour = match redalpha {
        1 => 0,
        2 => 0x80,
        5 if i.is_multiple_of(2) => 0x80,
        6 if !i.is_multiple_of(2) => 0x80,
        5 | 6 => 0,
        _ => colour_base,
    };
    let case = match ccase {
        1 => b'a',
        2 => b'A',
        _ => case_base,
    };
    letter.wrapping_add(case).wrapping_add(colour)
}

/// FTE's `strconv` byte conversion (see [`strconv`]).
pub(crate) fn convert_quake_chars(s: &[u8], ccase: i32, redalpha: i32, rednum: i32) -> Vec<u8> {
    let s = s.get(..s.len().min(TEMPBUF)).unwrap_or_default();
    s.iter()
        .enumerate()
        .map(|(i, &b)| match b {
            b'0'..=b'9' => conv_digit(b, b'0', rednum),
            0xB0..=0xB9 => conv_digit(b, 0xB0, rednum),
            0x92..=0x9B => conv_digit(b, 0x92, rednum),
            0x12..=0x1B => conv_digit(b, 0x12, rednum),
            b'a'..=b'z' => conv_alpha(b, b'a', 0, ccase, redalpha, i),
            b'A'..=b'Z' => conv_alpha(b, b'A', 0, ccase, redalpha, i),
            0xE1..=0xFA => conv_alpha(b, b'a', 0x80, ccase, redalpha, i),
            0xC1..=0xDA => conv_alpha(b, b'A', 0x80, ccase, redalpha, i),
            _ if b & 0x7F < 0x10 || redalpha == 0 => b,
            _ => match redalpha {
                1 => b & 0x7F,
                2 => b | 0x80,
                _ => b,
            },
        })
        .collect()
}

/// `string strconv(float ccase, float redalpha, float redchars, string...)`: converts Quake
/// characters between cases and white/red/gold colours (ccase: 1 lower, 2 upper; redalpha: 1
/// white, 2 red, 5/6 alternating; redchars for digits: 1 white, 2 red, 3 gold low, 4 gold high).
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn strconv<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let ccase = arg_int(vm, 0);
    let redalpha = arg_int(vm, 1);
    let rednum = arg_int(vm, 2);
    let out = convert_quake_chars(&args_concat(vm, 3), ccase, redalpha, rednum);
    vm.ret_str(&out)
}

/// `string strpad(float pad, string...)`: pads with spaces to `pad` bytes, on the right for a
/// positive `pad`, on the left for a negative one (at most 4095 bytes in all).
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn strpad<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let pad = i64::from(arg_int(vm, 0));
    let src = args_concat(vm, 1);
    let len = i64::try_from(src.len()).unwrap_or(i64::MAX);
    let max = i64::try_from(TEMPBUF).unwrap_or(0);
    let mut out = Vec::new();
    if pad < 0 {
        let spaces = pad.saturating_neg().saturating_sub(len).clamp(0, max);
        let spaces = usize::try_from(spaces).unwrap_or(0);
        out.resize(spaces, b' ');
        let room = TEMPBUF.saturating_sub(spaces);
        out.extend_from_slice(src.get(..src.len().min(room)).unwrap_or_default());
    } else {
        out.extend_from_slice(src.get(..src.len().min(TEMPBUF)).unwrap_or_default());
        let spaces = usize::try_from(pad.min(max).saturating_sub(len)).unwrap_or(0);
        out.resize(out.len().saturating_add(spaces), b' ');
    }
    vm.ret_str(&out)
}

/// `string strtrim(string)`: without leading and trailing spaces, tabs, newlines and carriage
/// returns.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn strtrim<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r');
    let s = vm.arg_str(0);
    let start = s.iter().take_while(|c| ws(c)).count();
    let rest = s.get(start..).unwrap_or_default();
    let end = rest.len().saturating_sub(rest.iter().rev().take_while(|c| ws(c)).count());
    let out = rest.get(..end).unwrap_or_default().to_vec();
    vm.ret_str(&out)
}

/// FTE's `strreplace` loop: non-overlapping left-to-right replacement, stopping once the output
/// reaches `4094 - len(replace)` bytes.
fn replace_all(search: &[u8], replace: &[u8], subject: &[u8], ignore_case: bool) -> Vec<u8> {
    if search.is_empty() {
        return subject.to_vec();
    }
    let limit = 4094usize.checked_sub(replace.len());
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < subject.len() && limit.is_some_and(|l| out.len() < l) {
        let here = subject.get(i..).unwrap_or_default();
        let hit = here.get(..search.len()).is_some_and(|w| {
            if ignore_case { w.eq_ignore_ascii_case(search) } else { w == search }
        });
        if hit {
            out.extend_from_slice(replace);
            i = i.saturating_add(search.len());
        } else {
            out.push(here.first().copied().unwrap_or(0));
            i = i.saturating_add(1);
        }
    }
    out
}

/// `string strreplace(string search, string replace, string subject)`.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn strreplace<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let out = replace_all(vm.arg_str(0), vm.arg_str(1), vm.arg_str(2), false);
    vm.ret_str(&out)
}

/// `string strireplace(string search, string replace, string subject)`: like [`strreplace`] with
/// an ASCII case-insensitive search.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn strireplace<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let out = replace_all(vm.arg_str(0), vm.arg_str(1), vm.arg_str(2), true);
    vm.ret_str(&out)
}

// ---- comparison -------------------------------------------------------------------------------

/// C's `strncmp` (with `None` = unlimited): the difference of the first differing bytes
/// (unsigned), as glibc on x86-64 returns it.
fn c_strncmp(a: &[u8], b: &[u8], n: Option<usize>, fold: bool) -> i32 {
    let n = n.unwrap_or(usize::MAX);
    let mut i = 0usize;
    while i < n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        let (x, y) = if fold { (x.to_ascii_lowercase(), y.to_ascii_lowercase()) } else { (x, y) };
        if x != y || x == 0 {
            return i32::from(x).wrapping_sub(i32::from(y));
        }
        i = i.saturating_add(1);
    }
    0
}

/// The optional `len`, `s1ofs`, `s2ofs` arguments of the comparison builtins resolved to byte
/// offsets and a byte limit.
fn compare_window<H: Host>(vm: &Vm<H>, a: &[u8], b: &[u8]) -> (usize, usize, Option<usize>) {
    let cs = charset(vm);
    let len = arg_int(vm, 2);
    let aofs = if vm.argc() > 3 { arg_int(vm, 3) } else { 0 };
    let bofs = if vm.argc() > 4 { arg_int(vm, 4) } else { 0 };
    if cs.utf8 {
        let aofs = if aofs != 0 { byte_ofs(a, aofs, cs.scheme) } else { 0 };
        let bofs = if bofs != 0 { byte_ofs(b, bofs, cs.scheme) } else { 0 };
        let la = byte_ofs(a.get(aofs..).unwrap_or_default(), len, cs.scheme);
        let lb = byte_ofs(b.get(bofs..).unwrap_or_default(), len, cs.scheme);
        (aofs, bofs, Some(la.max(lb)))
    } else {
        let clamp = |ofs: i32, s: &[u8]| match usize::try_from(ofs) {
            Ok(o) if o <= s.len() => o,
            _ => s.len(),
        };
        (clamp(aofs, a), clamp(bofs, b), usize::try_from(len).ok())
    }
}

/// `float strncmp(string s1, string s2, optional float len, optional float s1ofs, optional float
/// s2ofs)` (also `strcmp`): C's `strcmp`/`strncmp` of `s1 + s1ofs` and `s2 + s2ofs`, returning
/// the byte difference. (FTE ignores `s2ofs`; qcvm applies it.)
///
/// # Errors
/// Never.
pub fn strncmp<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let a = vm.arg_str(0);
    let b = vm.arg_str(1);
    let r = if vm.argc() > 2 {
        let (aofs, bofs, n) = compare_window(vm, a, b);
        c_strncmp(a.get(aofs..).unwrap_or_default(), b.get(bofs..).unwrap_or_default(), n, false)
    } else {
        c_strncmp(a, b, None, false)
    };
    vm.ret_f32(r as f32);
    Ok(())
}

/// `float strcasecmp(string s1, string s2, …)` (also `strncasecmp` with `len`, `s1ofs`, `s2ofs`):
/// C's case-insensitive comparison (ASCII letters folded to lower case, bytes unsigned),
/// returning -1, 0 or 1. (FTE folds to upper case and compares signed chars.)
///
/// # Errors
/// Never.
pub fn strcasecmp<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let a = vm.arg_str(0);
    let b = vm.arg_str(1);
    let r = if vm.argc() > 2 {
        let (aofs, bofs, n) = compare_window(vm, a, b);
        c_strncmp(a.get(aofs..).unwrap_or_default(), b.get(bofs..).unwrap_or_default(), n, true)
    } else {
        c_strncmp(a, b, None, true)
    };
    let r = match r.cmp(&0) {
        Ordering::Less => -1.0,
        Ordering::Equal => 0.0,
        Ordering::Greater => 1.0,
    };
    vm.ret_f32(r);
    Ok(())
}

// ---- case -------------------------------------------------------------------------------------

/// Output limit of the case and colour conversions (FTE's 8192-byte buffers).
const CASE_MAX: usize = 8191;

/// FTE's `unicode_strtoupper`/`unicode_strtolower`: decodes with the charset scheme, maps ASCII
/// letters (also inside `U+E020..=U+E07F`), re-encodes without markup.
pub(crate) fn change_case(s: &[u8], scheme: CharScheme, upper: bool) -> Vec<u8> {
    let map = |c: u32| -> u32 {
        let ascii = |c: u32| match u8::try_from(c) {
            Ok(b) if upper => u32::from(b.to_ascii_uppercase()),
            Ok(b) => u32::from(b.to_ascii_lowercase()),
            Err(_) => c,
        };
        if (0xE020..=0xE07F).contains(&c) { ascii(c & 0x7F) | (c & 0xFF80) } else { ascii(c) }
    };
    let mut out = Vec::new();
    let mut ch = Vec::new();
    for (_, c, _) in charset::chars(s, scheme) {
        ch.clear();
        charset::encode(&mut ch, map(c), scheme, false);
        if out.len().saturating_add(ch.len()) > CASE_MAX {
            break;
        }
        out.extend_from_slice(&ch);
    }
    out
}

/// `string strtolower(string)`: ASCII letters to lower case (other characters, Quake's red
/// letters included, pass through the VM's charset unchanged).
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn strtolower<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let out = change_case(vm.arg_str(0), charset(vm).scheme, false);
    vm.ret_str(&out)
}

/// `string strtoupper(string)`: ASCII letters to upper case (other characters, Quake's red
/// letters included, pass through the VM's charset unchanged).
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn strtoupper<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let out = change_case(vm.arg_str(0), charset(vm).scheme, true);
    vm.ret_str(&out)
}

// ---- markup -----------------------------------------------------------------------------------

/// Characters FTE's markup parser keeps (8192-entry buffer).
const FUN_MAX: usize = 8191;
/// Output limit of `strdecolorize`.
const DECOLOR_MAX: usize = 8190;

/// How the markup parser reads bytes that are not markup.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bytes {
    /// Quake glyphs: the high half is red text (loses its high bit), controls are glyphs.
    Quake,
    /// Byte value = code point (ISO-8859-1, and ASCII inside UTF-8 text).
    Raw,
    /// UTF-8; the first malformed sequence switches the rest of the string to `Quake`.
    Utf8,
    /// UTF-8 that never switches (`^`u8:` sections).
    ForcedUtf8,
}

/// The markup parser's output: code points with a hidden flag.
struct FunText {
    chars: Vec<(u32, bool)>,
    units: usize,
}

impl FunText {
    /// Appends a character; `false` once the buffer is full.
    fn push(&mut self, ch: u32, hidden: bool) -> bool {
        let cost = if ch > 0xFFFF { 2 } else { 1 };
        if self.units.saturating_add(cost) > FUN_MAX {
            return false;
        }
        self.units = self.units.saturating_add(cost);
        self.chars.push((ch, hidden));
        true
    }
}

fn is_hex(c: Option<&u8>) -> bool {
    c.is_some_and(u8::is_ascii_hexdigit)
}

fn hex_val(c: u8) -> u32 {
    char::from(c).to_digit(16).unwrap_or(0)
}

/// `^&` colour digits: `0-9`, `A-F` (uppercase only) or `-`.
fn is_extended_code(c: Option<&u8>) -> bool {
    c.is_some_and(|&c| c.is_ascii_digit() || (b'A'..=b'F').contains(&c) || c == b'-')
}

/// KOI8 (KOI8-R with the KOI8-U/RU additions) to Unicode, for ezQuake's `=`k8:…`=` markup.
fn koi8_to_unicode(b: u8) -> u32 {
    const HIGH: [char; 64] = [
        'ю', 'а', 'б', 'ц', 'д', 'е', 'ф', 'г', 'х', 'и', 'й', 'к', 'л', 'м', 'н', 'о', //
        'п', 'я', 'р', 'с', 'т', 'у', 'ж', 'в', 'ь', 'ы', 'з', 'ш', 'э', 'щ', 'ч', 'ъ', //
        'Ю', 'А', 'Б', 'Ц', 'Д', 'Е', 'Ф', 'Г', 'Х', 'И', 'Й', 'К', 'Л', 'М', 'Н', 'О', //
        'П', 'Я', 'Р', 'С', 'Т', 'У', 'Ж', 'В', 'Ь', 'Ы', 'З', 'Ш', 'Э', 'Щ', 'Ч', 'Ъ',
    ];
    match b {
        0xC0..=0xFF => {
            HIGH.get(usize::from(b.wrapping_sub(0xC0))).map_or(u32::from(b), |&c| c.into())
        }
        0xA3 => 0x451,
        0xB3 => 0x401,
        0xA4 => 0x454,
        0xB4 => 0x404,
        0xA6 => 0x456,
        0xB6 => 0x406,
        0xA7 => 0x457,
        0xB7 => 0x407,
        0xAE => 0x45E,
        0xBE => 0x40E,
        0xAF => 0x42A,
        _ => u32::from(b),
    }
}

/// A plain byte as the markup parser reads it.
fn literal(c: u8, mode: Bytes) -> u32 {
    if mode != Bytes::Quake {
        return u32::from(c);
    }
    match c {
        b'\n' | b'\r' | b'\t' | 0x0B | b' ' | 0x20..=0x7E => u32::from(c),
        0xA0..=0xFF => u32::from(c & 0x7F),
        _ => 0xE000 | u32::from(c),
    }
}

/// The end of a `^`u8:`/`=`k8:` section: the index of its "`=" terminator, or the end.
fn section_end(s: &[u8], from: usize) -> usize {
    s.get(from..)
        .and_then(|r| r.windows(2).position(|w| w == b"`="))
        .map_or(s.len(), |k| from.saturating_add(k))
}

/// How deeply `^`u8:`/`=`k8:` sections nest before further markers are just skipped (keeps the
/// recursion bounded on hostile input).
const SECTION_DEPTH: u32 = 16;

/// FTE's `COM_ParseFunString` as used by `strdecolorize`: parses colour and link markup into
/// characters (hidden ones flagged). Returns `false` once the buffer is full.
fn parse_markup(s: &[u8], mut mode: Bytes, out: &mut FunText, depth: u32) -> bool {
    let at = |i: usize| s.get(i).copied();
    let mut i = usize::from(matches!(at(0), Some(1 | 2)));
    let mut link: Option<usize> = None;
    while let Some(c) = at(i) {
        let next = |k: usize| s.get(i.saturating_add(k));
        if c & 0x80 != 0 && matches!(mode, Bytes::Utf8 | Bytes::ForcedUtf8) {
            let d = charset::decode_utf8(s.get(i..).unwrap_or_default());
            if d.error.is_some() && mode == Bytes::Utf8 {
                mode = Bytes::Quake;
            } else {
                let ch = if d.ch > 0x10_FFFF { REPLACEMENT } else { d.ch };
                if !out.push(ch, false) {
                    return false;
                }
                i = i.saturating_add(d.len);
                continue;
            }
        }
        if c == b'^' {
            let skip = match next(1).copied() {
                Some(b'0'..=b'9' | b'b' | b'd' | b'm' | b'a' | b'h' | b's' | b'r') => Some(2),
                Some(b'&') if is_extended_code(next(2)) && is_extended_code(next(3)) => Some(4),
                Some(b'x') => Some(if (2..5).all(|k| is_hex(next(k))) { 5 } else { 2 }),
                Some(b'[') if link.is_none() => {
                    link = Some(out.chars.len());
                    if !out.push(u32::from(b'['), false) {
                        return false;
                    }
                    Some(2)
                }
                Some(b']') => {
                    if !out.push(u32::from(b']'), link.is_some()) {
                        return false;
                    }
                    if let Some(start) = link.take() {
                        // The '[' and everything from the first '\' of the link on are hidden.
                        let last = out.chars.len().saturating_sub(1);
                        let mut k = start.saturating_add(1);
                        while k < last && out.chars.get(k).is_some_and(|&(c, _)| c != 0x5C) {
                            k = k.saturating_add(1);
                        }
                        if let Some(first) = out.chars.get_mut(start) {
                            first.1 = true;
                        }
                        for entry in out.chars.iter_mut().skip(k) {
                            entry.1 = true;
                        }
                    }
                    Some(2)
                }
                Some(b'`')
                    if s.get(i.saturating_add(2)..).is_some_and(|r| r.starts_with(b"u8:")) =>
                {
                    let from = i.saturating_add(5);
                    if depth >= SECTION_DEPTH {
                        i = from;
                        continue;
                    }
                    let end = section_end(s, from);
                    let inner = s.get(from..end).unwrap_or_default();
                    if !parse_markup(inner, Bytes::ForcedUtf8, out, depth.saturating_add(1)) {
                        return false;
                    }
                    i = end.saturating_add(2).min(s.len());
                    continue;
                }
                Some(b'U') if (2..6).all(|k| is_hex(next(k))) => {
                    let ch =
                        (2..6).fold(0, |acc, k| (acc << 4) | next(k).map_or(0, |&c| hex_val(c)));
                    if !out.push(ch, false) {
                        return false;
                    }
                    Some(6)
                }
                Some(b'{') => {
                    let mut k = 2usize;
                    let mut ch = 0u32;
                    while let Some(&h) = next(k).filter(|h| h.is_ascii_hexdigit()) {
                        ch = (ch << 4) | hex_val(h);
                        k = k.saturating_add(1);
                    }
                    if next(k) == Some(&b'}') {
                        k = k.saturating_add(1);
                    }
                    let ch = if ch > 0x10_FFFF { REPLACEMENT } else { ch };
                    if !out.push(ch, false) {
                        return false;
                    }
                    Some(k)
                }
                // `^^` is a literal caret: drop the first, read the second as a plain byte.
                Some(b'^') => {
                    i = i.saturating_add(1);
                    None
                }
                _ => None,
            };
            if let Some(n) = skip {
                i = i.saturating_add(n);
                continue;
            }
        } else if c == b'&' && next(1) == Some(&b'c') && (2..5).all(|k| is_hex(next(k))) {
            i = i.saturating_add(5);
            continue;
        } else if c == b'&' && next(1) == Some(&b'r') {
            i = i.saturating_add(2);
            continue;
        } else if c == b'=' && s.get(i.saturating_add(1)..).is_some_and(|r| r.starts_with(b"`k8:"))
        {
            let from = i.saturating_add(5);
            if depth >= SECTION_DEPTH {
                i = from;
                continue;
            }
            let end = section_end(s, from);
            let mut utf8 = Vec::new();
            for &b in s.get(from..end).unwrap_or_default() {
                charset::encode_utf8(&mut utf8, koi8_to_unicode(b));
            }
            if !parse_markup(&utf8, Bytes::ForcedUtf8, out, depth.saturating_add(1)) {
                return false;
            }
            i = end.saturating_add(2).min(s.len());
            continue;
        }
        let b = at(i).unwrap_or(0);
        let raw = if mode == Bytes::Quake { Bytes::Quake } else { Bytes::Raw };
        if !out.push(literal(b, raw), false) {
            return false;
        }
        i = i.saturating_add(1);
    }
    true
}

/// FTE's `strdecolorize`: removes colour codes and other markup (link targets are dropped,
/// `^^` becomes `^`, `^Uxxxx`/`^{x}` become characters), then re-encodes the visible characters
/// with the charset scheme (so Quake red text becomes white).
pub(crate) fn decolorize(s: &[u8], scheme: CharScheme) -> Vec<u8> {
    let mode = match scheme {
        CharScheme::Quake => Bytes::Quake,
        CharScheme::Iso8859_1 => Bytes::Raw,
        CharScheme::Utf8 => Bytes::Utf8,
    };
    let mut text = FunText { chars: Vec::new(), units: 0 };
    parse_markup(s, mode, &mut text, 0);
    let mut out = Vec::new();
    let mut ch = Vec::new();
    for &(c, hidden) in &text.chars {
        if hidden {
            continue;
        }
        ch.clear();
        charset::encode(&mut ch, c, scheme, false);
        if out.len().saturating_add(ch.len()) > DECOLOR_MAX {
            break;
        }
        out.extend_from_slice(&ch);
    }
    out
}

/// `string strdecolorize(string)`: the text without colour codes and markup
/// (`docs/spec/strings.md`).
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn strdecolorize<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let out = decolorize(vm.arg_str(0), charset(vm).scheme);
    vm.ret_str(&out)
}

/// `float strlennocol(string)`: the byte length of [`strdecolorize`]'s result.
///
/// # Errors
/// Never.
pub fn strlennocol<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = decolorize(vm.arg_str(0), charset(vm).scheme).len();
    vm.ret_f32(count_f32(n));
    Ok(())
}

// ---- info strings -----------------------------------------------------------------------------

/// Keys and values this long are not found (FTE's 1024-byte scratch buffers).
const INFO_FIELD_MAX: usize = 1022;
/// Maximum info string size including the terminator.
const INFO_MAX: usize = 4096;
/// Maximum size of one `\key\value` pair.
const INFO_PAIR_MAX: usize = 1023;
/// Keys must be shorter than this.
const INFO_KEY_MAX: usize = 256;

/// The value of `key` in a `\key\value` info string, or `""`.
pub(crate) fn info_get<'a>(info: &'a [u8], key: &[u8]) -> &'a [u8] {
    let mut rest = info.strip_prefix(b"\\").unwrap_or(info);
    loop {
        let Some(k) = rest.iter().position(|&c| c == b'\\') else { return b"" };
        let pkey = rest.get(..k).unwrap_or_default();
        if pkey.len() >= INFO_FIELD_MAX {
            return b"";
        }
        rest = rest.get(k.saturating_add(1)..).unwrap_or_default();
        let v = rest.iter().position(|&c| c == b'\\').unwrap_or(rest.len());
        let value = rest.get(..v).unwrap_or_default();
        if value.len() >= INFO_FIELD_MAX {
            return b"";
        }
        if pkey == key {
            return value;
        }
        if v >= rest.len() {
            return b"";
        }
        rest = rest.get(v.saturating_add(1)..).unwrap_or_default();
    }
}

/// Removes the first `\key\value` pair with this key.
fn info_remove(info: &mut Vec<u8>, key: &[u8]) {
    let mut start = 0usize;
    loop {
        let mut p = start;
        if info.get(p) == Some(&b'\\') {
            p = p.saturating_add(1);
        }
        let tail = info.get(p..).unwrap_or_default();
        let Some(k) = tail.iter().position(|&c| c == b'\\') else { return };
        let matches = tail.get(..k) == Some(key);
        p = p.saturating_add(k).saturating_add(1);
        let tail = info.get(p..).unwrap_or_default();
        let end = p.saturating_add(tail.iter().position(|&c| c == b'\\').unwrap_or(tail.len()));
        if matches {
            info.drain(start..end);
            return;
        }
        if end >= info.len() {
            return;
        }
        start = end;
    }
}

/// FTE's `Info_SetValueForStarKey` on an info string: returns the new string, or the unchanged
/// one with the reason when the change is rejected.
pub(crate) fn info_set(info: &[u8], key: &[u8], value: &[u8]) -> (Vec<u8>, Option<&'static str>) {
    let original = info.get(..info.len().min(INFO_MAX.saturating_sub(1))).unwrap_or_default();
    let mut s = original.to_vec();
    if key.contains(&b'\\') || value.contains(&b'\\') {
        return (s, Some("infoadd: keys and values can't contain a \\"));
    }
    if key.contains(&b'"') || value.contains(&b'"') {
        return (s, Some("infoadd: keys and values can't contain a \""));
    }
    if key.len() >= INFO_KEY_MAX {
        return (s, Some("infoadd: keys must be shorter than 256 characters"));
    }
    loop {
        let old = info_get(&s, key);
        let needed =
            value.len().saturating_add(s.len()).saturating_add(1).saturating_sub(old.len());
        if !old.is_empty() && needed > INFO_MAX {
            if !info_get(&s, b"*ver").is_empty() {
                info_remove(&mut s, b"*ver");
                continue;
            }
            return (s, Some("infoadd: info string length exceeded"));
        }
        break;
    }
    info_remove(&mut s, key);
    if value.is_empty() {
        return (s, None);
    }
    let mut pair = Vec::with_capacity(key.len().saturating_add(value.len()).saturating_add(2));
    pair.push(b'\\');
    pair.extend_from_slice(key);
    pair.push(b'\\');
    pair.extend_from_slice(value);
    pair.truncate(INFO_PAIR_MAX);
    if pair.len().saturating_add(s.len()).saturating_add(1) > INFO_MAX {
        // FTE has already removed the old pair at this point; qcvm leaves the string unchanged.
        return (original.to_vec(), Some("infoadd: info string length exceeded"));
    }
    s.extend(pair.into_iter().filter(|&c| c > 13));
    (s, None)
}

/// `string infoadd(string info, string key, string value...)`: sets (or with an empty value
/// removes) a key of a `\key\value` info string. Rejected changes (a `\` or `"` in key or value,
/// a key of 256+ bytes, a result over 4096 bytes) return the string unchanged with a warning.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn infoadd<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let value = args_concat(vm, 2);
    let (out, warning) = info_set(vm.arg_str(0), vm.arg_str(1), &value);
    if let Some(w) = warning {
        vm.warn(w);
    }
    vm.ret_str(&out)
}

/// `string infoget(string info, string key)`: the value of `key`, or `""`.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn infoget<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let out = info_get(vm.arg_str(0), vm.arg_str(1)).to_vec();
    vm.ret_str(&out)
}

// ---- URIs and quoting -------------------------------------------------------------------------

/// `string uri_escape(string)`: percent-encodes every byte except `A-Z a-z 0-9 . - _ ~`.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn uri_escape<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = Vec::new();
    for &c in vm.arg_str(0) {
        if out.len() >= 8188 {
            break;
        }
        if c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_' | b'~') {
            out.push(c);
        } else {
            out.push(b'%');
            out.push(HEX.get(usize::from(c >> 4)).copied().unwrap_or(b'0'));
            out.push(HEX.get(usize::from(c & 15)).copied().unwrap_or(b'0'));
        }
    }
    vm.ret_str(&out)
}

/// `string uri_unescape(string)`: decodes `%XX` escapes (a decoded NUL ends the string).
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn uri_unescape<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let s = vm.arg_str(0);
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(&c) = s.get(i) {
        if out.len() >= 8190 {
            break;
        }
        let hi = s.get(i.saturating_add(1)).filter(|c| c.is_ascii_hexdigit());
        let lo = s.get(i.saturating_add(2)).filter(|c| c.is_ascii_hexdigit());
        match (c, hi, lo) {
            (b'%', Some(&h), Some(&l)) => {
                out.push(((hex_val(h) << 4) | hex_val(l)) as u8);
                i = i.saturating_add(3);
            }
            _ => {
                out.push(c);
                i = i.saturating_add(1);
            }
        }
    }
    if let Some(nul) = out.iter().position(|&b| b == 0) {
        out.truncate(nul);
    }
    vm.ret_str(&out)
}

/// `string argescape(string)`: quotes a string so the console tokenizer reads it back as one
/// argument (like sprintf's `%S`).
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn argescape<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let out = quote_string(vm.arg_str(0), 8192);
    vm.ret_str(&out)
}

// ---- misc -------------------------------------------------------------------------------------

/// `string instr(string s, string token...)`: the rest of `s` from the first occurrence of the
/// (concatenated) token, or null if there is none. For strings in VM memory the result refers
/// into `s` itself; otherwise it is a new temp string.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn instr<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let r = vm.arg_str_ref(0);
    let needle = args_concat(vm, 1);
    let s = vm.arg_str(0);
    let Some(k) = find(s, &needle) else {
        vm.ret_str_ref(StrRef(0));
        return Ok(());
    };
    if let StrKind::Linear(p) = classify(r.0)
        && let Some(q) = u32::try_from(k).ok().and_then(|k| p.checked_add(k))
        && matches!(classify(q), StrKind::Linear(_))
    {
        vm.ret_str_ref(StrRef(q));
        return Ok(());
    }
    let out = s.get(k..).unwrap_or_default().to_vec();
    vm.ret_str(&out)
}

/// `float validstring(string)`: whether the reference is not null (menu QuakeC).
///
/// # Errors
/// Never.
pub fn validstring<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let valid = vm.arg_u32(0) != 0;
    vm.ret_f32(if valid { 1.0 } else { 0.0 });
    Ok(())
}

// ---- DP alt strings (menu QuakeC) -------------------------------------------------------------
//
// An "alt string" is a list of single-quoted strings, `'one' 'two'`, where `\` escapes the next
// byte inside and outside the quotes.

/// The byte index just after the `n`-th unescaped quote (1-based), if there is one.
fn after_quote(s: &[u8], n: i64) -> Option<usize> {
    if n <= 0 {
        return None;
    }
    let mut seen = 0i64;
    let mut i = 0usize;
    while let Some(&c) = s.get(i) {
        if c == b'\\' {
            i = i.saturating_add(2);
            continue;
        }
        i = i.saturating_add(1);
        if c == b'\'' {
            seen = seen.saturating_add(1);
            if seen == n {
                return Some(i);
            }
        }
    }
    None
}

/// The index of the next unescaped quote at or after `from` (or the end of the string).
fn closing_quote(s: &[u8], from: usize) -> usize {
    let mut i = from;
    while let Some(&c) = s.get(i) {
        match c {
            b'\\' => i = i.saturating_add(2),
            b'\'' => return i,
            _ => i = i.saturating_add(1),
        }
    }
    s.len()
}

/// `float altstr_count(string)`: the number of quoted strings (unescaped quotes / 2).
///
/// # Errors
/// Never.
pub fn altstr_count<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let s = vm.arg_str(0);
    let mut quotes = 0usize;
    let mut i = 0usize;
    while let Some(&c) = s.get(i) {
        if c == b'\\' {
            i = i.saturating_add(2);
            continue;
        }
        if c == b'\'' {
            quotes = quotes.saturating_add(1);
        }
        i = i.saturating_add(1);
    }
    vm.ret_f32(count_f32(quotes / 2));
    Ok(())
}

/// `string altstr_prepare(string)`: escapes single quotes (`'` → `\'`).
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn altstr_prepare<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let mut out = Vec::new();
    for &c in vm.arg_str(0) {
        if c == b'\'' {
            out.push(b'\\');
        }
        out.push(c);
    }
    vm.ret_str(&out)
}

/// `string altstr_get(string str, float num)`: the unescaped contents of quoted string `num`
/// (0-based), or `""`.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn altstr_get<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let s = vm.arg_str(0);
    let n = i64::from(arg_int(vm, 1)).saturating_mul(2).saturating_add(1);
    let mut out = Vec::new();
    if let Some(start) = after_quote(s, n) {
        let mut i = start;
        while let Some(&c) = s.get(i) {
            match c {
                b'\'' => break,
                b'\\' => {
                    let Some(&e) = s.get(i.saturating_add(1)) else { break };
                    out.push(e);
                    i = i.saturating_add(2);
                }
                _ => {
                    out.push(c);
                    i = i.saturating_add(1);
                }
            }
        }
    }
    vm.ret_str(&out)
}

/// `string altstr_set(string str, float num, string value)`: replaces the contents of quoted
/// string `num` with `value` (which should already be escaped, see [`altstr_prepare`]). Like DP
/// and FTE, a missing string `num` appends `value`.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn altstr_set<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let s = vm.arg_str(0);
    let n = i64::from(arg_int(vm, 1)).saturating_mul(2).saturating_add(1);
    let value = vm.arg_str(2);
    let (start, end) = match after_quote(s, n) {
        Some(start) => (start, closing_quote(s, start)),
        None => (s.len(), s.len()),
    };
    let mut out = s.get(..start).unwrap_or_default().to_vec();
    out.extend_from_slice(value);
    out.extend_from_slice(s.get(end..).unwrap_or_default());
    vm.ret_str(&out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn substring_search() {
        let naive = |h: &[u8], n: &[u8]| h.windows(n.len()).position(|w| w == n);
        let hay = b"abababababcabababababababcababababababababababcabcabcX";
        for start in 0..hay.len() {
            for len in 17..=hay.len().saturating_sub(start).min(40) {
                let needle = &hay[start..start + len];
                assert_eq!(find(hay, needle), naive(hay, needle), "{start} {len}");
            }
        }
        assert_eq!(find(hay, b"ababababababababababababababababababab"), None);
        assert_eq!(find(b"abc", b""), Some(0));
        // Linear time on a pathological input.
        let hay = vec![b'a'; 1 << 20];
        let mut needle = vec![b'a'; 1 << 16];
        needle.push(b'b');
        assert_eq!(find(&hay, &needle), None);
    }

    #[test]
    fn info_strings() {
        assert_eq!(info_get(b"\\name\\bob\\team\\red", b"team"), b"red");
        assert_eq!(info_get(b"name\\bob", b"name"), b"bob");
        assert_eq!(info_get(b"\\name\\bob", b"Name"), b"");
        assert_eq!(info_get(b"\\a\\\\b\\2", b"b"), b"2");
        assert_eq!(info_get(b"\\a\\1\\b", b"b"), b"");
        let mut long = b"\\k\\".to_vec();
        long.resize(long.len() + 1022, b'v');
        assert_eq!(info_get(&long, b"k"), b"");
        long.pop();
        assert_eq!(info_get(&long, b"k").len(), 1021);
    }

    #[test]
    fn info_set_rules() {
        assert_eq!(info_set(b"", b"name", b"bob").0, b"\\name\\bob");
        assert_eq!(info_set(b"\\name\\bob\\team\\red", b"name", b"al").0, b"\\team\\red\\name\\al");
        assert_eq!(info_set(b"\\name\\bob\\team\\red", b"name", b"").0, b"\\team\\red");
        let (s, w) = info_set(b"\\name\\bob", b"x", b"a\\b");
        assert_eq!((s.as_slice(), w.is_some()), (&b"\\name\\bob"[..], true));
        assert_eq!(info_set(b"\\name\\bob", b"x", b"a\nb").0, b"\\name\\bob\\x\\ab");
    }

    #[test]
    fn nested_sections_do_not_recurse_without_bound() {
        let mut s = b"^`u8:".repeat(100_000);
        s.extend_from_slice(b"x`=y");
        assert_eq!(decolorize(&s, CharScheme::Quake), b"xy");
        let mut s = b"=`k8:".repeat(100_000);
        s.extend_from_slice(b"x`=y");
        assert_eq!(decolorize(&s, CharScheme::Quake), b"xy");
    }

    #[test]
    fn markup() {
        let q = |s: &[u8]| decolorize(s, CharScheme::Quake);
        assert_eq!(q(b"^1Red ^7White"), b"Red White");
        assert_eq!(q(b"^[link\\url\\http://x^]"), b"link");
        assert_eq!(q(b"^Uzz12"), b"^Uzz12");
        assert_eq!(q(b"=`k8:\xC1`=x"), b"?x");
        assert_eq!(decolorize(b"=`k8:\xC1`=", CharScheme::Utf8), "а".as_bytes());
    }
}
