// SPDX-License-Identifier: MIT OR Apache-2.0

//! sprintf (docs/spec/strings.md).

use std::f32::consts::PI;

use qcvm::Arg;
use qcvm::vm::CharScheme;

use super::{eq, harness, warnings, with_charset};
use crate::support::harness::{Harness, f, i, s, v};

fn sp(h: &mut Harness, args: &[Arg<'_>]) -> Vec<u8> {
    h.s("sprintf", args)
}

/// A 64-bit value spread over two argument slots (for the `q` modifier).
fn raw64(bits: u64) -> Arg<'static> {
    Arg::Raw([bits as u32, (bits >> 32) as u32, 0])
}

#[test]
fn spec_examples() {
    let mut h = harness();
    let cases: Vec<(Vec<Arg<'_>>, &str)> = vec![
        (vec![s("%d"), f(3.7)], "3"),
        (vec![s("%d"), f(-3.7)], "-3"),
        (vec![s("%d"), f(1e10)], "10000000000"),
        (vec![s("%d"), i(5)], "0"),
        (vec![s("%ld"), i(5)], "5"),
        (vec![s("%i"), i(5)], "5"),
        (vec![s("%i"), f(5.0)], "1084227584"),
        (vec![s("%hi"), f(5.0)], "5"),
        (vec![s("%lf"), i(5)], "5.000000"),
        (vec![s("%+d"), f(42.0)], "+42"),
        (vec![s("% d"), f(42.0)], " 42"),
        (vec![s("%-6d|"), f(42.0)], "42    |"),
        (vec![s("%06.2f"), f(-1.5)], "-01.50"),
        (vec![s("%5.2f|"), f(PI)], " 3.14|"),
        (vec![s("%x"), f(255.0)], "ff"),
        (vec![s("%X"), f(255.9)], "FF"),
        (vec![s("%#x"), f(255.0)], "0xff"),
        (vec![s("%08x"), f(255.0)], "000000ff"),
        (vec![s("%x"), i(255)], "0"),
        (vec![s("%lx"), i(-1)], "ffffffff"),
        (vec![s("%p"), i(255)], "000000ff"),
        (vec![s("%o"), f(8.0)], "10"),
        (vec![s("%e"), f(12345.0)], "1.234500e+04"),
        (vec![s("%g"), f(0.1)], "0.1"),
        (vec![s("%g"), f(1e20)], "1e+20"),
        (vec![s("%f"), f(0.1)], "0.100000"),
        (vec![s("%.10f"), f(0.1)], "0.1000000015"),
        (vec![s("%c"), f(65.0)], "A"),
        (vec![s("%c"), f(321.0)], "A"),
        (vec![s("%3c"), f(65.0)], "  A"),
        (vec![s("%c"), f(0.0)], ""),
        (vec![s("%5s|"), s("ab")], "   ab|"),
        (vec![s("%-5s|"), s("ab")], "ab   |"),
        (vec![s("%.1s"), s("ab")], "a"),
        (vec![s("%S"), s("a b")], "\"a b\""),
        (vec![s("%S"), s("say \"hi\"")], "\\\"say \\\"hi\\\"\""),
        (vec![s("%S"), s("x\ny")], "\\\"x\\ny\""),
        (vec![s("%v"), v(1.0, 2.0, 3.0)], "1 2 3"),
        (vec![s("%.3v"), v(1.23456, 0.0, -0.5)], "1.23 0 -0.5"),
        (vec![s("%5v"), v(1.0, 2.0, 3.0)], "    1     2     3"),
        (vec![s("%#v"), v(1.0, 2.0, 3.0)], "1.00000 2.00000 3.00000"),
        (vec![s("%2$s-%1$s"), s("a"), s("b")], "b-a"),
        (vec![s("%2$s %s"), s("a"), s("b")], "b a"),
        (vec![s("%*d"), f(5.0), f(42.0)], "   42"),
        (vec![s("%*d|"), f(-5.0), f(42.0)], "42   |"),
        (vec![s("%.*f"), f(2.0), f(PI)], "3.14"),
        (vec![s("%*d"), i(5), f(42.0)], "42"),
        (vec![s("%d %s")], "0 "),
        (vec![s("%v")], "0 0 0"),
        (vec![s("%%")], "%"),
        (vec![s("%05s|"), s("ab")], "   ab|"),
    ];
    for (args, want) in cases {
        let got = sp(&mut h, &args);
        assert!(
            got == want.as_bytes(),
            "sprintf{args:?}: got {:?}, want {want:?}",
            got.escape_ascii().to_string()
        );
    }
}

