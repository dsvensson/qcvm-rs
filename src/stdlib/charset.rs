// SPDX-License-Identifier: MIT OR Apache-2.0

//! Character decoding and encoding for the Quake, UTF-8 and ISO-8859-1 schemes (docs/spec/strings.md).
//!
//! These are the conversions FTE applies between byte strings and code points. The Quake scheme
//! maps its special glyphs (control bytes and the "red" high half) into the private-use area
//! `U+E000..=U+E0FF`; the UTF-8 decoder is lenient in the same ways as FTE's (overlong forms,
//! modified-UTF-8 NUL, CESU-8 surrogate pairs and 5/6-byte forms decode; stray bytes map into the
//! private-use area). No builtins live here; the string builtins use these helpers.

use crate::builtins::Builtins;
use crate::host::Host;
use crate::vm::CharScheme;

/// Registers this module's builtins (there are none; the helpers serve other modules).
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    let _ = b;
}

/// The replacement character.
pub(crate) const REPLACEMENT: u32 = 0xFFFD;

/// Why a UTF-8 sequence did not decode cleanly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Utf8Error {
    /// A stray continuation byte, `0xFE`/`0xFF`, or a lead byte without its continuations.
    Malformed,
    /// An overlong form, a non-character (`U+FFFE`, `U+FFFF`) or a value above `U+10FFFF`.
    Illegal,
    /// A high surrogate without a following low surrogate.
    LoneHighSurrogate,
    /// A low surrogate.
    LowSurrogate,
}

/// One decoded character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Decoded {
    /// The code point.
    pub(crate) ch: u32,
    /// Bytes consumed (at least 1).
    pub(crate) len: usize,
    /// Set if the sequence was not clean UTF-8 (the code point is still meaningful).
    pub(crate) error: Option<Utf8Error>,
}

fn is_cont(s: &[u8], i: usize) -> bool {
    s.get(i).is_some_and(|&b| b & 0xC0 == 0x80)
}

/// Decodes one sequence without surrogate handling.
fn decode_utf8_basic(s: &[u8]) -> Decoded {
    let Some(&b0) = s.first() else {
        return Decoded { ch: 0, len: 1, error: None };
    };
    // (sequence length, payload bits of the lead byte, smallest non-overlong value)
    let (n, lead_mask, min): (usize, u8, u32) = match b0 {
        0x00..=0x7F => return Decoded { ch: u32::from(b0), len: 1, error: None },
        0x80..=0xBF | 0xFE | 0xFF => {
            return Decoded {
                ch: 0xE000 | u32::from(b0),
                len: 1,
                error: Some(Utf8Error::Malformed),
            };
        }
        0xC0..=0xDF => (2, 0x1F, 0x80),
        0xE0..=0xEF => (3, 0x0F, 0x800),
        0xF0..=0xF7 => (4, 0x07, 0x1_0000),
        0xF8..=0xFB => (5, 0x03, 0x20_0000),
        0xFC..=0xFD => (6, 0x01, 0x400_0000),
    };
    if !(1..n).all(|i| is_cont(s, i)) {
        return Decoded { ch: REPLACEMENT, len: 1, error: Some(Utf8Error::Malformed) };
    }
    let ch = s
        .iter()
        .take(n)
        .skip(1)
        .fold(u32::from(b0 & lead_mask), |acc, &b| (acc << 6) | u32::from(b & 0x3F));
    // Modified UTF-8 encodes NUL as the overlong C0 80, which is accepted.
    let error = if ch >= min || (n == 2 && ch == 0) { None } else { Some(Utf8Error::Illegal) };
    Decoded { ch, len: n, error }
}

