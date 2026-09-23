// SPDX-License-Identifier: MIT OR Apache-2.0

//! Number ↔ string conversions (docs/spec/strings.md).

use qcvm::{Arg, EntRef};

use super::{eq, harness};
use crate::support::harness::{f, i, s, v};

#[test]
fn ftos_prints_the_exact_float() {
    let mut h = harness();
    let cases: &[(f32, &str)] = &[
        (0.5, "0.5"),
        (0.1, "0.100000001"),
        (0.2, "0.2"),
        (0.3, "0.30000001"),
        (1.1, "1.10000002"),
        (1.0 / 3.0, "0.33333334"),
        (2.0 / 3.0, "0.66666669"),
        (0.01, "0.0099999998"),
        (0.001, "0.00100000005"),
        (0.00001, "0.0000099999997"),
        (std::f32::consts::PI, "3.14159274"),
        (10.1, "10.10000038"),
        (16.1, "16.1000004"),
        (99.99, "99.9899979"),
        (123.456, "123.4560013"),
        (1000.5, "1000.5"),
        (123_456.7, "123456.7031"),
        (1_048_576.1, "1048576.12"), // = 1048576.125
        (16_777_217_i32 as f32, "16777216"),
        (-2_147_483_648.0, "-2147483648"),
        (2_147_483_648.0, "2147483648"),
        (3e9, "3000000000"),
        (1e20, "100000002004087734272"),
        (f32::from_bits(1), "0.0000000000000000000000000000000000000000000014"),
        (f32::INFINITY, "1.#INF"),
        (f32::NEG_INFINITY, "-1.#INF"),
        (-42.0, "-42"),
        (0.0, "0"),
        (-0.0, "0"),
        (-0.5, "-0.5"),
        (1e-10, "0.000000000100000001"),
    ];
    for &(x, want) in cases {
        eq(h.s("ftos", &[f(x)]), want);
    }
    // x86's 0/0 is a NaN with the sign bit set; the sign is shown.
    eq(h.s("ftos", &[f(f32::from_bits(0xFFC0_0000))]), "-1.#NAN");
    eq(h.s("ftos", &[f(f32::NAN)]), "1.#NAN");
}

#[test]
fn vtos_uses_c_percent_f() {
    let mut h = harness();
    eq(h.s("vtos", &[v(1.0, 2.0, 3.0)]), "'1.000000 2.000000 3.000000'");
    eq(h.s("vtos", &[v(0.1, -0.0, 1e10)]), "'0.100000 -0.000000 10000000000.000000'");
    let nan = f32::from_bits(0xFFC0_0000);
    eq(h.s("vtos", &[v(f32::NAN, nan, f32::NEG_INFINITY)]), "'nan -nan -inf'");
    eq(h.s("vtos", &[v(0.0000005, 0.0000015, 2.5e-7)]), "'0.000000 0.000002 0.000000'");
}

#[test]
fn etos_itos_htos() {
    let mut h = harness();
    eq(h.s("etos", &[Arg::Ent(EntRef(5))]), "entity 5");
    eq(h.s("etos", &[Arg::Ent(EntRef(0))]), "entity 0");
    eq(h.s("itos", &[i(255)]), "255");
    eq(h.s("itos", &[i(-2_147_483_648)]), "-2147483648");
    eq(h.s("htos", &[i(255)]), "000000ff");
    eq(h.s("htos", &[i(-1)]), "ffffffff");
    eq(h.s("htos", &[i(0x1234_ABCD)]), "1234abcd");
}

#[test]
fn stoi_reads_the_base_from_the_prefix() {
    // FTE's documentation promises base 8/10/16 by prefix (FTE itself only reads decimal).
    let mut h = harness();
    let cases: &[(&str, i32)] = &[
        ("  -12abc", -12),
        ("0x1f", 31),
        ("0X1F", 31),
        ("-0x10", -16),
        ("010", 8),
        ("08", 0),
        ("0x", 0),
        ("abc", 0),
        ("", 0),
        ("+7", 7),
        ("\t\n 42", 42),
        ("4294967297", 1),
        ("2147483648", -2_147_483_648),
        ("99999999999999999999", -1),
    ];
    for &(text, want) in cases {
        assert_eq!(h.i("stoi", &[s(text)]), want, "stoi({text:?})");
    }
}

#[test]
fn stoh_reads_hex() {
    let mut h = harness();
    let cases: &[(&str, i32)] = &[
        ("1F", 31),
        ("0x1f", 31),
        ("10", 16),
        ("-1", -1),
        ("ffffffff", -1),
        ("zz", 0),
        (" 7g", 7),
    ];
    for &(text, want) in cases {
        assert_eq!(h.i("stoh", &[s(text)]), want, "stoh({text:?})");
    }
}