#[test]
fn format_errors_stop_the_output() {
    let mut h = harness();
    for (fmt, want) in [
        ("ab%kcd", "ab"),
        ("x%", "x"),
        ("A%.f", "A"),
        ("%5-d", ""),
        ("1%a2", "1"),
        ("1%n2", "1"),
        ("1%I2", "1"),
        ("q%*1d", "q"),
        ("q%.*1f", "q"),
        ("ok %d then %y", "ok 1 then "),
    ] {
        let before = warnings(&h);
        eq(sp(&mut h, &[s(fmt), f(1.0)]), want);
        assert!(warnings(&h) > before, "{fmt}: no warning");
    }
    // A valid format warns about nothing.
    let before = warnings(&h);
    eq(sp(&mut h, &[s("%d%%"), f(1.0)]), "1%");
    assert_eq!(warnings(&h), before);
}

#[test]
fn flags_and_widths() {
    let mut h = harness();
    let cases: Vec<(Vec<Arg<'_>>, &str)> = vec![
        (vec![s("%05d"), f(42.0)], "00042"),
        (vec![s("%-05d|"), f(42.0)], "42   |"),
        (vec![s("%0-5d|"), f(42.0)], "42   |"),
        (vec![s("%00d"), f(7.0)], "7"),
        (vec![s("%+.3d"), f(7.0)], "+007"),
        (vec![s("%08.3d"), f(-7.0)], "    -007"),
        (vec![s("% +d"), f(1.0)], "+1"),
        (vec![s("%.0d|"), f(0.0)], "|"),
        (vec![s("%#o"), f(8.0)], "010"),
        (vec![s("%#.3o"), f(8.0)], "010"),
        (vec![s("%#o"), f(0.0)], "0"),
        (vec![s("%#X"), f(255.0)], "0XFF"),
        (vec![s("%#x"), f(0.0)], "0"),
        (vec![s("%+u"), f(5.0)], "5"),
        (vec![s("%u"), f(-1.0)], "18446744073709551615"),
        (vec![s("%x"), f(-1.0)], "ffffffffffffffff"),
        (vec![s("%lu"), i(-1)], "4294967295"),
        (vec![s("%d"), f(f32::NAN)], "-9223372036854775808"),
        (vec![s("%d"), f(1e19)], "-9223372036854775808"),
        (vec![s("%d"), f(-0.0)], "0"),
        (vec![s("%P"), i(0xAB)], "000000AB"),
        (vec![s("%4p|"), i(0xAB)], "00ab|"),
        (vec![s("%-10p|"), i(0xAB)], "ab        |"),
        (vec![s("%e"), f(0.0)], "0.000000e+00"),
        (vec![s("%E"), f(-1.5e-7)], "-1.500000E-07"),
        (vec![s("%.0e"), f(12345.0)], "1e+04"),
        (vec![s("%#.0e"), f(12345.0)], "1.e+04"),
        (vec![s("%#.0f"), f(3.0)], "3."),
        (vec![s("%.0f"), f(2.5)], "2"),
        (vec![s("%.0f"), f(3.5)], "4"),
        (vec![s("%.2f"), f(1.005)], "1.00"),
        (vec![s("%g"), f(100_000.0)], "100000"),
        (vec![s("%g"), f(1e6)], "1e+06"),
        (vec![s("%G"), f(1e-5)], "1E-05"),
        (vec![s("%g"), f(0.0001)], "0.0001"),
        (vec![s("%.3g"), f(1234.5)], "1.23e+03"),
        (vec![s("%#g"), f(0.5)], "0.500000"),
        (vec![s("%g"), f(-0.0)], "-0"),
        (vec![s("%010.3f"), f(PI)], "000003.142"),
        (vec![s("%+.1e"), f(1.0)], "+1.0e+00"),
        (vec![s("%f"), f(f32::INFINITY)], "inf"),
        (vec![s("%F"), f(f32::NEG_INFINITY)], "-INF"),
        (vec![s("%5.1f|"), f(f32::NAN)], "  nan|"),
        (vec![s("%08.1f"), f(f32::NEG_INFINITY)], "    -inf"),
        (vec![s("%+f"), f(f32::INFINITY)], "+inf"),
        (vec![s("%E"), f(f32::from_bits(0xFFC0_0000))], "-NAN"),
        (vec![s("%5s|"), s("abcdefg")], "abcdefg|"),
        (vec![s("%.3s"), s("abcdef")], "abc"),
        (vec![s("%.0s|"), s("abc")], "|"),
        (vec![s("%-3c|"), f(65.0)], "A  |"),
        (vec![s("%3c|"), f(0.0)], "  |"),
        (vec![s("%-3c|"), f(0.0)], "|"),
        (vec![s("%c"), f(-191.0)], "A"),
        (vec![s("%lc"), i(66)], "B"),
        (vec![s("%#5s|"), s("ab")], "   ab|"),
        (vec![s("%-8S|"), s("ab")], "\"ab\"    |"),
        (vec![s("%.3S"), s("abc")], "\"ab"),
        (vec![s("%s"), f(0.0)], ""),
        (vec![s("%s"), s("")], ""),
        (vec![s("%V"), v(1e-5, 1e6, 0.5)], "1E-05 1E+06 0.5"),
        (vec![s("%lv"), Arg::Raw([1, (-2i32) as u32, 3])], "1 -2 3"),
        (vec![s("%-4v|"), v(1.0, 2.0, 3.0)], "1    2    3   |"),
        (vec![s("%s%s%s"), s("a"), s("b")], "ab"),
    ];
    for (args, want) in cases {
        let got = sp(&mut h, &args);
        assert!(
            got == want.as_bytes(),
            "sprintf{args:?}: got {:?}, want {want:?}",
            got.escape_ascii().to_string()
        );
    }
}

