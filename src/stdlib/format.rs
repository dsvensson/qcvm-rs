// SPDX-License-Identifier: MIT OR Apache-2.0

//! sprintf and the C printf engine it is built on (docs/spec/strings.md).
//!
//! The engine reproduces glibc's `printf` output exactly. Floating-point digits come from
//! `core::fmt`, which like glibc rounds the exact binary value correctly (exact ties to the even
//! digit); the C conventions on top (exponents of at least two digits, `%g`, flags, `inf`/`nan`,
//! strings and characters padded with spaces even with the `0` flag) are applied here. Output is
//! written to a sink with a byte cap, so huge widths or precisions never allocate more than
//! the cap.

use crate::builtins::Builtins;
use crate::error::VmError;
use crate::host::Host;
use crate::stdlib::charset;
use crate::vm::num::{d2i64, d2u64, f2i, f2u};
use crate::vm::{CharScheme, Vm};

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("sprintf", sprintf::<H>);
}

// ---- float digits -----------------------------------------------------------------------------

/// Most fraction digits the exact decimal value of a double has (for 2^-1074); `%f` digits
/// beyond them are zeros.
const MAX_FRACTION_DIGITS: usize = 1074;
/// Most significant digits the exact decimal value of a double has; `%e` digits beyond them are
/// zeros.
const MAX_SIGNIFICANT_DIGITS: usize = 767;

// ---- output ---------------------------------------------------------------------------------

/// A byte buffer that silently drops output beyond a cap.
#[derive(Clone, Debug)]
pub(crate) struct Sink {
    /// The output so far.
    pub(crate) buf: Vec<u8>,
    cap: usize,
}

impl Sink {
    /// An empty sink holding at most `cap` bytes.
    pub(crate) fn new(cap: usize) -> Self {
        Self { buf: Vec::new(), cap }
    }

    /// Bytes that still fit.
    pub(crate) fn room(&self) -> usize {
        self.cap.saturating_sub(self.buf.len())
    }

    /// Appends one byte if it fits.
    pub(crate) fn push(&mut self, b: u8) {
        if self.room() > 0 {
            self.buf.push(b);
        }
    }

    /// Appends as much of `s` as fits.
    pub(crate) fn extend(&mut self, s: &[u8]) {
        let n = s.len().min(self.room());
        self.buf.extend_from_slice(s.get(..n).unwrap_or_default());
    }

    /// Appends `n` copies of `b` (as many as fit).
    pub(crate) fn fill(&mut self, b: u8, n: usize) {
        let n = n.min(self.room());
        self.buf.resize(self.buf.len().saturating_add(n), b);
    }
}

/// A printf conversion's flags, width and precision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Spec {
    /// `-`: left-justify.
    pub(crate) left: bool,
    /// `0`: pad numbers with zeros.
    pub(crate) zero: bool,
    /// `+`: always print a sign.
    pub(crate) plus: bool,
    /// ` `: print a space for positive numbers.
    pub(crate) space: bool,
    /// `#`: alternate form.
    pub(crate) alt: bool,
    /// Minimum field width.
    pub(crate) width: usize,
    /// Precision, if given.
    pub(crate) prec: Option<usize>,
}

/// A formatted number: sign, prefix, `lead` zeros, body, `trail` zeros and suffix.
#[derive(Default)]
struct Pieces {
    sign: Option<u8>,
    prefix: &'static [u8],
    lead: usize,
    body: Vec<u8>,
    trail: usize,
    suffix: Vec<u8>,
}

impl Pieces {
    fn emit(&self, sink: &mut Sink, spec: &Spec, zero_pad: bool) {
        let len = usize::from(self.sign.is_some())
            .saturating_add(self.prefix.len())
            .saturating_add(self.lead)
            .saturating_add(self.body.len())
            .saturating_add(self.trail)
            .saturating_add(self.suffix.len());
        let pad = spec.width.saturating_sub(len);
        if !spec.left && !zero_pad {
            sink.fill(b' ', pad);
        }
        if let Some(s) = self.sign {
            sink.push(s);
        }
        sink.extend(self.prefix);
        if !spec.left && zero_pad {
            sink.fill(b'0', pad);
        }
        sink.fill(b'0', self.lead);
        sink.extend(&self.body);
        sink.fill(b'0', self.trail);
        sink.extend(&self.suffix);
        if spec.left {
            sink.fill(b' ', pad);
        }
    }
}