#[test]
fn ftoi_and_itof() {
    let mut h = harness();
    assert_eq!(h.i("ftoi", &[f(3.9)]), 3);
    assert_eq!(h.i("ftoi", &[f(-3.9)]), -3);
    assert_eq!(h.i("ftoi", &[f(f32::NAN)]), i32::MIN);
    assert_eq!(h.i("ftoi", &[f(3e9)]), i32::MIN);
    assert_eq!(h.f("itof", &[i(5)]), 5.0);
    assert_eq!(h.f("itof", &[i(-7)]), -7.0);
    assert_eq!(h.f("itof", &[i(16_777_217)]), 16_777_216.0);
    assert_eq!(h.f("itof", &[i(16_777_219)]), 16_777_220.0);
    // Bit fields: (value >> shift) & mask(count), count defaults to 24.
    assert_eq!(h.f("itof", &[i(0x1234_5678), f(8.0), f(8.0)]), 86.0);
    assert_eq!(h.f("itof", &[i(-1), f(0.0), f(32.0)]), 4_294_967_296.0);
    assert_eq!(h.f("itof", &[i(-1), f(4.0)]), 16_777_215.0);
    // Shift and count are taken modulo 32 like x86 shifts.
    assert_eq!(h.f("itof", &[i(0x30), f(36.0), f(33.0)]), 1.0);
    assert_eq!(h.f("itof", &[i(0x30), f(4.0), f(0.0)]), 0.0);
}

#[test]
fn ftou_and_utof() {
    let mut h = harness();
    assert_eq!(h.call("ftou", &[f(-1.0)]).unwrap().0[0], u32::MAX);
    assert_eq!(h.call("ftou", &[f(4_294_967_040.0)]).unwrap().0[0], 4_294_967_040);
    assert_eq!(h.call("ftou", &[f(3.7)]).unwrap().0[0], 3);
    assert_eq!(h.f("utof", &[i(-1)]), 4_294_967_296.0);
    assert_eq!(h.f("utof", &[i(0xF0), f(4.0), f(2.0)]), 3.0);
}

#[test]
fn stof_is_c_atof() {
    let mut h = harness();
    let cases: &[(&str, f32)] = &[
        ("  12.5xyz", 12.5),
        (".5", 0.5),
        ("1e3", 1000.0),
        ("1,5", 1.0),
        ("\"5\"", 0.0),
        ("1e39", f32::INFINITY),
        ("-1e39", f32::NEG_INFINITY),
        ("0x10", 16.0),
        ("0x1p4", 16.0),
        ("0x1.8", 1.5),
        ("inf", f32::INFINITY),
        ("-Infinity", f32::NEG_INFINITY),
        ("\x0b\x0c\r3", 3.0),
        ("abc", 0.0),
        ("", 0.0),
        ("1e-50", 0.0),
        ("0.1", 0.1),
        ("16777217", 16_777_216.0),
        ("3.4028235e38", f32::MAX),
    ];
    for &(text, want) in cases {
        assert_eq!(h.f("stof", &[s(text)]).to_bits(), want.to_bits(), "stof({text:?})");
    }
    assert!(h.f("stof", &[s("nan")]).is_nan());
    assert!(h.f("stof", &[s("NaN(123)")]).is_nan());
    assert!(h.f("stof", &[s("-nan")]).is_sign_negative());
    // Double rounding like C: the decimal is rounded to double (1 + 2^-24, a float tie), then
    // to float (ties to even), not directly to float (which would round up).
    assert_eq!(h.f("stof", &[s("1.00000005960464477539062500001")]), 1.0);
}

#[test]
fn stov_examples() {
    let mut h = harness();
    let cases: &[(&str, [f32; 3])] = &[
        ("'1 2 3'", [1.0, 2.0, 3.0]),
        ("1\t2\t3", [1.0, 2.0, 3.0]),
        ("'1 2'", [1.0, 2.0, 0.0]),
        ("1,2,3", [1.0, 0.0, 0.0]),
        ("1\n2\n3", [1.0, 0.0, 0.0]),
        ("1-2 3", [1.0, 3.0, 0.0]),
        ("0 1 2", [0.0, 1.0, 2.0]),
        (".0 1 2", [0.0, 0.0, 0.0]),
        (" '1 2 3'", [0.0, 0.0, 0.0]),
        ("(1 2 3)", [0.0, 0.0, 0.0]),
        ("1e3 2 3", [1000.0, 2.0, 3.0]),
        ("-0 5 6", [-0.0, 5.0, 6.0]),
        ("'1 2 3' 4", [1.0, 2.0, 3.0]),
        ("1 2 3 4", [1.0, 2.0, 3.0]),
        ("", [0.0, 0.0, 0.0]),
    ];
    for &(text, want) in cases {
        assert_eq!(h.v("stov", &[s(text)]), want, "stov({text:?})");
    }
    let n = h.v("stov", &[s("nan 1 2")]);
    assert!(n[0].is_nan() && n[1] == 1.0 && n[2] == 2.0);
    // The arguments are concatenated.
    assert_eq!(h.v("stov", &[s("'1 "), s("2 "), s("3'")]), [1.0, 2.0, 3.0]);
}