#[test]
fn positional_and_star_arguments() {
    let mut h = harness();
    eq(sp(&mut h, &[s("%3$d"), f(1.0), f(2.0)]), "0");
    eq(sp(&mut h, &[s("%1$d %1$d %d"), f(1.0), f(2.0)]), "1 1 1");
    eq(sp(&mut h, &[s("%1$*2$d|"), f(42.0), f(5.0)]), "   42|");
    eq(sp(&mut h, &[s("%.*2$f|"), f(PI), f(1.0)]), "3.1|");
    eq(sp(&mut h, &[s("%*.*f|"), f(7.0), f(2.0), f(PI)]), "   3.14|");
    eq(sp(&mut h, &[s("%.*f"), f(-1.0), f(PI)]), "3.141593");
    eq(sp(&mut h, &[s("%0$d|%d"), f(7.0)]), "0|7");
    // `*` always reads a float, even for an int argument (whose bits are a tiny float).
    eq(sp(&mut h, &[s("%.*f"), i(2), f(1.5)]), "2");
    // A huge `*` width reads as INT_MIN, which is no width at all.
    eq(sp(&mut h, &[s("%*d|"), f(-3e9), f(1.0)]), "1|");
}

#[test]
fn length_modifiers() {
    let mut h = harness();
    eq(sp(&mut h, &[s("%hhd"), f(5.9)]), "5");
    eq(sp(&mut h, &[s("%lld"), i(-5)]), "-5");
    eq(sp(&mut h, &[s("%jd %zd %td"), f(1.0), f(2.0), f(3.0)]), "1 2 3");
    eq(sp(&mut h, &[s("%Lf"), i(5)]), "5.000000");
    eq(sp(&mut h, &[s("%li"), f(5.0)]), "1084227584");
    eq(sp(&mut h, &[s("%hx"), i(255)]), "0");
    // `q`: 64-bit arguments over two slots (double, or int64 with `l`).
    eq(sp(&mut h, &[s("%qd"), raw64(1e15f64.to_bits())]), "1000000000000000");
    eq(sp(&mut h, &[s("%lqd"), raw64((-5i64) as u64)]), "-5");
    eq(sp(&mut h, &[s("%qi"), raw64((-5i64) as u64)]), "-5");
    eq(sp(&mut h, &[s("%lqx"), raw64(0x1234_5678_9ABC_DEF0)]), "123456789abcdef0");
    eq(sp(&mut h, &[s("%qf"), raw64(0.1f64.to_bits())]), "0.100000");
    eq(sp(&mut h, &[s("%.17qg"), raw64(0.1f64.to_bits())]), "0.10000000000000001");
    eq(sp(&mut h, &[s("%lqf"), raw64((-3i64) as u64)]), "-3.000000");
    eq(sp(&mut h, &[s("%qu"), raw64((-1.0f64).to_bits())]), "18446744073709551615");
}