/// `%f` digits of `|v|` (finite) with `prec` decimals: `(body, trailing zeros)`.
fn fixed_digits(v: f64, prec: usize, alt: bool) -> (Vec<u8>, usize) {
    let exact = prec.min(MAX_FRACTION_DIGITS);
    let mut body = format!("{:.*}", exact, v.abs()).into_bytes();
    if exact == 0 && alt {
        body.push(b'.');
    }
    (body, prec.saturating_sub(exact))
}

/// `%e` digits of `|v|` (finite) with `prec` decimals: `(body, trailing zeros, decimal
/// exponent)`.
fn sci_digits(v: f64, prec: usize, alt: bool) -> (Vec<u8>, usize, i64) {
    let exact = prec.min(MAX_SIGNIFICANT_DIGITS);
    let text = format!("{:.*e}", exact, v.abs());
    let (mantissa, exp) = text.split_once('e').unwrap_or((&text, "0"));
    let mut body = mantissa.as_bytes().to_vec();
    if exact == 0 && alt {
        body.push(b'.');
    }
    (body, prec.saturating_sub(exact), exp.parse().unwrap_or(0))
}

fn exponent_suffix(x: i64, upper: bool) -> Vec<u8> {
    let mut s = vec![if upper { b'E' } else { b'e' }, if x < 0 { b'-' } else { b'+' }];
    s.extend_from_slice(format!("{:02}", x.unsigned_abs()).as_bytes());
    s
}

/// Formats a double like C's `%e`, `%f`, `%g` (or their uppercase forms) into `sink`.
pub(crate) fn fmt_float(sink: &mut Sink, v: f64, conv: u8, spec: &Spec) {
    let upper = conv.is_ascii_uppercase();
    let mut p = Pieces {
        sign: if v.is_sign_negative() {
            Some(b'-')
        } else if spec.plus {
            Some(b'+')
        } else if spec.space {
            Some(b' ')
        } else {
            None
        },
        ..Pieces::default()
    };
    if !v.is_finite() {
        let text: &[u8] = match (v.is_nan(), upper) {
            (true, false) => b"nan",
            (true, true) => b"NAN",
            (false, false) => b"inf",
            (false, true) => b"INF",
        };
        p.body = text.to_vec();
        p.emit(sink, spec, false);
        return;
    }
    match conv.to_ascii_lowercase() {
        b'f' => {
            (p.body, p.trail) = fixed_digits(v, spec.prec.unwrap_or(6), spec.alt);
        }
        b'e' => {
            let (body, trail, x) = sci_digits(v, spec.prec.unwrap_or(6), spec.alt);
            (p.body, p.trail, p.suffix) = (body, trail, exponent_suffix(x, upper));
        }
        _ => {
            let prec = spec.prec.unwrap_or(6).max(1);
            let pi = i64::try_from(prec).unwrap_or(i64::MAX);
            // The exponent after rounding to `prec` significant digits picks the style.
            let (_, _, x) = sci_digits(v, prec.saturating_sub(1), false);
            if x < pi && x >= -4 {
                let decimals = usize::try_from(pi.saturating_sub(1).saturating_sub(x)).unwrap_or(0);
                (p.body, p.trail) = fixed_digits(v, decimals, spec.alt);
            } else {
                let (body, trail, x) = sci_digits(v, prec.saturating_sub(1), spec.alt);
                (p.body, p.trail, p.suffix) = (body, trail, exponent_suffix(x, upper));
            }
            if !spec.alt {
                p.trail = 0;
                if p.body.contains(&b'.') {
                    while p.body.last() == Some(&b'0') {
                        p.body.pop();
                    }
                    if p.body.last() == Some(&b'.') {
                        p.body.pop();
                    }
                }
            }
        }
    }
    p.emit(sink, spec, spec.zero && !spec.left);
}

