// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scalar maths and random numbers (docs/spec/builtins.md).
//!
//! Like FTE's C code, transcendental functions work in double precision on the float argument
//! and round the result to float. They go through `libm`, so results are identical on every
//! platform.

use crate::builtins::Builtins;
use crate::error::VmError;
use crate::host::Host;
use crate::vm::Vm;
use crate::vm::num::f2i;

use super::util::arg_int;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("sin", sin::<H>);
    b.set("cos", cos::<H>);
    b.set("tan", tan::<H>);
    b.set("asin", asin::<H>);
    b.set("acos", acos::<H>);
    b.set("atan", atan::<H>);
    b.set("atan2", atan2::<H>);
    b.set("sqrt", sqrt::<H>);
    b.set("pow", pow::<H>);
    b.set("floor", floor::<H>);
    b.set("ceil", ceil::<H>);
    b.set("fabs", fabs::<H>);
    b.set("rint", rint::<H>);
    b.set("bound", bound::<H>);
    b.set("min", min::<H>);
    b.set("max", max::<H>);
    b.set("mod", modulo::<H>);
    b.alias("modulo", "mod");
    b.set("bitshift", bitshift::<H>);
    b.set("log", log::<H>);
    b.alias("logarithm", "log");
    b.set("anglemod", anglemod::<H>);
    b.set("anglesub", anglesub::<H>);
    b.set("random", random::<H>);
    b.set("randomvec", randomvec::<H>);
}

/// `(int)d` as x86 does it: truncation, with NaN and out-of-range values giving `i32::MIN`.
pub(crate) fn d2i(d: f64) -> i32 {
    if d.is_nan() || !(-2_147_483_648.0..2_147_483_648.0).contains(&d) {
        i32::MIN
    } else {
        d as i32
    }
}

/// Quake's `anglemod`: the angle quantised to 1/65536 of a turn, in `[0, 360)`.
pub(crate) fn anglemod16(a: f32) -> f32 {
    let steps = d2i(f64::from(a) * (65536.0 / 360.0)) & 0xFFFF;
    ((360.0 / 65536.0) * f64::from(steps)) as f32
}

macro_rules! double_fn {
    ($($(#[$doc:meta])* $name:ident => $f:path;)*) => {$(
        $(#[$doc])*
        pub fn $name<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
            let x = f64::from(vm.arg_f32(0));
            vm.ret_f32($f(x) as f32);
            Ok(())
        }
    )*};
}

double_fn! {
    /// `float sin(float radians)`.
    sin => libm::sin;
    /// `float cos(float radians)`.
    cos => libm::cos;
    /// `float tan(float radians)`.
    tan => libm::tan;
    /// `float asin(float)`, in radians.
    asin => libm::asin;
    /// `float acos(float)`, in radians.
    acos => libm::acos;
    /// `float atan(float)`, in radians.
    atan => libm::atan;
    /// `float sqrt(float)`.
    sqrt => libm::sqrt;
    /// `float floor(float)`.
    floor => libm::floor;
    /// `float ceil(float)`.
    ceil => libm::ceil;
    /// `float fabs(float)`.
    fabs => libm::fabs;
}

/// `float atan2(float y, float x)`, in radians.
pub fn atan2<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let (y, x) = (f64::from(vm.arg_f32(0)), f64::from(vm.arg_f32(1)));
    vm.ret_f32(libm::atan2(y, x) as f32);
    Ok(())
}

/// `float pow(float x, float y)`.
pub fn pow<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let (x, y) = (f64::from(vm.arg_f32(0)), f64::from(vm.arg_f32(1)));
    vm.ret_f32(libm::pow(x, y) as f32);
    Ok(())
}

/// `float rint(float)`: rounds half away from zero (`(int)(f ± 0.5)`, with x86 overflow
/// behaviour).
pub fn rint<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let f = f64::from(vm.arg_f32(0));
    let i = if f > 0.0 { d2i(f + 0.5) } else { d2i(f - 0.5) };
    vm.ret_f32(i as f32);
    Ok(())
}

/// `float bound(float min, float value, float max)`: `max` if `value > max`, else `min` if
/// `value < min`, else `value` (so `max` wins over `min`, and a NaN value is returned as is).
pub fn bound<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let (lo, v, hi) = (vm.arg_f32(0), vm.arg_f32(1), vm.arg_f32(2));
    let r = if v > hi {
        hi
    } else if v < lo {
        lo
    } else {
        v
    };
    vm.ret_f32(r);
    Ok(())
}

