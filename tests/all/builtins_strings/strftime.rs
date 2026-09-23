// SPDX-License-Identifier: MIT OR Apache-2.0

//! strftime (docs/spec/strings.md).

use qcvm::CalendarTime;

use super::{eq, harness};
use crate::support::harness::{Harness, f, s};

/// 2024-02-29 12:34:56 UTC, a Thursday.
fn fixed() -> Harness {
    let mut h = harness();
    h.host.now = Some(qcvm::stdlib::calendar_from_unix(1_709_210_096));
    h
}

#[test]
fn formats_the_host_time() {
    let mut h = fixed();
    eq(h.s("strftime", &[f(0.0), s("%Y-%m-%d %H:%M:%S")]), "2024-02-29 12:34:56");
    eq(h.s("strftime", &[f(1.0), s("%a %d %b %Y")]), "Thu 29 Feb 2024");
    eq(h.s("strftime", &[f(0.0), s("%A, %B %e")]), "Thursday, February 29");
    eq(h.s("strftime", &[f(0.0), s("%c")]), "Thu Feb 29 12:34:56 2024");
    eq(h.s("strftime", &[f(0.0), s("%x %X %D %T")]), "02/29/24 12:34:56 02/29/24 12:34:56");
    eq(h.s("strftime", &[f(0.0), s("%I:%M %p|%l|%k|%P")]), "12:34 PM|12|12|pm");
    eq(h.s("strftime", &[f(0.0), s("%j %U %W %V %G %g %u %w")]), "060 08 09 09 2024 24 4 4");
    eq(h.s("strftime", &[f(0.0), s("%s %z %Z %C %y %%")]), "1709210096 +0000 UTC 20 24 %");
    eq(h.s("strftime", &[f(0.0), s("%n%t")]), "\n\t");
    eq(h.s("strftime", &[f(0.0), s("")]), "");
}

#[test]
fn whole_format_shortcuts_and_concatenation() {
    let mut h = fixed();
    eq(h.s("strftime", &[f(0.0), s("%R")]), "12:34");
    eq(h.s("strftime", &[f(0.0), s("%F")]), "2024-02-29");
    eq(h.s("strftime", &[f(0.0), s("%R %F")]), "12:34 2024-02-29");
    eq(h.s("strftime", &[f(0.0), s("%Y"), s("-"), s("%m")]), "2024-02");
}

#[test]
fn flags_widths_and_unknown_specifiers() {
    let mut h = fixed();
    eq(h.s("strftime", &[f(0.0), s("%^a %#B %^p %#p")]), "THU FEBRUARY PM pm");
    eq(h.s("strftime", &[f(0.0), s("%-m/%-d %_m %05d %3e")]), "2/29  2 00029  29");
    eq(h.s("strftime", &[f(0.0), s("%10A|%-10A|%010A")]), "  Thursday|Thursday|00Thursday");
    eq(h.s("strftime", &[f(0.0), s("%EY %Oe %Ey %OS")]), "2024 29 24 56");
    eq(h.s("strftime", &[f(0.0), s("%Q %Ea %Oz %5")]), "%Q %Ea %Oz %5");
    eq(h.s("strftime", &[f(0.0), s("100%")]), "100%");
    eq(h.s("strftime", &[f(0.0), s("%30c|")]), "      Thu Feb 29 12:34:56 2024|");
    eq(h.s("strftime", &[f(0.0), s("%^c")]), "THU FEB 29 12:34:56 2024");
}

#[test]
fn edge_dates() {
    let mut h = harness();
    // 2021-01-01 (a Friday) belongs to ISO week 53 of 2020.
    h.host.now = Some(qcvm::stdlib::calendar_from_unix(1_609_459_200));
    eq(h.s("strftime", &[f(0.0), s("%G-W%V-%u %j %U %W")]), "2020-W53-5 001 00 00");
    // A local time with an offset and zone name from the host.
    h.host.now = Some(CalendarTime {
        year: 1999,
        month: 11,
        day: 31,
        hour: 23,
        minute: 59,
        second: 60,
        weekday: 5,
        yearday: 364,
        utc_offset: -(5 * 3600 + 30 * 60),
        zone: Some("XST"),
    });
    eq(
        h.s("strftime", &[f(1.0), s("%F %T %z %Z %#Z %I%p")]),
        "1999-12-31 23:59:60 -0530 XST xst 11PM",
    );
    // Seconds since the epoch account for the offset.
    eq(h.s("strftime", &[f(1.0), s("%s")]), "946704600");
}

#[test]
fn output_is_limited() {
    let mut h = fixed();
    assert_eq!(h.s("strftime", &[f(0.0), s(&"x".repeat(9000))]).len(), 8191);
    assert_eq!(h.s("strftime", &[f(0.0), s("%99999Y")]).len(), 8191);
}