/// Decodes one UTF-8 character at the start of `s` (FTE's lenient decoder).
pub(crate) fn decode_utf8(s: &[u8]) -> Decoded {
    let mut d = decode_utf8_basic(s);
    if d.error.is_none() {
        if (0xD800..0xDC00).contains(&d.ch) {
            // CESU-8: a high surrogate followed by a low one forms one character.
            let low = decode_utf8_basic(s.get(d.len..).unwrap_or_default());
            if low.error.is_none() && (0xDC00..0xE000).contains(&low.ch) {
                d.ch = (((d.ch & 0x3FF) << 10) | (low.ch & 0x3FF)).wrapping_add(0x1_0000);
                d.len = d.len.saturating_add(low.len);
            } else {
                d.error = Some(Utf8Error::LoneHighSurrogate);
            }
        }
        if (0xDC00..0xE000).contains(&d.ch) {
            d.error = Some(Utf8Error::LowSurrogate);
        }
        if d.ch == 0xFFFE || d.ch == 0xFFFF || d.ch > 0x10_FFFF {
            d.error = Some(Utf8Error::Illegal);
        }
    }
    d
}

/// Decodes one character of `s` (which must not be empty) with `scheme`: `(code point, bytes)`.
pub(crate) fn decode(s: &[u8], scheme: CharScheme) -> (u32, usize) {
    let b = u32::from(s.first().copied().unwrap_or(0));
    match scheme {
        CharScheme::Utf8 => {
            let d = decode_utf8(s);
            (d.ch, d.len)
        }
        CharScheme::Iso8859_1 => (b, 1),
        CharScheme::Quake => {
            let special =
                b != 0 && b != 0x0A && b != 0x09 && b != 0x0D && !(0x20..0x80).contains(&b);
            (if special { 0xE000 | b } else { b }, 1)
        }
    }
}

/// Iterates over the characters of `s`: `(byte offset, code point, byte length)`.
pub(crate) fn chars(
    s: &[u8],
    scheme: CharScheme,
) -> impl Iterator<Item = (usize, u32, usize)> + '_ {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let rest = s.get(at..).filter(|r| !r.is_empty())?;
        let (ch, len) = decode(rest, scheme);
        let item = (at, ch, len);
        at = at.saturating_add(len);
        Some(item)
    })
}

/// Number of characters in `s`.
pub(crate) fn char_count(s: &[u8], scheme: CharScheme) -> usize {
    match scheme {
        CharScheme::Utf8 => chars(s, scheme).count(),
        _ => s.len(),
    }
}

/// The byte offset of character `index` of `s`, or `s.len()` if there are fewer characters.
pub(crate) fn byte_offset(s: &[u8], index: usize, scheme: CharScheme) -> usize {
    match scheme {
        CharScheme::Utf8 => chars(s, scheme).nth(index).map_or(s.len(), |(at, _, _)| at),
        _ => index.min(s.len()),
    }
}

/// The number of characters that end at or before byte offset `ofs`.
pub(crate) fn char_offset(s: &[u8], ofs: usize, scheme: CharScheme) -> usize {
    match scheme {
        CharScheme::Utf8 => {
            chars(s, scheme).take_while(|&(at, _, len)| at.saturating_add(len) <= ofs).count()
        }
        _ => ofs.min(s.len()),
    }
}

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";

fn hex_digit(v: u32) -> u8 {
    HEX_LOWER.get((v & 15) as usize).copied().unwrap_or(b'0')
}

/// Appends FTE's markup for a character the charset cannot represent: `^Uxxxx`, or `^{x…}` above
/// `U+FFFF` (lowercase hex).
fn encode_markup(out: &mut Vec<u8>, ch: u32) {
    if ch > 0xFFFF {
        out.extend_from_slice(b"^{");
        out.extend_from_slice(format!("{ch:x}").as_bytes());
        out.push(b'}');
    } else {
        out.extend_from_slice(b"^U");
        out.extend([12u32, 8, 4, 0].map(|shift| hex_digit(ch >> shift)));
    }
}

