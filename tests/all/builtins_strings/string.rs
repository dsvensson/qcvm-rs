// SPDX-License-Identifier: MIT OR Apache-2.0

//! Length, substrings, characters, conversions, comparisons, markup, info strings, URIs
//! (docs/spec/strings.md) and the menu-only string builtins.

use qcvm::vm::CharScheme;
use qcvm::{Arg, Numbering, StrRef, VmConfig};

use super::{EXTRA, eq, harness, menu, opt_s, warnings, with_charset};
use crate::support::harness::{Harness, b, f, i, s};

#[test]
fn string_results_are_new_non_null_temps() {
    let mut h = harness();
    for (name, args) in [
        ("substring", vec![s("abc"), f(5.0), f(1.0)]),
        ("strcat", vec![]),
        ("strtrim", vec![s("   ")]),
        ("strreplace", vec![s("a"), s(""), s("aaa")]),
        ("infoget", vec![s(""), s("x")]),
        ("strdecolorize", vec![s("^1")]),
    ] {
        let r = h.call(name, &args).unwrap().str_ref();
        assert_ne!(r, StrRef(0), "{name}");
        assert!(h.vm.is_temp(r), "{name}");
        eq(h.vm.str(r), "");
    }
}

#[test]
fn strlen_strcat_strzone() {
    let mut h = harness();
    assert_eq!(h.f("strlen", &[s("hello")]), 5.0);
    assert_eq!(h.f("strlen", &[Arg::Str(StrRef(0))]), 0.0);
    assert_eq!(h.f("strlen", &[s("h\u{e9}llo")]), 6.0);
    assert_eq!(h.f("memstrsize", &[s("h\u{e9}llo")]), 6.0);
    eq(h.s("strcat", &[s("a"), s("b"), s("c")]), "abc");
    eq(h.s("strcat", &[s("a")]), "a");
    let args: Vec<_> = (0..8).map(|k| Arg::Bytes(["x", "y"][k % 2].as_bytes())).collect();
    eq(h.s("strcat", &args), "xyxyxyxy");
    // No length limit.
    let big = "z".repeat(100_000);
    assert_eq!(h.s("strcat", &[s(&big), s(&big)]).len(), 200_000);
    eq(h.s("strzone", &[s("a"), s("b")]), "ab");
    h.call("strunzone", &[s("a")]).unwrap();
}

#[test]
fn substring_examples() {
    let mut h = harness();
    let cases: &[(f32, f32, &str)] = &[
        (1.0, 3.0, "ell"),
        (-3.0, 2.0, "ll"),
        (1.0, -1.0, "ello"),
        (0.0, -2.0, "hell"),
        (-10.0, 3.0, "hel"),
        (-10.0, -1.0, "hello"),
        (3.0, 100.0, "lo"),
        (5.0, 1.0, ""),
        (2.0, 0.0, ""),
        (4.9, 1.0, "o"),
        (0.0, 3e9, ""),
        (-1.0, -1.0, "o"),
        (0.0, 5.0, "hello"),
    ];
    for &(start, len, want) in cases {
        eq(h.s("substring", &[s("hello"), f(start), f(len)]), want);
    }
}

