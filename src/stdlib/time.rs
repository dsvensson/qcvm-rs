// SPDX-License-Identifier: MIT OR Apache-2.0

//! gettime and calltimeofday (docs/spec/builtins.md).

use crate::builtins::Builtins;
use crate::error::VmError;
use crate::host::{CalendarTime, Host};
use crate::value::Arg;
use crate::vm::Vm;
use crate::vm::num::f2i;

use super::introspect::find_function_rt;
use super::util::opt_f32;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("gettime", gettime::<H>);
    b.alias("gettimef", "gettime");
    b.set("gettimed", gettimed::<H>);
    b.set("calltimeofday", calltimeofday::<H>);
}

/// The clock `gettime` type `t` reads: 1 the current time rounded down to milliseconds, 5 the
/// host's simulation time ([`Host::sim_time`]), anything else the frame's real time.
fn clock<H: Host>(vm: &Vm<H>, host: &mut H, t: i32) -> f64 {
    let now = vm.realtime();
    match t {
        1 => (now * 1000.0).floor() / 1000.0,
        5 => host.sim_time().unwrap_or(now),
        _ => now,
    }
}

/// `float gettime(optional float type)` (FTE's CSQC name is `gettimef`): 0 (or omitted, or
/// unknown) the real time of the current frame, 1 the current real time in whole milliseconds,
/// 5 the client simulation time. In seconds.
pub fn gettime<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let t = f2i(opt_f32(vm, 0, 0.0));
    let v = clock(vm, host, t);
    vm.ret_f32(v as f32);
    Ok(())
}

/// `__double gettimed(optional int type)`: like `gettime`, in double precision.
pub fn gettimed<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let t = if vm.argc() > 0 { vm.arg_i32(0) } else { 0 };
    let bits = clock(vm, host, t).to_bits().to_le_bytes();
    let word = |i: usize| {
        u32::from_le_bytes(
            bits.get(i..i.saturating_add(4)).and_then(|b| b.try_into().ok()).unwrap_or([0; 4]),
        )
    };
    vm.ret_raw([word(0), word(4), 0]);
    Ok(())
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `strftime(…, "%a %b %d, %H:%M:%S %Y")`.
fn date_text(t: &CalendarTime) -> String {
    let wd = WEEKDAYS.get(usize::try_from(t.weekday).unwrap_or(0)).copied().unwrap_or("?");
    let mon = MONTHS.get(usize::try_from(t.month).unwrap_or(0)).copied().unwrap_or("?");
    format!("{wd} {mon} {:02}, {:02}:{:02}:{:02} {}", t.day, t.hour, t.minute, t.second, t.year)
}

/// `void calltimeofday()`: if the progs defines `timeofday`, calls
/// `timeofday(second, minute, hour, day, month, year, text)` with the local time (month 0–11,
/// text like `"Wed Sep 23, 14:05:09 2026"`).
pub fn calltimeofday<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let Some(f) = find_function_rt(&vm.core, -2, b"timeofday") else { return Ok(()) };
    let Some(t) = host.calendar_time(true).or_else(crate::stdlib::utc_now) else {
        return Ok(());
    };
    let text = date_text(&t);
    let num = |v: u32| Arg::Float(v as f32);
    vm.call(
        host,
        f,
        &[
            num(t.second),
            num(t.minute),
            num(t.hour),
            num(t.day),
            num(t.month),
            Arg::Float(t.year as f32),
            Arg::Bytes(text.as_bytes()),
        ],
    )?;
    Ok(())
}