/// Appends the UTF-8 encoding of `ch` (FTE's encoder: NUL becomes the overlong `C0 80`, values
/// up to `0x7FFFFFFF` use the historical 5- and 6-byte forms). Larger values, which FTE cannot
/// encode, become U+FFFD.
pub(crate) fn encode_utf8(out: &mut Vec<u8>, ch: u32) {
    let ch = if ch > 0x7FFF_FFFF { REPLACEMENT } else { ch };
    let n: u32 = match ch {
        0 => {
            out.extend_from_slice(&[0xC0, 0x80]);
            return;
        }
        0x01..=0x7F => {
            out.push(ch as u8);
            return;
        }
        0x80..=0x7FF => 2,
        0x800..=0xFFFF => 3,
        0x1_0000..=0x1F_FFFF => 4,
        0x20_0000..=0x3FF_FFFF => 5,
        _ => 6,
    };
    let lead_bits = (0xFF00u32 >> n) & 0xFF;
    let first_shift = n.saturating_sub(1).saturating_mul(6);
    out.push((lead_bits | (ch >> first_shift)) as u8);
    for k in (0..n.saturating_sub(1)).rev() {
        out.push((0x80 | ((ch >> k.saturating_mul(6)) & 0x3F)) as u8);
    }
}

/// Appends `ch` encoded with `scheme`. Characters the scheme cannot represent become `?`, or FTE's
/// `^U`/`^{}` markup when `markup` is set.
///
/// The Quake encoder maps `U+E000..=U+E0FF` back to single bytes, including `U+E00B` (a fix: FTE
/// cannot encode `\v`), but not `U+E009`/`U+E00A`/`U+E00D` (tab, newline and carriage return are
/// kept as plain ASCII).
pub(crate) fn encode(out: &mut Vec<u8>, ch: u32, scheme: CharScheme, markup: bool) {
    let direct = match scheme {
        CharScheme::Utf8 => {
            encode_utf8(out, ch);
            return;
        }
        CharScheme::Quake => {
            (0x20..0x80).contains(&ch)
                || matches!(ch, 0x09 | 0x0A | 0x0B | 0x0D)
                || ((0xE000..=0xE0FF).contains(&ch) && !matches!(ch, 0xE009 | 0xE00A | 0xE00D))
        }
        CharScheme::Iso8859_1 => ch < 0x100 || (0xE020..0xE080).contains(&ch),
    };
    if direct {
        out.push((ch & 0xFF) as u8);
    } else if markup {
        encode_markup(out, ch);
    } else {
        out.push(b'?');
    }
}