/// Formats an integer like C's `%d`/`%u`/`%o`/`%x`/`%X` into `sink`. `signed` enables the `+`
/// and space flags.
pub(crate) fn fmt_int(
    sink: &mut Sink,
    negative: bool,
    magnitude: u64,
    radix: u32,
    upper: bool,
    signed: bool,
    spec: &Spec,
) {
    let mut body = if magnitude == 0 && spec.prec == Some(0) {
        Vec::new()
    } else {
        match (radix, upper) {
            (8, _) => format!("{magnitude:o}"),
            (16, false) => format!("{magnitude:x}"),
            (16, true) => format!("{magnitude:X}"),
            _ => format!("{magnitude}"),
        }
        .into_bytes()
    };
    let lead = spec.prec.map_or(0, |p| p.saturating_sub(body.len()));
    if spec.alt && radix == 8 && lead == 0 && body.first() != Some(&b'0') {
        body.insert(0, b'0');
    }
    let p = Pieces {
        sign: if negative {
            Some(b'-')
        } else if signed && spec.plus {
            Some(b'+')
        } else if signed && spec.space {
            Some(b' ')
        } else {
            None
        },
        prefix: match (spec.alt && radix == 16 && magnitude != 0, upper) {
            (true, false) => b"0x",
            (true, true) => b"0X",
            (false, _) => b"",
        },
        lead,
        body,
        ..Pieces::default()
    };
    p.emit(sink, spec, spec.zero && !spec.left && spec.prec.is_none());
}

/// Formats a byte string like C's `%s` (precision and width in bytes, padded with spaces).
pub(crate) fn fmt_str(sink: &mut Sink, s: &[u8], spec: &Spec) {
    let s = spec.prec.map_or(s, |p| s.get(..p.min(s.len())).unwrap_or_default());
    let pad = spec.width.saturating_sub(s.len());
    if !spec.left {
        sink.fill(b' ', pad);
    }
    sink.extend(s);
    if spec.left {
        sink.fill(b' ', pad);
    }
}

/// FTE's `unicode_strpad`: like [`fmt_str`], but in the UTF-8 scheme width and precision count
/// characters.
fn fmt_str_chars(sink: &mut Sink, s: &[u8], spec: &Spec, scheme: CharScheme) {
    if scheme != CharScheme::Utf8 {
        fmt_str(sink, s, spec);
        return;
    }
    let max = spec.prec.unwrap_or_else(|| sink.room());
    let end = charset::byte_offset(s, max, scheme);
    let shown = s.get(..end).unwrap_or_default();
    let pad = spec.width.saturating_sub(charset::char_count(shown, scheme));
    if !spec.left {
        sink.fill(b' ', pad);
    }
    sink.extend(shown);
    if spec.left {
        sink.fill(b' ', pad);
    }
}

/// `C`'s `%.{prec}f` of `v`.
pub(crate) fn format_f(v: f64, prec: usize) -> Vec<u8> {
    let mut sink = Sink::new(usize::MAX);
    fmt_float(&mut sink, v, b'f', &Spec { prec: Some(prec), ..Spec::default() });
    sink.buf
}

/// `C`'s `%.{prec}e` of `v`.
#[allow(dead_code, reason = "shared helper for other builtin modules")]
pub(crate) fn format_e(v: f64, prec: usize) -> Vec<u8> {
    let mut sink = Sink::new(usize::MAX);
    fmt_float(&mut sink, v, b'e', &Spec { prec: Some(prec), ..Spec::default() });
    sink.buf
}

/// `C`'s `%.{prec}g` of `v`.
#[allow(dead_code, reason = "shared helper for other builtin modules")]
pub(crate) fn format_g(v: f64, prec: usize) -> Vec<u8> {
    let mut sink = Sink::new(usize::MAX);
    fmt_float(&mut sink, v, b'g', &Spec { prec: Some(prec), ..Spec::default() });
    sink.buf
}

/// FTE's `COM_QuotedString`: a string quoted so the console tokenizer reads it back as one
/// argument, limited as if written to a buffer of `bufsize` bytes. Strings containing a newline,
/// carriage return or double quote use the `\"…"` form with C-style escapes.
pub(crate) fn quote_string(s: &[u8], bufsize: usize) -> Vec<u8> {
    let mut out = Vec::new();
    if s.iter().any(|&c| matches!(c, b'\n' | b'\r' | b'"')) {
        out.extend_from_slice(b"\\\"");
        let mut budget = bufsize.saturating_sub(4);
        for &c in s {
            if budget < 2 {
                break;
            }
            let esc = match c {
                b'\n' => Some(b'n'),
                b'\r' => Some(b'r'),
                b'\t' => Some(b't'),
                b'\'' | b'"' | b'\\' | b'$' => Some(c),
                _ => None,
            };
            if let Some(e) = esc {
                out.extend_from_slice(&[b'\\', e]);
                budget = budget.saturating_sub(2);
            } else {
                out.push(c);
                budget = budget.saturating_sub(1);
            }
        }
    } else {
        out.push(b'"');
        let budget = bufsize.saturating_sub(3);
        out.extend_from_slice(s.get(..budget.min(s.len())).unwrap_or_default());
    }
    out.push(b'"');
    out
}

