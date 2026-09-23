// SPDX-License-Identifier: MIT OR Apache-2.0

//! The charset settings (`utf8_enable` and the character scheme) across the builtins that honour
//! them (docs/spec/strings.md).

use qcvm::vm::CharScheme;

use super::{eq, with_charset};
use crate::support::harness::{b, f, s};

/// "héllo" in UTF-8: six bytes, five characters.
const HELLO: &str = "h\u{e9}llo";

/// Every combination of the settings, with whether characters are counted as UTF-8.
fn configs() -> [(bool, CharScheme, bool); 6] {
    [
        (false, CharScheme::Quake, false),
        (false, CharScheme::Utf8, false),
        (false, CharScheme::Iso8859_1, false),
        (true, CharScheme::Quake, false),
        (true, CharScheme::Utf8, true),
        (true, CharScheme::Iso8859_1, false),
    ]
}

#[test]
fn lengths_and_offsets() {
    for (utf8, scheme, chars) in configs() {
        let mut h = with_charset(utf8, scheme);
        let ctx = format!("utf8={utf8} scheme={scheme:?}");
        assert_eq!(h.f("strlen", &[s(HELLO)]), if chars { 5.0 } else { 6.0 }, "{ctx}");
        assert_eq!(h.f("memstrsize", &[s(HELLO)]), 6.0, "{ctx}");
        let sub = h.s("substring", &[s(HELLO), f(1.0), f(2.0)]);
        eq(&sub, if chars { "\u{e9}l".as_bytes() } else { "\u{e9}".as_bytes() });
        let sub = h.s("substring", &[s(HELLO), f(-2.0), f(-1.0)]);
        eq(&sub, "lo");
        assert_eq!(h.f("strstrofs", &[s(HELLO), s("l")]), if chars { 2.0 } else { 3.0 }, "{ctx}");
        assert_eq!(h.f("strstrofs", &[s(HELLO), s("l"), f(3.0)]), 3.0, "{ctx}");
        let c = h.f("str2chr", &[s(HELLO), f(1.0)]);
        let want = match (utf8, scheme) {
            (true, CharScheme::Utf8) => 233.0,
            (true, CharScheme::Quake) => 0xE0C3 as f32,
            _ => 195.0,
        };
        assert_eq!(c, want, "{ctx}");
        assert_eq!(h.f("str2chr", &[s(HELLO), f(-1.0)]), 111.0, "{ctx}");
        // strncmp's length counts characters: "hél" vs "héL" differ in the third character.
        let r = h.f("strncmp", &[s(HELLO), s("h\u{e9}Llo"), f(3.0)]);
        assert_eq!(r != 0.0, chars, "{ctx}");
        let r = h.f("strncmp", &[s("xx\u{e9}a"), s("\u{e9}b"), f(1.0), f(2.0)]);
        assert_eq!(r, 0.0, "{ctx}");
        // Bytes always: strpad, strconv, uri, info, tokenizers.
        eq(h.s("strpad", &[f(8.0), s(HELLO)]), format!("{HELLO}  "));
        assert_eq!(h.f("tokenizebyseparator", &[s(HELLO), s("\u{e9}")]), 2.0);
        assert_eq!(h.f("argv_start_index", &[f(1.0)]), 3.0, "{ctx}");
    }
}

#[test]
fn chr2str_per_config() {
    for (utf8, scheme, _) in configs() {
        let mut h = with_charset(utf8, scheme);
        let got = h.s("chr2str", &[f(233.0), f(9786.0)]);
        let want: &[u8] = match (utf8, scheme) {
            (false, CharScheme::Utf8) => b"\xE9\xE2\x98\xBA",
            (false, _) => b"\xE9^U263a",
            (true, CharScheme::Quake) => b"?^U263a",
            (true, CharScheme::Utf8) => "\u{e9}\u{263a}".as_bytes(),
            (true, CharScheme::Iso8859_1) => b"\xE9^U263a",
        };
        eq(got, want);
    }
}

#[test]
fn case_and_colour_follow_the_scheme_only() {
    for (utf8, scheme, _) in configs() {
        let mut h = with_charset(utf8, scheme);
        let up = h.s("strtoupper", &[b(b"a\xE9\x0b")]);
        let want: &[u8] = match scheme {
            CharScheme::Quake => b"A\xE9\x0b",
            CharScheme::Utf8 => b"A\xEF\xBF\xBD\x0b",
            CharScheme::Iso8859_1 => b"A\xE9\x0b",
        };
        eq(up, want);
        let plain = h.s("strdecolorize", &[b(b"^2a\xE9")]);
        let want: &[u8] = match scheme {
            CharScheme::Quake => b"ai",
            CharScheme::Utf8 => b"ai",
            CharScheme::Iso8859_1 => b"a\xE9",
        };
        eq(plain, want);
    }
}