#[test]
fn strstrofs_examples() {
    let mut h = harness();
    let mut t = |args: &[Arg<'_>]| h.f("strstrofs", args);
    assert_eq!(t(&[s("abcabc"), s("bc")]), 1.0);
    assert_eq!(t(&[s("abcabc"), s("bc"), f(2.0)]), 4.0);
    assert_eq!(t(&[s("abc"), s("")]), 0.0);
    assert_eq!(t(&[s("abc"), s(""), f(3.0)]), 3.0);
    assert_eq!(t(&[s("abc"), s(""), f(4.0)]), -1.0);
    assert_eq!(t(&[s("abc"), s("a"), f(-1.0)]), -1.0);
    assert_eq!(t(&[s("abc"), s("x")]), -1.0);
    assert_eq!(t(&[s("abc"), s("c"), f(2.0)]), 2.0);
}

#[test]
fn str2chr_examples() {
    let mut h = harness();
    let cases: &[(f32, f32)] = &[
        (0.0, 97.0),
        (2.0, 99.0),
        (3.0, 0.0),
        (4.0, 0.0),
        (-1.0, 99.0),
        (-3.0, 97.0),
        (-4.0, 0.0),
        (1.9, 98.0),
        (-0.5, 97.0),
    ];
    for &(index, want) in cases {
        assert_eq!(h.f("str2chr", &[s("abc"), f(index)]), want, "str2chr(abc, {index})");
    }
    assert_eq!(h.f("str2chr", &[s("abc")]), 97.0);
    assert_eq!(h.f("str2chr", &[b(b"\xE1"), f(0.0)]), 225.0);
    let mut h = with_charset(true, CharScheme::Quake);
    assert_eq!(h.f("str2chr", &[b(b"\xE1"), f(0.0)]), 0xE0E1 as f32);
}

#[test]
fn chr2str_examples() {
    let mut h = harness();
    let mut t = |codes: &[f32]| -> Vec<u8> {
        let args: Vec<_> = codes.iter().map(|&c| f(c)).collect();
        h.s("chr2str", &args)
    };
    eq(t(&[72.0, 105.0]), "Hi");
    eq(t(&[225.0]), b"\xE1");
    eq(t(&[9786.0]), "^U263a");
    eq(t(&[256.0]), "^U0100");
    eq(t(&[128_512.0]), "^{1f600}");
    eq(t(&[57_537.0]), b"\xC1");
    eq(t(&[57_354.0]), "^Ue00a");
    eq(t(&[65.0, 0.0, 66.0]), "A");
    eq(t(&[-1.0]), b"\xFF");
    eq(t(&[-128.0]), b"\x80");
    // The Quake encoder round-trips \v (FTE cannot encode U+E00B).
    eq(t(&[57_355.0]), b"\x0B");
    let mut h = with_charset(false, CharScheme::Utf8);
    eq(h.s("chr2str", &[f(9786.0)]), "\u{263a}");
    eq(h.s("chr2str", &[f(57_409.0)]), b"\xEE\x81\x81");
    eq(h.s("chr2str", &[f(200.0)]), b"\xC8");
    let mut h = with_charset(true, CharScheme::Quake);
    eq(h.s("chr2str", &[f(200.0)]), "?");
    eq(h.s("chr2str", &[f(0.0)]), "?");
    eq(h.s("chr2str", &[f(65.0), f(57_537.0)]), b"A\xC1");
    let mut h = with_charset(true, CharScheme::Utf8);
    eq(h.s("chr2str", &[f(0.0)]), b"\xC0\x80");
    eq(h.s("chr2str", &[f(200.0)]), "\u{c8}");
    eq(h.s("chr2str", &[f(-1.0)]), "\u{fffd}");
}

#[test]
fn case_conversion() {
    let mut h = harness();
    eq(h.s("strtoupper", &[s("Hello, World!")]), "HELLO, WORLD!");
    eq(h.s("strtolower", &[s("Hello, World!")]), "hello, world!");
    eq(h.s("strtoupper", &[b(b"\xE1bc")]), b"\xE1BC");
    // Fixed: FTE turns \v into '?'.
    eq(h.s("strtoupper", &[s("a\x0bb")]), "A\x0bB");
    eq(h.s("strtolower", &[b(b"\x01\x7F\xFF\n\tX")]), b"\x01\x7F\xFF\n\tx");
    let mut h = with_charset(false, CharScheme::Utf8);
    eq(h.s("strtoupper", &[s("h\u{e9}llo")]), "H\u{e9}LLO");
    eq(h.s("strtoupper", &[b(b"\x80")]), b"\xEE\x82\x80");
    eq(h.s("strtoupper", &[b(b"\xE1bc")]), b"\xEF\xBF\xBDBC");
    eq(h.s("strtoupper", &[b(b"\xC1\xA1")]), "A");
    eq(h.s("strtoupper", &[s("\u{e061}")]), "\u{e041}");
    eq(h.s("strtolower", &[s("\u{e041}\u{c9}")]), "\u{e061}\u{c9}");
    let mut h = with_charset(true, CharScheme::Iso8859_1);
    eq(h.s("strtoupper", &[b(b"\xE9a")]), b"\xE9A");
    // Output is limited to 8191 bytes.
    let mut h = harness();
    assert_eq!(h.s("strtoupper", &[s(&"a".repeat(9000))]).len(), 8191);
}

#[test]
fn strconv_examples() {
    let mut h = harness();
    let mut t = |c: f32, a: f32, n: f32, text: &[u8]| h.s("strconv", &[f(c), f(a), f(n), b(text)]);
    eq(t(1.0, 0.0, 0.0, b"Hello WORLD"), "hello world");
    eq(t(2.0, 0.0, 0.0, b"\xE8i"), b"\xC8I");
    eq(t(0.0, 2.0, 2.0, b"Score: 10"), b"\xD3\xE3\xEF\xF2\xE5\xBA\xA0\xB1\xB0");
    eq(t(0.0, 1.0, 1.0, b"\xD3\xE3\xEF\xF2\xE5\xBA\xA0\xB1\xB0"), "Score: 10");
    eq(t(0.0, 0.0, 3.0, b"123"), b"\x13\x14\x15");
    eq(t(0.0, 0.0, 4.0, b"05"), b"\x92\x97");
    eq(t(0.0, 5.0, 0.0, b"ab cd"), b"\xE1b c\xE4");
    eq(t(0.0, 6.0, 0.0, b"abcd"), b"a\xE2c\xE4");
    eq(t(0.0, 0.0, 5.0, b"\x92"), b"\x92");
    eq(t(0.0, 2.0, 0.0, b"\x01\x8F!"), b"\x01\x8F\xA1");
    // The arguments after the third are concatenated; input is cut to 4095 bytes.
    let mut h = harness();
    eq(h.s("strconv", &[f(2.0), f(0.0), f(0.0), s("a"), s("b")]), "AB");
    assert_eq!(h.s("strconv", &[f(0.0), f(0.0), f(0.0), s(&"x".repeat(5000))]).len(), 4095);
}

#[test]
fn strpad_strtrim() {
    let mut h = harness();
    eq(h.s("strpad", &[f(5.0), s("ab")]), "ab   ");
    eq(h.s("strpad", &[f(-5.0), s("ab")]), "   ab");
    eq(h.s("strpad", &[f(2.0), s("abcd")]), "abcd");
    eq(h.s("strpad", &[f(-2.0), s("abcd")]), "abcd");
    eq(h.s("strpad", &[f(-3.0), s("a"), s("b")]), " ab");
    eq(h.s("strpad", &[f(-5.5), s("a")]), "    a");
    assert_eq!(h.s("strpad", &[f(10_000.0), s("a")]).len(), 4095);
    assert_eq!(h.s("strpad", &[f(-10_000.0), s("a")]).len(), 4095);
    assert_eq!(h.s("strpad", &[f(0.0), s(&"a".repeat(5000))]).len(), 4095);
    eq(h.s("strtrim", &[s("  a b \n")]), "a b");
    eq(h.s("strtrim", &[s("\x0b x")]), "\x0b x");
    eq(h.s("strtrim", &[s("\t\r\nx\x0c")]), "x\x0c");
}

#[test]
fn strreplace_examples() {
    let mut h = harness();
    eq(h.s("strreplace", &[s("a"), s("bb"), s("banana")]), "bbbnbbnbb");
    eq(h.s("strreplace", &[s("aa"), s("b"), s("aaa")]), "ba");
    eq(h.s("strreplace", &[s(""), s("x"), s("abc")]), "abc");
    eq(h.s("strreplace", &[s("a"), s("a"), s("aaa")]), "aaa");
    eq(h.s("strreplace", &[s("A"), s("x"), s("abc")]), "abc");
    eq(h.s("strireplace", &[s("AB"), s("x"), s("abAb")]), "xx");
    eq(h.s("strireplace", &[s("b"), s("Q"), s("aBc")]), "aQc");
    // Output stops once it reaches 4094 - len(replace) bytes.
    assert_eq!(h.s("strreplace", &[s("x"), s("y"), s(&"a".repeat(5000))]).len(), 4093);
    eq(h.s("strreplace", &[s("x"), s(&"y".repeat(4094)), s("abc")]), "");
}

#[test]
fn strncmp_examples() {
    let mut h = with_charset(false, CharScheme::Quake);
    let mut t = |args: &[Arg<'_>]| h.f("strncmp", args);
    assert_eq!(t(&[s("hello"), s("help"), f(3.0)]), 0.0);
    assert!(t(&[s("hello"), s("help"), f(4.0)]) < 0.0);
    assert_eq!(t(&[s("xxhello"), s("hello"), f(5.0), f(2.0)]), 0.0);
    // Fixed: FTE ignores s2ofs.
    assert_eq!(t(&[s("hello"), s("xxhel"), f(3.0), f(0.0), f(2.0)]), 0.0);
    assert_eq!(t(&[s("a"), s("c")]), -2.0);
    assert_eq!(t(&[s("abc"), s("ab")]), 99.0);
    assert_eq!(t(&[s("abc"), s("abc")]), 0.0);
    assert_eq!(t(&[b(b"\xE9"), s("a")]), 136.0);
    assert_eq!(t(&[s("abc"), s("abd"), f(-1.0)]), -1.0);
    assert_eq!(t(&[s("abc"), s("xyz"), f(0.0)]), 0.0);
    assert_eq!(t(&[s("abc"), s(""), f(5.0), f(10.0)]), 0.0);
    assert_eq!(t(&[s("abc"), s(""), f(5.0), f(-1.0)]), 0.0);
    assert_eq!(h.f("strcmp", &[s("b"), s("a")]), 1.0);
}

#[test]
fn strcasecmp_uses_c_semantics() {
    let mut h = harness();
    let mut t = |name: &str, args: &[Arg<'_>]| h.f(name, args);
    assert_eq!(t("strcasecmp", &[s("abc"), s("ABC")]), 0.0);
    assert_eq!(t("strcasecmp", &[s("a"), s("b")]), -1.0);
    assert_eq!(t("strcasecmp", &[s("b"), s("A")]), 1.0);
    assert_eq!(t("strcasecmp", &[s("abc"), s("abcd")]), -1.0);
    assert_eq!(t("strcasecmp", &[s("zz"), s("A")]), 1.0);
    // Fixed: FTE folds to upper case ('_' > 'A') and compares signed chars.
    assert_eq!(t("strcasecmp", &[s("_"), s("a")]), -1.0);
    assert_eq!(t("strcasecmp", &[b(b"\xE9"), s("a")]), 1.0);
    assert_eq!(t("strncasecmp", &[s("abc"), s("ABD"), f(2.0)]), 0.0);
    assert_eq!(t("strncasecmp", &[s("abc"), s("ABD"), f(3.0)]), -1.0);
    assert_eq!(t("strncasecmp", &[s("xxABC"), s("abc"), f(3.0), f(2.0)]), 0.0);
    assert_eq!(t("strncasecmp", &[s("ABC"), s("xxabc"), f(3.0), f(0.0), f(2.0)]), 0.0);
    assert_eq!(t("strncasecmp", &[s("abc"), s("ABCD"), f(-1.0)]), -1.0);
}

#[test]
fn decolorize_quake_scheme() {
    let mut h = harness();
    let cases: &[(&[u8], &[u8])] = &[
        (b"^1Red ^7White", b"Red White"),
        (b"^^1", b"^1"),
        (b"a^", b"a^"),
        (b"^z", b"^z"),
        (b"^&F0text", b"text"),
        (b"^&-Ftext", b"text"),
        (b"^&G0x", b"^&G0x"),
        (b"^&f0x", b"^&f0x"),
        (b"^xF00red", b"red"),
        (b"^xf0ared", b"red"),
        (b"^xZZ", b"ZZ"),
        (b"^[link\\url\\http://x^]", b"link"),
        (b"^[abc", b"[abc"),
        (b"a^]b", b"a]b"),
        (b"&cf00red&r", b"red"),
        (b"&cxyz", b"&cxyz"),
        (b"rock&roll", b"rockoll"),
        (b"\x01hi", b"hi"),
        (b"\x02hi", b"hi"),
        (b"a\x01b", b"a\x01b"),
        (b"\xC8\xE9", b"Hi"),
        (b"\x80\x9F\x7F", b"\x80\x9F\x7F"),
        (b"\x0b\t\n\r", b"\x0b\t\n\r"),
        (b"^U0041", b"A"),
        (b"^U263a", b"?"),
        (b"^Ue0c1", b"\xC1"),
        (b"^{41}^{}x", b"A?x"),
        (b"^{1f600}", b"?"),
        (b"^{41", b"A"),
        (b"^b^m^h^a^d^s^r", b""),
        (b"^`u8:\xC3\xA9`=x", b"?x"),
        (b"^`u8:^1a", b"a"),
        (b"=`k8:\xC1`=b", b"?b"),
    ];
    for &(text, want) in cases {
        eq(h.s("strdecolorize", &[b(text)]), want);
        assert_eq!(h.f("strlennocol", &[b(text)]), want.len() as f32);
    }
}

#[test]
fn decolorize_requires_hex_after_caret_u() {
    // Fixed: FTE consumes six bytes after ^U without checking for hex digits.
    let mut h = harness();
    eq(h.s("strdecolorize", &[s("^Uzz12x")]), "^Uzz12x");
    eq(h.s("strdecolorize", &[s("^U004")]), "^U004");
    eq(h.s("strdecolorize", &[s("^U")]), "^U");
}

#[test]
fn decolorize_utf8_scheme() {
    let mut h = with_charset(false, CharScheme::Utf8);
    eq(h.s("strdecolorize", &[s("^U263a")]), "\u{263a}");
    eq(h.s("strdecolorize", &[s("^1h\u{e9}llo")]), "h\u{e9}llo");
    eq(h.s("strdecolorize", &[s("^{1f600}")]), "\u{1f600}");
    eq(h.s("strdecolorize", &[s("^{110000}")]), "\u{fffd}");
    eq(h.s("strdecolorize", &[b(b"^`u8:\xC3\xA9`=")]), "\u{e9}");
    eq(h.s("strdecolorize", &[b(b"=`k8:\xC1\xE1`=")]), "\u{430}\u{410}");
    // The first malformed sequence switches the rest of the string to Quake rules.
    eq(h.s("strdecolorize", &[b(b"\xC3\xA9\xFFa\x01\xE9")]), b"\xC3\xA9\x7Fa\xEE\x80\x81i");
    assert_eq!(h.f("strlennocol", &[s("^1\u{e9}")]), 2.0);
    let mut h = with_charset(false, CharScheme::Iso8859_1);
    eq(h.s("strdecolorize", &[b(b"^1\xE9\x01")]), b"\xE9\x01");
}

#[test]
fn infoget_examples() {
    let mut h = harness();
    let mut t = |info: &str, key: &str| h.s("infoget", &[s(info), s(key)]);
    eq(t("\\name\\bob\\team\\red", "team"), "red");
    eq(t("name\\bob", "name"), "bob");
    eq(t("\\name\\bob", "Name"), "");
    eq(t("\\a\\\\b\\2", "b"), "2");
    eq(t("\\a\\1\\b", "b"), "");
    eq(t("\\a\\1", "a"), "1");
    let long = format!("\\k\\{}", "v".repeat(1022));
    eq(t(&long, "k"), "");
}

#[test]
fn infoadd_examples() {
    let mut h = harness();
    let mut t = |info: &str, key: &str, value: &str| h.s("infoadd", &[s(info), s(key), s(value)]);
    eq(t("", "name", "bob"), "\\name\\bob");
    eq(t("\\name\\bob", "team", "red"), "\\name\\bob\\team\\red");
    eq(t("\\name\\bob\\team\\red", "name", "al"), "\\team\\red\\name\\al");
    eq(t("\\name\\bob\\team\\red", "name", ""), "\\team\\red");
    eq(t("\\name\\bob", "x", "a\\b"), "\\name\\bob");
    eq(t("\\name\\bob", "x", "a\nb"), "\\name\\bob\\x\\ab");
    eq(t("\\name\\bob", "x\"", "1"), "\\name\\bob");
    eq(t("", "*star", "1"), "\\*star\\1");
    eq(t("\\a\\1\\a\\2", "a", "3"), "\\a\\2\\a\\3");
    let long_key = "k".repeat(256);
    eq(t("\\a\\1", &long_key, "v"), "\\a\\1");
    // The value is the concatenation of the arguments from the third on.
    eq(h.s("infoadd", &[s(""), s("k"), s("a"), s("b")]), "\\k\\ab");
    // The pair is cut to 1023 bytes.
    let out = h.s("infoadd", &[s(""), s("k"), s(&"v".repeat(2000))]);
    assert_eq!(out.len(), 1023);
}

#[test]
fn infoadd_size_limits() {
    let mut h = harness();
    // Rejected for size: the string comes back unchanged (FTE drops the old pair first).
    let info = format!("\\k\\\\z\\{}", "w".repeat(4089));
    assert_eq!(info.len(), 4095);
    let before = warnings(&h);
    eq(h.s("infoadd", &[s(&info), s("k"), s("v")]), &info);
    assert!(warnings(&h) > before);
    // A key with a value that no longer fits: `*ver` is dropped to make room.
    let info =
        format!("\\a\\{}\\*ver\\{}\\b\\{}", "x".repeat(100), "v".repeat(50), "y".repeat(3900));
    let want = format!("\\b\\{}\\a\\{}", "y".repeat(3900), "z".repeat(150));
    eq(h.s("infoadd", &[s(&info), s("a"), s(&"z".repeat(150))]), want);
    // Too big even without `*ver`: unchanged except for the dropped `*ver`.
    let want = format!("\\a\\{}\\b\\{}", "x".repeat(100), "y".repeat(3900));
    eq(h.s("infoadd", &[s(&info), s("a"), s(&"z".repeat(200))]), want);
    // Info strings are cut to 4095 bytes first.
    let out = h.s("infoadd", &[s(&format!("\\a\\{}", "x".repeat(5000))), s("a"), s("")]);
    assert_eq!(out, b"");
}

#[test]
fn uri_escaping() {
    let mut h = harness();
    eq(h.s("uri_escape", &[s("a b")]), "a%20b");
    eq(h.s("uri_escape", &[s("100%")]), "100%25");
    eq(h.s("uri_escape", &[s("\u{e9}")]), "%C3%A9");
    eq(h.s("uri_escape", &[s("~x-y_z.0")]), "~x-y_z.0");
    eq(h.s("uri_unescape", &[s("%41")]), "A");
    eq(h.s("uri_unescape", &[s("%4")]), "%4");
    eq(h.s("uri_unescape", &[s("%zz")]), "%zz");
    eq(h.s("uri_unescape", &[s("%%41")]), "%A");
    eq(h.s("uri_unescape", &[s("a%00b")]), "a");
    eq(h.s("uri_unescape", &[s("a+b")]), "a+b");
    eq(h.s("uri_unescape", &[s("%c3%A9")]), "\u{e9}");
    assert_eq!(h.s("uri_escape", &[s(&" ".repeat(5000))]).len(), 8190);
    assert_eq!(h.s("uri_unescape", &[s(&"a".repeat(9000))]).len(), 8190);
}

#[test]
fn argescape_quotes_like_percent_s_upper() {
    let mut h = harness();
    eq(h.s("argescape", &[s("a b")]), "\"a b\"");
    eq(h.s("argescape", &[s("say \"hi\"")]), "\\\"say \\\"hi\\\"\"");
    eq(h.s("argescape", &[s("x\ny\t'$\\")]), "\\\"x\\ny\\t\\'\\$\\\\\"");
    eq(h.s("argescape", &[s("")]), "\"\"");
    assert_eq!(h.s("argescape", &[s(&"a".repeat(9000))]).len(), 8191);
}

#[test]
fn instr_refers_into_the_input() {
    let mut ofs = 0;
    let mut h = Harness::with(Numbering::Csqc, VmConfig::default(), |asm| {
        ofs = asm.string("hello world");
        Harness::named(EXTRA)(asm);
    });
    let r = h.call("instr", &[Arg::Str(StrRef(ofs)), s("wor")]).unwrap().str_ref();
    assert_eq!(r, StrRef(ofs + 6));
    eq(h.vm.str(r), "world");
    let r = h.call("instr", &[Arg::Str(StrRef(ofs)), s("")]).unwrap().str_ref();
    assert_eq!(r, StrRef(ofs));
    // Temp strings: a new temp holding the rest.
    eq(opt_s(&mut h, "instr", &[s("hello world"), s("o w")]).unwrap(), "o world");
    eq(opt_s(&mut h, "instr", &[s("hello world"), s("wo"), s("r")]).unwrap(), "world");
    assert_eq!(opt_s(&mut h, "instr", &[s("hello world"), s("xyz")]), None);
}

#[test]
fn menu_validstring() {
    let mut h = menu();
    assert_eq!(h.f("validstring", &[Arg::Str(StrRef(0))]), 0.0);
    assert_eq!(h.f("validstring", &[s("")]), 1.0);
    assert_eq!(h.f("validstring", &[s("x")]), 1.0);
}

#[test]
fn menu_altstr() {
    let mut h = menu();
    assert_eq!(h.f("altstr_count", &[s("'a' 'b' 'c'")]), 3.0);
    assert_eq!(h.f("altstr_count", &[s("'a\\'b' 'c'")]), 2.0);
    assert_eq!(h.f("altstr_count", &[s("'a")]), 0.0);
    assert_eq!(h.f("altstr_count", &[s("'a' 'b")]), 1.0);
    eq(h.s("altstr_prepare", &[s("it's 'x'")]), "it\\'s \\'x\\'");
    eq(h.s("altstr_prepare", &[s("a\\b")]), "a\\b");
    eq(h.s("altstr_get", &[s("'one' 'two'"), f(1.0)]), "two");
    eq(h.s("altstr_get", &[s("'one' 'two'"), f(0.0)]), "one");
    eq(h.s("altstr_get", &[s("'a\\'b' 'c'"), f(0.0)]), "a'b");
    eq(h.s("altstr_get", &[s("'a\\'b' 'c'"), f(1.0)]), "c");
    eq(h.s("altstr_get", &[s("'a' 'b'"), f(5.0)]), "");
    eq(h.s("altstr_get", &[s("'a' 'b'"), f(-1.0)]), "");
    eq(h.s("altstr_get", &[s("'a' 'unterminated"), f(1.0)]), "unterminated");
    eq(h.s("altstr_set", &[s("'one' 'two'"), f(1.0), s("x")]), "'one' 'x'");
    eq(h.s("altstr_set", &[s("'one' 'two'"), f(0.0), s("")]), "'' 'two'");
    eq(h.s("altstr_set", &[s("'a\\'b' 'c'"), f(1.0), s("z")]), "'a\\'b' 'z'");
    eq(h.s("altstr_set", &[s("'a\\'b' 'c'"), f(0.0), s("q")]), "'q' 'c'");
    eq(h.s("altstr_set", &[s("'a'"), f(3.0), s("x")]), "'a'x");
    eq(h.s("altstr_set", &[s("'a' 'b"), f(1.0), s("x")]), "'a' 'x");
}

#[test]
fn menu_uses_its_own_numbers() {
    let mut h = menu();
    assert_eq!(h.f("strlen", &[s("abc")]), 3.0);
    eq(h.s("strcat", &[s("a"), s("b")]), "ab");
    eq(h.s("ftos", &[f(0.5)]), "0.5");
    assert_eq!(h.f("stof", &[s("2.5")]), 2.5);
    eq(h.s("substring", &[s("hello"), f(1.0), f(2.0)]), "el");
    let _ = i(0);
}