#[test]
fn output_is_capped_at_65535_bytes() {
    let mut h = harness();
    let out = sp(&mut h, &[s("%70000d"), f(1.0)]);
    assert_eq!(out.len(), 65_535);
    assert!(out.iter().all(|&c| c == b' '));
    let out = sp(&mut h, &[s("%-70000d"), f(1.0)]);
    assert_eq!(out.len(), 65_535);
    assert_eq!(out[0], b'1');
    let out = sp(&mut h, &[s("%.100000f"), f(0.5)]);
    assert_eq!(out.len(), 65_535);
    assert!(out.starts_with(b"0.5000"));
    let big = "x".repeat(70_000);
    let out = sp(&mut h, &[s(&big)]);
    assert_eq!(out.len(), 65_535);
    // Directives after the cap still consume their arguments; nothing more is written.
    let fmt = format!("{}%d%s", "y".repeat(65_535));
    assert_eq!(sp(&mut h, &[s(&fmt), f(1.0), s("z")]).len(), 65_535);
    // A huge precision on a tiny value prints its exact digits, then zeros.
    let out = sp(&mut h, &[s("%.1100f"), f(f32::from_bits(1))]);
    assert_eq!(out.len(), 1102);
    assert!(out.starts_with(b"0.000000000000000000000000000000000000000000001401298464"));
}

#[test]
fn percent_c_and_s_count_characters_with_utf8() {
    // utf8_enable 1 + UTF-8 scheme: %c encodes code points, %s/%c widths count characters.
    let mut h = with_charset(true, CharScheme::Utf8);
    eq(sp(&mut h, &[s("%c"), f(9786.0)]), "\u{263a}");
    eq(sp(&mut h, &[s("%3c|"), f(233.0)]), "  \u{e9}|");
    eq(sp(&mut h, &[s("%c"), f(0.0)]), b"\xC0\x80");
    eq(sp(&mut h, &[s("%#c"), f(9786.0)]), ":");
    eq(sp(&mut h, &[s("%5s|"), s("h\u{e9}")]), "   h\u{e9}|");
    eq(sp(&mut h, &[s("%-4s|"), s("\u{e9}")]), "\u{e9}   |");
    eq(sp(&mut h, &[s("%.1s|"), s("\u{e9}a")]), "\u{e9}|");
    eq(sp(&mut h, &[s("%#5s|"), s("h\u{e9}")]), "  h\u{e9}|");
    eq(sp(&mut h, &[s("%4S|"), s("\u{e9}")]), " \"\u{e9}\"|");
    // utf8_enable 1 + Quake scheme: one byte per character; unencodable characters are `?`.
    let mut h = with_charset(true, CharScheme::Quake);
    eq(sp(&mut h, &[s("%c%c"), f(200.0), f(65.0)]), "?A");
    eq(sp(&mut h, &[s("%c"), f(57_537.0)]), b"\xC1");
    eq(sp(&mut h, &[s("%3s|"), s("h\u{e9}")]), "h\u{e9}|");
    // utf8_enable 1 + ISO-8859-1: bytes.
    let mut h = with_charset(true, CharScheme::Iso8859_1);
    eq(sp(&mut h, &[s("%c"), f(233.0)]), b"\xE9");
    eq(sp(&mut h, &[s("%c|"), f(0.0)]), "|");
    // utf8_enable 0: %c is C's %c whatever the scheme.
    let mut h = with_charset(false, CharScheme::Utf8);
    eq(sp(&mut h, &[s("%c"), f(233.0)]), b"\xE9");
    eq(sp(&mut h, &[s("%3s|"), s("\u{e9}")]), " \u{e9}|");
}