// ---- sprintf ----------------------------------------------------------------------------------

/// sprintf's output limit (FTE's buffer is 65536 bytes including the terminator).
const SPRINTF_MAX: usize = 65_535;

/// Reads `sprintf` arguments by parameter index; indices outside the passed arguments read as
/// zero (or `""`).
struct Args<'a, H> {
    vm: &'a Vm<H>,
    argc: usize,
}

impl<'a, H: Host> Args<'a, H> {
    fn index(&self, a: i64) -> Option<usize> {
        usize::try_from(a).ok().filter(|&a| a >= 1 && a < self.argc)
    }

    fn raw(&self, a: i64) -> [u32; 3] {
        self.index(a).map_or([0; 3], |a| self.vm.arg_raw(a))
    }

    fn f32(&self, a: i64) -> f32 {
        f32::from_bits(self.raw(a)[0])
    }

    fn u32(&self, a: i64) -> u32 {
        self.raw(a)[0]
    }

    fn u64(&self, a: i64) -> u64 {
        let [lo, hi, _] = self.raw(a);
        u64::from(lo) | (u64::from(hi) << 32)
    }

    fn f64(&self, a: i64) -> f64 {
        f64::from_bits(self.u64(a))
    }

    fn str(&self, a: i64) -> &'a [u8] {
        match self.index(a) {
            Some(a) => self.vm.arg_str(a),
            None => b"",
        }
    }
}

/// C `strtol(s, &end, 10)` on the digits at `*i` (the caller checked there is one): the value
/// truncated to `int` as x86-64 does, advancing `*i` past the digits.
fn parse_int(fmt: &[u8], i: &mut usize) -> i32 {
    let mut v: i64 = 0;
    while let Some(&c) = fmt.get(*i).filter(|c| c.is_ascii_digit()) {
        v = v.saturating_mul(10).saturating_add(i64::from(c.wrapping_sub(b'0')));
        *i = i.saturating_add(1);
    }
    v as i32
}

fn is_digit(fmt: &[u8], i: usize) -> bool {
    fmt.get(i).is_some_and(u8::is_ascii_digit)
}

