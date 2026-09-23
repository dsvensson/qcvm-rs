// SPDX-License-Identifier: MIT OR Apache-2.0

//! gettime and calltimeofday.

use qcvm::{CalendarTime, Numbering, Op, StrRef, VmConfig};

use crate::support::asm::{Asm, ty};
use crate::support::harness::{Harness, f, i};

const PARTS: [&str; 6] = ["t_sec", "t_min", "t_hour", "t_day", "t_mon", "t_year"];

/// `timeofday(sec, min, hour, day, mon, year, text)` storing its arguments in globals.
fn setup(asm: &mut Asm) {
    Harness::named(&["gettimef", "gettimed"])(asm);
    let globals: Vec<u32> = PARTS.iter().map(|n| asm.global(n, ty::FLOAT, &[])).collect();
    let text = asm.global("t_text", ty::STRING, &[]);
    let func = asm.function("timeofday", &[1, 1, 1, 1, 1, 1, 1], 0);
    for (k, g) in (0..).zip(globals) {
        asm.emit(Op::StoreF, func.local(k), g, 0);
    }
    asm.emit(Op::StoreS, func.local(6), text, 0);
    asm.emit(Op::Done, 0, 0, 0);
}

#[test]
fn gettime_types() {
    let mut h = Harness::with(Numbering::Csqc, VmConfig::default(), setup);
    h.vm.set_realtime(12.345_678);
    assert_eq!(h.f("gettime", &[]), 12.345_678);
    assert_eq!(h.f("gettime", &[f(0.0)]), 12.345_678);
    assert_eq!(h.f("gettime", &[f(7.0)]), 12.345_678, "unknown types read the frame time");
    assert_eq!(h.f("gettime", &[f(1.0)]), 12.345, "whole milliseconds");
    // Type 5 is the host's simulation time, if it has one.
    assert_eq!(h.f("gettime", &[f(5.0)]), 12.345_678);
    h.host.sim_time = Some(99.5);
    assert_eq!(h.f("gettime", &[f(5.0)]), 99.5);
    // FTE's CSQC name, and the double-precision variant (whose type is an int).
    assert_eq!(h.f("gettimef", &[f(5.0)]), 99.5);
    let r = h.call("gettimed", &[i(0)]).unwrap();
    let d = f64::from_bits(u64::from(r.0[0]) | (u64::from(r.0[1]) << 32));
    assert_eq!(d, 12.345_678);
    let r = h.call("gettimed", &[]).unwrap();
    assert_eq!(f64::from_bits(u64::from(r.0[0]) | (u64::from(r.0[1]) << 32)), 12.345_678);
}

#[test]
fn gettime_defaults_to_the_time_since_creation() {
    let mut h = Harness::new();
    let t = h.f("gettime", &[]);
    assert!((0.0..60.0).contains(&t), "{t}");
}

#[test]
fn calltimeofday_calls_timeofday_with_local_time() {
    let mut h = Harness::with(Numbering::Csqc, VmConfig::default(), setup);
    h.host.now = Some(CalendarTime {
        year: 2026,
        month: 8,
        day: 3,
        hour: 14,
        minute: 5,
        second: 9,
        weekday: 4,
        yearday: 245,
        utc_offset: 7200,
        zone: Some("CEST"),
    });
    h.call("calltimeofday", &[]).unwrap();
    let parts: Vec<f32> = PARTS.iter().map(|n| h.vm.get(h.vm.global::<f32>(n).unwrap())).collect();
    // The month counts from 0.
    assert_eq!(parts, [9.0, 5.0, 14.0, 3.0, 8.0, 2026.0]);
    let text = h.vm.get(h.vm.global::<StrRef>("t_text").unwrap());
    assert_eq!(h.vm.str(text), b"Thu Sep 03, 14:05:09 2026");

    // Without a timeofday function nothing happens.
    let mut h = Harness::new();
    h.call("calltimeofday", &[]).unwrap();
}
