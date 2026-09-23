// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scalar maths and random numbers.

use qcvm::{ErrorKind, Numbering, VmConfig};

use super::warnings;
use crate::support::harness::{Harness, f};

fn h() -> Harness {
    Harness::with(Numbering::Csqc, VmConfig::default(), Harness::named(&["anglesub"]))
}

#[test]
fn transcendental_functions_use_double_precision() {
    let mut h = h();
    for (name, x, want) in [
        ("sin", 1.0f32, libm::sin(1.0) as f32),
        ("cos", 1.0, libm::cos(1.0) as f32),
        ("tan", 0.5, libm::tan(0.5) as f32),
        ("asin", 0.5, libm::asin(0.5) as f32),
        ("acos", 0.5, libm::acos(0.5) as f32),
        ("atan", 1.0, libm::atan(1.0) as f32),
        ("sqrt", 2.0, libm::sqrt(2.0) as f32),
    ] {
        assert_eq!(h.f(name, &[f(x)]).to_bits(), want.to_bits(), "{name}({x})");
    }
    assert_eq!(h.f("sin", &[f(0.0)]), 0.0);
    assert_eq!(h.f("sqrt", &[f(16.0)]), 4.0);
    assert!(h.f("sqrt", &[f(-1.0)]).is_nan());
    assert!(h.f("asin", &[f(2.0)]).is_nan());
    assert_eq!(h.f("atan2", &[f(1.0), f(1.0)]), std::f32::consts::FRAC_PI_4);
    assert_eq!(h.f("atan2", &[f(1.0), f(0.0)]), std::f32::consts::FRAC_PI_2);
    assert_eq!(h.f("pow", &[f(2.0), f(10.0)]), 1024.0);
    assert_eq!(h.f("pow", &[f(9.0), f(0.5)]), 3.0);
}

#[test]
fn rounding() {
    let mut h = h();
    assert_eq!(h.f("floor", &[f(-1.5)]), -2.0);
    assert_eq!(h.f("floor", &[f(1.5)]), 1.0);
    assert_eq!(h.f("ceil", &[f(-1.5)]), -1.0);
    assert_eq!(h.f("ceil", &[f(1.2)]), 2.0);
    assert_eq!(h.f("fabs", &[f(-3.25)]), 3.25);
    assert_eq!(h.f("fabs", &[f(-0.0)]).to_bits(), 0);
    // rint rounds half away from zero.
    for (x, want) in [(2.5, 3.0), (-2.5, -3.0), (2.4, 2.0), (-2.6, -3.0), (0.0, 0.0)] {
        assert_eq!(h.f("rint", &[f(x)]), want, "rint({x})");
    }
    // x86 overflow: out-of-range conversions give INT_MIN.
    assert_eq!(h.f("rint", &[f(3e9)]), -2_147_483_648.0);
    assert_eq!(h.f("rint", &[f(f32::NAN)]), -2_147_483_648.0);
}

#[test]
fn bound_min_max() {
    let mut h = h();
    assert_eq!(h.f("bound", &[f(0.0), f(5.0), f(10.0)]), 5.0);
    assert_eq!(h.f("bound", &[f(0.0), f(-1.0), f(10.0)]), 0.0);
    assert_eq!(h.f("bound", &[f(0.0), f(11.0), f(10.0)]), 10.0);
    // The maximum wins over the minimum, and a NaN value passes through.
    assert_eq!(h.f("bound", &[f(10.0), f(5.0), f(0.0)]), 0.0);
    assert!(h.f("bound", &[f(0.0), f(f32::NAN), f(10.0)]).is_nan());

    assert_eq!(h.f("min", &[f(3.0), f(2.0)]), 2.0);
    assert_eq!(h.f("max", &[f(3.0), f(2.0)]), 3.0);
    assert_eq!(h.f("min", &[f(3.0), f(2.0), f(7.0), f(-1.0), f(4.0)]), -1.0);
    assert_eq!(h.f("max", &[f(3.0), f(2.0), f(7.0), f(-1.0), f(4.0)]), 7.0);
    let eight: Vec<_> = (1..=8).map(|i| f(i as f32)).collect();
    assert_eq!(h.f("max", &eight), 8.0);
    assert_eq!(h.f("min", &eight), 1.0);
    // NaNs and ties follow the comparisons: `a < b ? a : b` for two arguments, and only strictly
    // smaller values replace the first for more.
    assert_eq!(h.f("min", &[f(f32::NAN), f(1.0)]), 1.0);
    assert!(h.f("min", &[f(1.0), f(f32::NAN)]).is_nan());
    assert!(h.f("min", &[f(f32::NAN), f(1.0), f(2.0)]).is_nan());
    assert_eq!(h.f("min", &[f(-0.0), f(0.0)]).to_bits(), 0);
    assert_eq!(h.f("min", &[f(-0.0), f(0.0), f(1.0)]).to_bits(), (-0.0f32).to_bits());
    assert_eq!(h.f("max", &[f(f32::NAN), f(1.0)]), 1.0);
    // Fewer than two arguments is a builtin error.
    let err = h.call("min", &[f(1.0)]).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Builtin(m) if m.contains("at least 2")));
    let err = h.call("max", &[]).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Builtin(_)));
}