/// Shared by `min` and `max`: `better(candidate, current)` says whether to replace.
fn extreme<H: Host>(
    vm: &mut Vm<H>,
    name: &str,
    better: fn(f32, f32) -> bool,
) -> Result<(), VmError> {
    let argc = vm.argc().min(8);
    if argc < 2 {
        return Err(VmError::builtin(format!("{name}: must supply at least 2 floats")));
    }
    let (first, second) = (vm.arg_f32(0), vm.arg_f32(1));
    if argc == 2 {
        // FTE's `min`/`max` macros: `a < b ? a : b` (so a NaN or a tie picks `b`).
        vm.ret_f32(if better(first, second) { first } else { second });
        return Ok(());
    }
    let mut r = first;
    for i in 1..argc {
        let v = vm.arg_f32(i);
        if better(v, r) {
            r = v;
        }
    }
    vm.ret_f32(r);
    Ok(())
}

/// `float min(float a, float b, ...)`: the smallest argument (2–8 arguments). With two
/// arguments `a < b ? a : b`; with more, only strictly smaller values replace the first. NaNs
/// and ties follow from those comparisons.
pub fn min<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    extreme(vm, "min", |v, r| v < r)
}

/// `float max(float a, float b, ...)`: the largest argument (2–8 arguments).
pub fn max<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    extreme(vm, "max", |v, r| v > r)
}

/// `float mod(float a, float n)` (SSQC `modulo`): `a - n * (int)(a / n)`, taking the sign of
/// `a`. `n == 0` warns "mod by zero" and returns 0.
pub fn modulo<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let (a, n) = (vm.arg_f32(0), vm.arg_f32(1));
    if n == 0.0 {
        vm.warn("mod by zero");
        vm.ret_f32(0.0);
    } else {
        vm.ret_f32(a - n * f2i(a / n) as f32);
    }
    Ok(())
}

/// `float bitshift(float n, float count)`: `n << count` for a non-negative count, else an
/// arithmetic `n >> -count` (both truncated to integers). Counts of 32 or more shift everything
/// out.
pub fn bitshift<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = arg_int(vm, 0);
    let q = arg_int(vm, 1);
    let r = if q >= 0 {
        n.checked_shl(q.unsigned_abs()).unwrap_or(0)
    } else {
        n.checked_shr(q.unsigned_abs()).unwrap_or(if n < 0 { -1 } else { 0 })
    };
    vm.ret_f32(r as f32);
    Ok(())
}

/// `float log(float v, optional float base)` (also `logarithm`): the natural logarithm, or the
/// logarithm to `base`.
pub fn log<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let mut r = libm::log(f64::from(vm.arg_f32(0)));
    if vm.argc() > 1 {
        r /= libm::log(f64::from(vm.arg_f32(1)));
    }
    vm.ret_f32(r as f32);
    Ok(())
}

/// `float anglemod(float)`: the angle wrapped into `[0, 360)`. FTE subtracts or adds 360 in a
/// loop (which never ends for huge values); the exact remainder gives the same results.
pub fn anglemod<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = vm.arg_f32(0);
    let r = if v >= 360.0 {
        v % 360.0
    } else if v < 0.0 {
        let r = v % 360.0;
        if r < 0.0 { r + 360.0 } else { 0.0 }
    } else {
        v
    };
    vm.ret_f32(r);
    Ok(())
}

/// `float anglesub(float a, float b)`: `a - b` wrapped into `[-180, 180]` (±180 are kept).
pub fn anglesub<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = vm.arg_f32(0) - vm.arg_f32(1);
    let r = if v > 180.0 {
        let r = v % 360.0;
        if r > 180.0 { r - 360.0 } else { r }
    } else if v < -180.0 {
        let r = v % 360.0;
        if r < -180.0 {
            r + 360.0
        } else if r == 0.0 {
            0.0
        } else {
            r
        }
    } else {
        v
    };
    vm.ret_f32(r);
    Ok(())
}

/// `float random(optional float max)` / `float random(float min, float max)`: a value strictly
/// inside (0, 1) (`(k + 0.5) / 32768` for a 15-bit draw `k`), scaled to `(0, max)` or
/// `(min, max)`.
pub fn random<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let k = vm.core.rng.rand15();
    let r = (k as f32 + 0.5) / 32768.0;
    let v = match vm.argc() {
        0 => r,
        1 => r * vm.arg_f32(0),
        _ => {
            let (lo, hi) = (vm.arg_f32(0), vm.arg_f32(1));
            lo + r * (hi - lo)
        }
    };
    vm.ret_f32(v);
    Ok(())
}

/// `vector randomvec()`: a random vector strictly inside the unit sphere (components are
/// `k / 32767 * 2 - 1` for 15-bit draws, redrawn until the length is below 1).
pub fn randomvec<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    loop {
        let v: [f32; 3] = std::array::from_fn(|_| {
            (f64::from(vm.core.rng.rand15()) * (2.0 / 32767.0) - 1.0) as f32
        });
        let [x, y, z] = v;
        if x * x + y * y + z * z < 1.0 {
            vm.ret_vec(v);
            return Ok(());
        }
    }
}