/// Formats `fmt` with the builtin's arguments from parameter 1 on. Returns the output and, on a
/// format error, the warning (the output then holds the text produced before the error).
fn sprintf_impl<H: Host>(vm: &Vm<H>) -> (Vec<u8>, Option<String>) {
    let fmt = vm.arg_str(0);
    let args = Args { vm, argc: vm.argc().min(8) };
    let charset = vm.config().charset;
    let mut out = Sink::new(SPRINTF_MAX);
    let mut argpos: i64 = 1;
    let mut i = 0usize;
    let bad = |at: usize| {
        let rest = fmt.get(at..).unwrap_or_default();
        Some(format!("sprintf: bad format string: {}", String::from_utf8_lossy(rest)))
    };
    while let Some(&c) = fmt.get(i) {
        if c != b'%' {
            out.push(c);
            i = i.saturating_add(1);
            continue;
        }
        let start = i;
        i = i.saturating_add(1);
        if fmt.get(i) == Some(&b'%') {
            out.push(b'%');
            i = i.saturating_add(1);
            continue;
        }
        let mut spec = Spec::default();
        let mut width: i32 = -1;
        let mut prec: i32 = -1;
        let mut thisarg: i64 = -1;
        let mut isfloat: Option<bool> = None;
        let mut is64 = false;

        // A number right after the '%' is an argument position (`N$`) or the width.
        if is_digit(fmt, i) {
            let first = fmt.get(i).copied();
            let n = parse_int(fmt, &mut i);
            if fmt.get(i) == Some(&b'$') {
                thisarg = i64::from(n);
                i = i.saturating_add(1);
            } else {
                width = n;
                if first == Some(b'0') {
                    spec.zero = true;
                    if width == 0 {
                        width = -1;
                    }
                }
            }
        }
        // Flags and width, unless the width came first.
        if width < 0 {
            while let Some(&f) = fmt.get(i) {
                match f {
                    b'#' => spec.alt = true,
                    b'0' => spec.zero = true,
                    b'-' => spec.left = true,
                    b' ' => spec.space = true,
                    b'+' => spec.plus = true,
                    _ => break,
                }
                i = i.saturating_add(1);
            }
            if fmt.get(i) == Some(&b'*') {
                i = i.saturating_add(1);
                let at = if is_digit(fmt, i) {
                    let n = parse_int(fmt, &mut i);
                    if fmt.get(i) != Some(&b'$') {
                        return (out.buf, bad(start));
                    }
                    i = i.saturating_add(1);
                    i64::from(n)
                } else {
                    argpos = argpos.saturating_add(1);
                    argpos.saturating_sub(1)
                };
                width = f2i(args.f32(at));
                if width < 0 {
                    spec.left = true;
                    width = width.wrapping_neg();
                }
            } else if is_digit(fmt, i) {
                width = parse_int(fmt, &mut i);
                if width < 0 {
                    spec.left = true;
                    width = width.wrapping_neg();
                }
            }
        }
        // Precision.
        if fmt.get(i) == Some(&b'.') {
            i = i.saturating_add(1);
            if fmt.get(i) == Some(&b'*') {
                i = i.saturating_add(1);
                let at = if is_digit(fmt, i) {
                    let n = parse_int(fmt, &mut i);
                    if fmt.get(i) != Some(&b'$') {
                        return (out.buf, bad(start));
                    }
                    i = i.saturating_add(1);
                    i64::from(n)
                } else {
                    argpos = argpos.saturating_add(1);
                    argpos.saturating_sub(1)
                };
                prec = f2i(args.f32(at));
            } else if is_digit(fmt, i) {
                prec = parse_int(fmt, &mut i);
            } else {
                return (out.buf, bad(start));
            }
        }
        // Length modifiers.
        while let Some(&m) = fmt.get(i) {
            match m {
                b'h' => isfloat = Some(true),
                b'l' | b'L' => isfloat = Some(false),
                b'q' => is64 = true,
                b'j' | b'z' | b't' => {}
                _ => break,
            }
            i = i.saturating_add(1);
        }
        let conv = fmt.get(i).copied().unwrap_or(0);
        if matches!(conv, b'p' | b'P') {
            spec.zero = true;
            if width < 0 {
                width = 8;
            }
            isfloat = isfloat.or(Some(false));
        } else if conv == b'i' {
            isfloat = isfloat.or(Some(false));
        }
        let isfloat = isfloat.unwrap_or(true);
        if thisarg < 0 {
            thisarg = argpos;
            argpos = argpos.saturating_add(1);
        }
        spec.width = usize::try_from(width).unwrap_or(0);
        spec.prec = usize::try_from(prec).ok();

        if out.room() > 0 {
            match conv {
                b'd' | b'i' => {
                    let v: i64 = match (is64, isfloat) {
                        (true, true) => d2i64(args.f64(thisarg)),
                        (true, false) => args.u64(thisarg).cast_signed(),
                        (false, true) => d2i64(f64::from(args.f32(thisarg))),
                        (false, false) => i64::from(args.u32(thisarg).cast_signed()),
                    };
                    fmt_int(&mut out, v < 0, v.unsigned_abs(), 10, false, true, &spec);
                }
                b'o' | b'u' | b'x' | b'X' | b'p' | b'P' => {
                    let v: u64 = match (is64, isfloat) {
                        (true, true) => d2u64(args.f64(thisarg)),
                        (true, false) => args.u64(thisarg),
                        (false, true) => d2u64(f64::from(args.f32(thisarg))),
                        (false, false) => u64::from(args.u32(thisarg)),
                    };
                    let radix = match conv {
                        b'o' => 8,
                        b'u' => 10,
                        _ => 16,
                    };
                    let upper = matches!(conv, b'X' | b'P');
                    fmt_int(&mut out, false, v, radix, upper, false, &spec);
                }
                b'e' | b'E' | b'f' | b'F' | b'g' | b'G' => {
                    let v: f64 = match (is64, isfloat) {
                        (true, true) => args.f64(thisarg),
                        (true, false) => args.u64(thisarg).cast_signed() as f64,
                        (false, true) => f64::from(args.f32(thisarg)),
                        (false, false) => f64::from(args.u32(thisarg).cast_signed()),
                    };
                    fmt_float(&mut out, v, conv, &spec);
                }
                b'v' | b'V' => {
                    let g = if conv == b'V' { b'G' } else { b'g' };
                    let raw = args.raw(thisarg);
                    for (k, w) in raw.into_iter().enumerate() {
                        if k > 0 {
                            out.push(b' ');
                        }
                        let v = if isfloat {
                            f64::from(f32::from_bits(w))
                        } else {
                            f64::from(w.cast_signed())
                        };
                        fmt_float(&mut out, v, g, &spec);
                    }
                }
                b'c' => {
                    let code = if isfloat { f2u(args.f32(thisarg)) } else { args.u32(thisarg) };
                    let byte_mode = spec.alt || !charset.utf8;
                    spec.alt = false;
                    if byte_mode {
                        // C's %c writes the low byte; FTE then cuts its output at a NUL.
                        let b = (code & 0xFF) as u8;
                        let pad = spec.width.saturating_sub(1);
                        if b != 0 {
                            fmt_str(&mut out, &[b], &Spec { prec: None, ..spec });
                        } else if !spec.left {
                            out.fill(b' ', pad);
                        }
                    } else {
                        let mut enc = charset::encoded(code, charset.scheme, false);
                        if let Some(nul) = enc.iter().position(|&b| b == 0) {
                            enc.truncate(nul);
                        }
                        fmt_str_chars(&mut out, &enc, &spec, charset.scheme);
                    }
                }
                b's' | b'S' => {
                    let s = args.str(thisarg);
                    let quoted;
                    let s = if conv == b'S' {
                        quoted = quote_string(s, 65_536);
                        &quoted[..]
                    } else {
                        s
                    };
                    if spec.alt || !charset.utf8 {
                        fmt_str(&mut out, s, &spec);
                    } else {
                        fmt_str_chars(&mut out, s, &spec, charset.scheme);
                    }
                }
                _ => return (out.buf, bad(start)),
            }
        }
        i = i.saturating_add(1);
    }
    (out.buf, None)
}