#[test]
fn modulo_takes_the_sign_of_the_dividend() {
    let mut h = h();
    assert_eq!(h.f("mod", &[f(7.0), f(3.0)]), 1.0);
    assert_eq!(h.f("mod", &[f(-7.0), f(3.0)]), -1.0);
    assert_eq!(h.f("mod", &[f(7.0), f(-3.0)]), 1.0);
    assert_eq!(h.f("mod", &[f(5.5), f(2.0)]), 1.5);
    assert!(warnings(&h).is_empty());
    assert_eq!(h.f("mod", &[f(1.0), f(0.0)]), 0.0);
    assert_eq!(warnings(&h), ["mod by zero"]);

    // SSQC's name for it.
    let mut ssqc = Harness::with(Numbering::Ssqc, VmConfig::ssqc(), |_| {});
    assert_eq!(ssqc.f("modulo", &[f(-7.0), f(3.0)]), -1.0);
}

#[test]
fn bitshift_shifts_integers() {
    let mut h = h();
    assert_eq!(h.f("bitshift", &[f(1.0), f(4.0)]), 16.0);
    assert_eq!(h.f("bitshift", &[f(256.0), f(-4.0)]), 16.0);
    assert_eq!(h.f("bitshift", &[f(-16.0), f(-2.0)]), -4.0, "right shifts are arithmetic");
    assert_eq!(h.f("bitshift", &[f(3.9), f(1.9)]), 6.0, "operands are truncated");
    assert_eq!(h.f("bitshift", &[f(1.0), f(31.0)]), -2_147_483_648.0);
    // Counts of 32 or more are clamped (everything shifted out), not masked like x86.
    assert_eq!(h.f("bitshift", &[f(1.0), f(32.0)]), 0.0);
    assert_eq!(h.f("bitshift", &[f(-1.0), f(-40.0)]), -1.0);
    assert_eq!(h.f("bitshift", &[f(5.0), f(-40.0)]), 0.0);
}

#[test]
fn logarithms() {
    let mut h = h();
    assert_eq!(h.f("log", &[f(1.0)]), 0.0);
    assert_eq!(h.f("log", &[f(1.0), f(2.0)]), 0.0);
    assert_eq!(h.f("log", &[f(8.0), f(2.0)]), 3.0);
    assert_eq!(h.f("log", &[f(1000.0), f(10.0)]), 3.0);
    assert_eq!(h.f("logarithm", &[f(8.0), f(2.0)]), 3.0);
    assert_eq!(h.f("log", &[f(0.0)]), f32::NEG_INFINITY);
}

#[test]
fn anglemod_wraps_into_0_360() {
    let mut h = h();
    for (x, want) in [
        (370.0f32, 10.0f32),
        (-10.0, 350.0),
        (360.0, 0.0),
        (720.0, 0.0),
        (-720.0, 0.0),
        (359.5, 359.5),
        (-0.25, 359.75),
        (1000.25, 280.25),
    ] {
        let r = h.f("anglemod", &[f(x)]);
        assert_eq!(r.to_bits(), want.to_bits(), "anglemod({x}) = {r}");
    }
    assert_eq!(h.f("anglemod", &[f(-0.0)]).to_bits(), (-0.0f32).to_bits());
    assert!(h.f("anglemod", &[f(f32::NAN)]).is_nan());
    // FTE loops forever on huge values; the remainder is exact.
    let r = h.f("anglemod", &[f(1e10)]);
    assert!((0.0..360.0).contains(&r));
    assert_eq!(r, 1e10f32 % 360.0);
}

#[test]
fn anglesub_wraps_into_plus_minus_180() {
    let mut h = h();
    for (a, b, want) in [
        (10.0, 350.0, 20.0),
        (350.0, 10.0, -20.0),
        (180.0, 0.0, 180.0),
        (0.0, 180.0, -180.0),
        (540.0, 0.0, 180.0),
        (-540.0, 0.0, -180.0),
        (90.0, 0.0, 90.0),
        (-360.0, 0.0, 0.0),
    ] {
        let r = h.f("anglesub", &[f(a), f(b)]);
        assert_eq!(r.to_bits(), f32::to_bits(want), "anglesub({a}, {b}) = {r}");
    }
}

#[test]
fn random_is_strictly_inside_the_range_and_seeded() {
    let mut h = h();
    for _ in 0..2000 {
        let r = h.f("random", &[]);
        assert!(r > 0.0 && r < 1.0, "{r}");
        // (k + 0.5) / 32768 for a 15-bit k.
        let k = r * 32768.0 - 0.5;
        assert_eq!(k.fract(), 0.0);
        assert!((0.0..=32767.0).contains(&k));
        let r = h.f("random", &[f(10.0)]);
        assert!(r > 0.0 && r < 10.0);
        let r = h.f("random", &[f(5.0), f(6.0)]);
        assert!(r > 5.0 && r < 6.0);
    }
    // A reversed range still interpolates.
    let r = h.f("random", &[f(6.0), f(5.0)]);
    assert!(r > 5.0 && r < 6.0);

    let draws = |seed: u64| {
        let config = VmConfig { seed, ..VmConfig::default() };
        let mut h = Harness::with(Numbering::Csqc, config, |_| {});
        (0..16).map(|_| h.f("random", &[]).to_bits()).collect::<Vec<_>>()
    };
    assert_eq!(draws(1), draws(1));
    assert_ne!(draws(1), draws(2));
}

#[test]
fn randomvec_is_inside_the_unit_sphere() {
    let mut h = h();
    for _ in 0..1000 {
        let [x, y, z] = h.v("randomvec", &[]);
        assert!(x * x + y * y + z * z < 1.0);
        for c in [x, y, z] {
            // k / 32767 * 2 - 1 for a 15-bit k.
            let k = ((f64::from(c) + 1.0) * 32767.0 / 2.0).round();
            assert_eq!(((k * (2.0 / 32767.0)) - 1.0) as f32, c);
        }
    }
}