/// The bytes of `ch` encoded with `scheme` (see [`encode`]).
pub(crate) fn encoded(ch: u32, scheme: CharScheme, markup: bool) -> Vec<u8> {
    let mut out = Vec::new();
    encode(&mut out, ch, scheme, markup);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn utf8(ch: u32) -> Vec<u8> {
        let mut v = Vec::new();
        encode_utf8(&mut v, ch);
        v
    }

    #[test]
    fn utf8_encoder_matches_std_for_scalar_values() {
        for ch in [1u32, 0x41, 0x7F, 0x80, 0x7FF, 0x800, 0xFFFD, 0x1_0000, 0x10_FFFF] {
            let c = char::from_u32(ch).unwrap();
            let mut buf = [0u8; 4];
            assert_eq!(utf8(ch), c.encode_utf8(&mut buf).as_bytes(), "{ch:#x}");
        }
        assert_eq!(utf8(0), [0xC0, 0x80]);
        assert_eq!(utf8(0x20_0000), [0xF8, 0x88, 0x80, 0x80, 0x80]);
        assert_eq!(utf8(0x7FFF_FFFF), [0xFD, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF]);
        assert_eq!(utf8(0x8000_0000), utf8(REPLACEMENT));
    }

    #[test]
    fn utf8_decoder_round_trips_long_forms() {
        for ch in [0u32, 0x41, 0x7FF, 0xFFFD, 0x10_FFFF, 0x20_0000, 0x7FFF_FFFF] {
            let bytes = utf8(ch);
            let d = decode_utf8(&bytes);
            assert_eq!((d.ch, d.len), (ch, bytes.len()), "{ch:#x}");
        }
    }

    #[test]
    fn utf8_decoder_is_lenient() {
        let d = decode_utf8(b"\x80");
        assert_eq!((d.ch, d.len, d.error), (0xE080, 1, Some(Utf8Error::Malformed)));
        let d = decode_utf8(b"\xFF");
        assert_eq!((d.ch, d.len), (0xE0FF, 1));
        let d = decode_utf8(b"\xE1bc");
        assert_eq!((d.ch, d.len), (REPLACEMENT, 1));
        let d = decode_utf8(b"\xC1\xA1");
        assert_eq!((d.ch, d.len, d.error), (0x61, 2, Some(Utf8Error::Illegal)));
        let d = decode_utf8(b"\xC0\x80");
        assert_eq!((d.ch, d.len, d.error), (0, 2, None));
        // CESU-8 surrogate pair for U+1F600.
        let d = decode_utf8(b"\xED\xA0\xBD\xED\xB8\x80");
        assert_eq!((d.ch, d.len, d.error), (0x1F600, 6, None));
        let d = decode_utf8(b"\xED\xA0\xBDx");
        assert_eq!((d.ch, d.len, d.error), (0xD83D, 3, Some(Utf8Error::LoneHighSurrogate)));
        let d = decode_utf8(b"\xED\xB8\x80");
        assert_eq!(d.error, Some(Utf8Error::LowSurrogate));
        // A six-byte form cut short is malformed (FTE would read past the end).
        let d = decode_utf8(b"\xFD\xBF\xBF\xBF\xBF");
        assert_eq!((d.ch, d.len), (REPLACEMENT, 1));
    }

    #[test]
    fn quake_scheme() {
        assert_eq!(decode(b"a", CharScheme::Quake), (0x61, 1));
        assert_eq!(decode(b"\n", CharScheme::Quake), (0x0A, 1));
        assert_eq!(decode(b"\x0B", CharScheme::Quake), (0xE00B, 1));
        assert_eq!(decode(b"\x7F", CharScheme::Quake), (0x7F, 1));
        assert_eq!(decode(b"\xE1", CharScheme::Quake), (0xE0E1, 1));
        assert_eq!(encoded(0xE0E1, CharScheme::Quake, false), b"\xE1");
        assert_eq!(encoded(0xE00B, CharScheme::Quake, false), b"\x0B");
        assert_eq!(encoded(0xE00A, CharScheme::Quake, true), b"^Ue00a");
        assert_eq!(encoded(0x263A, CharScheme::Quake, true), b"^U263a");
        assert_eq!(encoded(0x263A, CharScheme::Quake, false), b"?");
        assert_eq!(encoded(0x1F600, CharScheme::Quake, true), b"^{1f600}");
        assert_eq!(encoded(0xC8, CharScheme::Quake, false), b"?");
    }

    #[test]
    fn iso_scheme() {
        assert_eq!(decode(b"\xE9", CharScheme::Iso8859_1), (0xE9, 1));
        assert_eq!(encoded(0xE9, CharScheme::Iso8859_1, false), b"\xE9");
        assert_eq!(encoded(0xE041, CharScheme::Iso8859_1, false), b"A");
        assert_eq!(encoded(0x263A, CharScheme::Iso8859_1, true), b"^U263a");
    }

    #[test]
    fn offsets() {
        let s = "h\u{e9}llo".as_bytes();
        assert_eq!(char_count(s, CharScheme::Utf8), 5);
        assert_eq!(char_count(s, CharScheme::Quake), 6);
        assert_eq!(byte_offset(s, 2, CharScheme::Utf8), 3);
        assert_eq!(byte_offset(s, 9, CharScheme::Utf8), 6);
        assert_eq!(char_offset(s, 3, CharScheme::Utf8), 2);
        assert_eq!(char_offset(s, 2, CharScheme::Utf8), 1);
    }
}