/// `string sprintf(string fmt, ...)` (#627): C-style formatting with FTE's conventions.
///
/// Numeric conversions read float arguments unless `l` (int), `q` (64-bit, two slots) or `%i`/`%p`
/// say otherwise; `%v` prints a vector, `%S` a quoted string; a malformed directive ends the
/// output (with a warning). Output is limited to 65535 bytes.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn sprintf<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let (out, warning) = sprintf_impl(vm);
    if let Some(w) = warning {
        vm.warn(w);
    }
    vm.ret_str(&out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn f(v: f64, conv: u8, spec: Spec) -> String {
        let mut sink = Sink::new(usize::MAX);
        fmt_float(&mut sink, v, conv, &spec);
        String::from_utf8(sink.buf).unwrap()
    }

    fn p(prec: usize) -> Spec {
        Spec { prec: Some(prec), ..Spec::default() }
    }

    #[test]
    fn fixed_matches_rust_formatting() {
        // Rust's `{:.N}` is also correctly rounded with ties to even.
        for &v in &[0.0, 0.1, 0.5, 1.5, 2.5, 0.125, 123.456, 1e-7, 5e-324, 1.7976931348623157e308] {
            for prec in [0usize, 1, 2, 3, 6, 10, 20] {
                assert_eq!(f(v, b'f', p(prec)), format!("{v:.prec$}"), "{v} {prec}");
                assert_eq!(f(-v, b'f', p(prec)), format!("{:.prec$}", -v), "-{v} {prec}");
            }
        }
    }

    #[test]
    fn scientific_and_general() {
        assert_eq!(f(12345.0, b'e', Spec::default()), "1.234500e+04");
        assert_eq!(f(0.0, b'e', p(2)), "0.00e+00");
        assert_eq!(f(9.99, b'e', p(1)), "1.0e+01");
        assert_eq!(f(1e-300, b'E', p(3)), "1.000E-300");
        assert_eq!(f(0.1, b'g', Spec::default()), "0.1");
        assert_eq!(f(1e20, b'g', Spec::default()), "1e+20");
        assert_eq!(f(100000.0, b'g', Spec::default()), "100000");
        assert_eq!(f(1000000.0, b'g', Spec::default()), "1e+06");
        assert_eq!(f(0.0001, b'g', Spec::default()), "0.0001");
        assert_eq!(f(0.00001, b'g', Spec::default()), "1e-05");
        assert_eq!(f(9.9999995, b'g', Spec::default()), "10");
        assert_eq!(f(0.0, b'g', Spec::default()), "0");
        assert_eq!(f(1.0, b'g', Spec { alt: true, ..Spec::default() }), "1.00000");
        assert_eq!(f(f64::NAN, b'f', Spec::default()), "nan");
        assert_eq!(f(-f64::NAN, b'f', Spec::default()), "-nan");
        assert_eq!(f(f64::NEG_INFINITY, b'E', Spec::default()), "-INF");
        assert_eq!(
            f(f64::INFINITY, b'f', Spec { width: 5, zero: true, ..Spec::default() }),
            "  inf"
        );
        assert_eq!(
            f(-1.5, b'f', Spec { width: 6, zero: true, prec: Some(2), ..Spec::default() }),
            "-01.50"
        );
    }

    #[test]
    fn ties_go_to_even() {
        assert_eq!(f(0.5, b'f', p(0)), "0");
        assert_eq!(f(1.5, b'f', p(0)), "2");
        assert_eq!(f(2.5, b'f', p(0)), "2");
        assert_eq!(f(0.125, b'f', p(2)), "0.12");
        assert_eq!(f(0.375, b'f', p(2)), "0.38");
        assert_eq!(f(1048576.125, b'f', p(2)), "1048576.12");
    }

    #[test]
    fn integers() {
        let mut s = Sink::new(100);
        fmt_int(&mut s, false, 255, 16, false, false, &Spec { alt: true, ..Spec::default() });
        fmt_int(&mut s, false, 0, 10, false, false, &Spec { prec: Some(0), ..Spec::default() });
        s.push(b'|');
        fmt_int(
            &mut s,
            true,
            42,
            10,
            false,
            true,
            &Spec { width: 6, zero: true, ..Spec::default() },
        );
        s.push(b'|');
        fmt_int(&mut s, false, 8, 8, false, false, &Spec { alt: true, ..Spec::default() });
        s.push(b'|');
        fmt_int(
            &mut s,
            false,
            7,
            10,
            false,
            true,
            &Spec { prec: Some(3), width: 5, ..Spec::default() },
        );
        assert_eq!(s.buf, b"0xff|-00042|010|  007");
    }

    #[test]
    fn sink_caps_output() {
        let mut s = Sink::new(4);
        fmt_int(&mut s, false, 1, 10, false, false, &Spec { width: 1 << 40, ..Spec::default() });
        assert_eq!(s.buf, b"    ");
    }

    #[test]
    fn quoting() {
        assert_eq!(quote_string(b"a b", 100), b"\"a b\"");
        assert_eq!(quote_string(b"say \"hi\"", 100), b"\\\"say \\\"hi\\\"\"");
        assert_eq!(quote_string(b"x\ny$", 100), b"\\\"x\\ny\\$\"");
        assert_eq!(quote_string(b"abcdef", 6), b"\"abc\"");
    }

    /// Digits beyond a double's exact decimal expansion are zeros, emitted without formatting
    /// them first.
    #[test]
    fn huge_precisions() {
        let f = format_f(0.5, 3000);
        assert_eq!((&f[..3], f.len()), (&b"0.5"[..], 3002));
        assert!(f[3..].iter().all(|&c| c == b'0'));
        // 2^-1074 has exactly 1074 fraction digits, the last a 5.
        let tiny = format_f(f64::from_bits(1), 1100);
        assert_eq!((tiny.len(), tiny[1075]), (1102, b'5'));
        assert!(tiny[1076..].iter().all(|&c| c == b'0'));
        let e = format_e(1.0, 2000);
        assert_eq!((&e[..2], &e[e.len() - 4..], e.len()), (&b"1."[..], &b"e+00"[..], 2006));
        let mut s = Sink::new(10);
        fmt_float(&mut s, 1.0, b'f', &Spec { prec: Some(usize::MAX), ..Spec::default() });
        assert_eq!(s.buf, b"1.00000000");
    }
}
