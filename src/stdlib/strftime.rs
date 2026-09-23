// SPDX-License-Identifier: MIT OR Apache-2.0

//! strftime (docs/spec/strings.md).
//!
//! A C-locale `strftime` in the style of glibc: every C99/POSIX conversion plus the common GNU
//! ones (`%k %l %P %s`), the `_ - 0 ^ #` flags, field widths and the `E`/`O` modifiers.
//! Unknown conversions are copied to the output literally.

use crate::builtins::Builtins;
use crate::error::VmError;
use crate::host::{CalendarTime, Host};
use crate::stdlib::format::Sink;
use crate::stdlib::util::args_concat;
use crate::vm::Vm;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("strftime", strftime::<H>);
}

/// Output limit (FTE's 8192-byte buffer).
const STRFTIME_MAX: usize = 8191;

const DAYS: [&str; 7] =
    ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian; `month` 1–12).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year.saturating_sub(1) } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month.saturating_add(9)).rem_euclid(12);
    let doy =
        (153i64.saturating_mul(mp).saturating_add(2) / 5).saturating_add(day.saturating_sub(1));
    let doe = yoe
        .saturating_mul(365)
        .saturating_add(yoe / 4)
        .saturating_sub(yoe / 100)
        .saturating_add(doy);
    era.saturating_mul(146_097).saturating_add(doe).saturating_sub(719_468)
}

/// ISO 8601 week-based days: the day of the year relative to the Monday starting week 1 (may
/// be negative).
fn iso_week_days(yday: i64, wday: i64) -> i64 {
    // Monday is day 1; week 1 contains the year's first Thursday (day 4).
    const BIG_MULTIPLE_OF_7: i64 = 378;
    let shift = yday.saturating_sub(wday).saturating_add(4 + BIG_MULTIPLE_OF_7).rem_euclid(7);
    yday.saturating_sub(shift).saturating_add(3)
}

/// The ISO 8601 week-based year and week number (1–53).
fn iso_week(t: &CalendarTime) -> (i64, i64) {
    let mut year = i64::from(t.year);
    let yday = i64::from(t.yearday);
    let wday = i64::from(t.weekday);
    let mut days = iso_week_days(yday, wday);
    if days < 0 {
        year = year.saturating_sub(1);
        let len = if is_leap(year) { 366 } else { 365 };
        days = iso_week_days(yday.saturating_add(len), wday);
    } else {
        let len = if is_leap(year) { 366 } else { 365 };
        let next = iso_week_days(yday.saturating_sub(len), wday);
        if next >= 0 {
            year = year.saturating_add(1);
            days = next;
        }
    }
    (year, (days / 7).saturating_add(1))
}

/// The padding flag of a conversion.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pad {
    Default,
    Space,
    None,
    Zero,
}

#[derive(Clone, Copy)]
struct Directive {
    pad: Pad,
    upper: bool,
    swap_case: bool,
    width: usize,
}

impl Directive {
    /// Emits text, padded to the width (with zeros only for the `0` flag).
    fn text(&self, out: &mut Sink, s: &[u8], upper: bool, lower: bool) {
        let w = if self.pad == Pad::None { 0 } else { self.width };
        out.fill(if self.pad == Pad::Zero { b'0' } else { b' ' }, w.saturating_sub(s.len()));
        for &c in s {
            out.push(if upper {
                c.to_ascii_uppercase()
            } else if lower {
                c.to_ascii_lowercase()
            } else {
                c
            });
        }
    }

    /// Emits a number with at least `digits` digits (zero padded by default, space padded for
    /// `space_default` conversions or the `_` flag, unpadded with `-`); a field width raises the
    /// digit count.
    fn number(&self, out: &mut Sink, digits: usize, value: i64, space_default: bool, sign: bool) {
        let pad = match self.pad {
            Pad::Default if space_default => Pad::Space,
            p => p,
        };
        let body = value.unsigned_abs().to_string().into_bytes();
        let sign_char = if value < 0 {
            Some(b'-')
        } else if sign {
            Some(b'+')
        } else {
            None
        };
        let len = body.len().saturating_add(usize::from(sign_char.is_some()));
        let digits = digits.max(self.width);
        let padding = if pad == Pad::None { 0 } else { digits.saturating_sub(len) };
        if pad == Pad::Space {
            out.fill(b' ', padding);
        }
        if let Some(s) = sign_char {
            out.push(s);
        }
        if pad != Pad::Space {
            out.fill(b'0', padding);
        }
        out.extend(&body);
    }
}

/// Which modifiers a conversion accepts (POSIX): `E` for `c C x X y Y`, `O` for the numeric
/// ones (and month names, like glibc).
fn modifier_ok(conv: u8, modifier: Option<u8>) -> bool {
    match modifier {
        None => true,
        Some(b'E') => b"cCxXyY".contains(&conv),
        Some(_) => b"bBhdeHImMSuUVwWy".contains(&conv),
    }
}

/// Formats `fmt` for time `t` into `out` (C locale).
fn format_into(out: &mut Sink, fmt: &[u8], t: &CalendarTime) {
    let year = i64::from(t.year);
    let hour = i64::from(t.hour);
    let wday = i64::from(t.weekday);
    let yday = i64::from(t.yearday);
    let mut i = 0usize;
    while let Some(&c) = fmt.get(i) {
        if c != b'%' {
            out.push(c);
            i = i.saturating_add(1);
            continue;
        }
        let start = i;
        let mut d = Directive { pad: Pad::Default, upper: false, swap_case: false, width: 0 };
        i = i.saturating_add(1);
        while let Some(&f) = fmt.get(i) {
            match f {
                b'_' => d.pad = Pad::Space,
                b'-' => d.pad = Pad::None,
                b'0' | b'+' => d.pad = Pad::Zero,
                b'^' => d.upper = true,
                b'#' => d.swap_case = true,
                _ => break,
            }
            i = i.saturating_add(1);
        }
        while let Some(&w) = fmt.get(i).filter(|w| w.is_ascii_digit()) {
            d.width = d.width.saturating_mul(10).saturating_add(usize::from(w.wrapping_sub(b'0')));
            i = i.saturating_add(1);
        }
        let modifier = match fmt.get(i) {
            Some(&m @ (b'E' | b'O')) => {
                i = i.saturating_add(1);
                Some(m)
            }
            _ => None,
        };
        let conv = fmt.get(i).copied().unwrap_or(0);
        let day_name = DAYS.get(t.weekday as usize).copied().unwrap_or("?");
        let month_name = MONTHS.get(t.month as usize).copied().unwrap_or("?");
        let hour12 = if hour % 12 == 0 { 12 } else { hour % 12 };
        let sub: Option<&[u8]> = match conv {
            b'c' => Some(b"%a %b %e %H:%M:%S %Y"),
            b'D' | b'x' => Some(b"%m/%d/%y"),
            b'F' => Some(b"%Y-%m-%d"),
            b'r' => Some(b"%I:%M:%S %p"),
            b'R' => Some(b"%H:%M"),
            b'T' | b'X' => Some(b"%H:%M:%S"),
            _ => None,
        };
        let known = b"aAbBcCdDeFgGhHIjklmMnpPrRsStTuUVwWxXyYzZ%".contains(&conv);
        if !known || !modifier_ok(conv, modifier) {
            // Unknown conversions (and a trailing '%') are copied literally.
            let end = if conv == 0 { i } else { i.saturating_add(1) };
            out.extend(fmt.get(start..end).unwrap_or_default());
            i = end;
            continue;
        }
        i = i.saturating_add(1);
        if let Some(sub) = sub {
            let mut inner = Sink::new(out.room());
            format_into(&mut inner, sub, t);
            d.text(out, &inner.buf, d.upper, false);
            continue;
        }
        let names_upper = d.upper || d.swap_case;
        match conv {
            b'a' => d.text(out, day_name.get(..3).unwrap_or("").as_bytes(), names_upper, false),
            b'A' => d.text(out, day_name.as_bytes(), names_upper, false),
            b'b' | b'h' => {
                d.text(out, month_name.get(..3).unwrap_or("").as_bytes(), names_upper, false)
            }
            b'B' => d.text(out, month_name.as_bytes(), names_upper, false),
            b'p' | b'P' => {
                let ampm: &[u8] = if hour < 12 { b"AM" } else { b"PM" };
                let lower = conv == b'P' || d.swap_case;
                d.text(out, ampm, d.upper && !lower, lower);
            }
            b'Z' => {
                let zone = t.zone.unwrap_or("").as_bytes();
                d.text(out, zone, d.upper && !d.swap_case, d.swap_case);
            }
            b'n' => d.text(out, b"\n", false, false),
            b't' => d.text(out, b"\t", false, false),
            b'%' => d.text(out, b"%", false, false),
            b'C' => d.number(out, 2, year.div_euclid(100), false, false),
            b'd' => d.number(out, 2, i64::from(t.day), false, false),
            b'e' => d.number(out, 2, i64::from(t.day), true, false),
            b'g' => d.number(out, 2, iso_week(t).0.rem_euclid(100), false, false),
            b'G' => d.number(out, 1, iso_week(t).0, false, false),
            b'H' => d.number(out, 2, hour, false, false),
            b'I' => d.number(out, 2, hour12, false, false),
            b'j' => d.number(out, 3, yday.saturating_add(1), false, false),
            b'k' => d.number(out, 2, hour, true, false),
            b'l' => d.number(out, 2, hour12, true, false),
            b'm' => d.number(out, 2, i64::from(t.month).saturating_add(1), false, false),
            b'M' => d.number(out, 2, i64::from(t.minute), false, false),
            b's' => {
                let days =
                    days_from_civil(year, i64::from(t.month).saturating_add(1), i64::from(t.day));
                let secs = days
                    .saturating_mul(86_400)
                    .saturating_add(hour.saturating_mul(3600))
                    .saturating_add(i64::from(t.minute).saturating_mul(60))
                    .saturating_add(i64::from(t.second))
                    .saturating_sub(i64::from(t.utc_offset));
                d.number(out, 1, secs, false, false);
            }
            b'S' => d.number(out, 2, i64::from(t.second), false, false),
            b'u' => d.number(
                out,
                1,
                wday.saturating_add(6).rem_euclid(7).saturating_add(1),
                false,
                false,
            ),
            b'U' => d.number(out, 2, yday.saturating_sub(wday).saturating_add(7) / 7, false, false),
            b'V' => d.number(out, 2, iso_week(t).1, false, false),
            b'w' => d.number(out, 1, wday, false, false),
            b'W' => {
                let monday_based = wday.saturating_add(6).rem_euclid(7);
                d.number(
                    out,
                    2,
                    yday.saturating_sub(monday_based).saturating_add(7) / 7,
                    false,
                    false,
                );
            }
            b'y' => d.number(out, 2, year.rem_euclid(100), false, false),
            b'Y' => d.number(out, 1, year, false, false),
            b'z' => {
                let off = i64::from(t.utc_offset) / 60;
                let hhmm = (off.abs() / 60).saturating_mul(100).saturating_add(off.abs() % 60);
                let v = if off < 0 { hhmm.saturating_neg() } else { hhmm };
                d.number(out, 5, v, false, off >= 0);
            }
            _ => {}
        }
    }
}

/// C's `strftime` in the C locale for time `t`, limited to 8191 bytes. A format of exactly
/// `%R` or `%F` is replaced by `%H:%M` / `%Y-%m-%d`, as FTE does.
pub(crate) fn format_time(fmt: &[u8], t: &CalendarTime) -> Vec<u8> {
    let fmt: &[u8] = match fmt {
        b"%R" => b"%H:%M",
        b"%F" => b"%Y-%m-%d",
        f => f,
    };
    let mut out = Sink::new(STRFTIME_MAX);
    format_into(&mut out, fmt, t);
    out.buf
}

/// `string strftime(float uselocaltime, string format...)`: the current time (local if the
/// first argument is non-zero, else UTC, from [`Host::calendar_time`]) formatted like C's
/// `strftime`; `""` if the host provides no time.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn strftime<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let local = vm.arg_f32(0) != 0.0;
    let fmt = args_concat(vm, 1);
    let out = host.calendar_time(local).map(|t| format_time(&fmt, &t)).unwrap_or_default();
    vm.ret_str(&out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::stdlib::calendar_from_unix;

    fn f(fmt: &str, secs: i64) -> String {
        String::from_utf8(format_time(fmt.as_bytes(), &calendar_from_unix(secs))).unwrap()
    }

    #[test]
    fn conversions() {
        // 2024-02-29 12:34:56 UTC, a Thursday.
        let t = 1_709_210_096;
        assert_eq!(f("%Y-%m-%d %H:%M:%S", t), "2024-02-29 12:34:56");
        assert_eq!(f("%a %A %b %B %h", t), "Thu Thursday Feb February Feb");
        assert_eq!(f("%c", t), "Thu Feb 29 12:34:56 2024");
        assert_eq!(f("%D|%F|%R|%T|%r", t), "02/29/24|2024-02-29|12:34|12:34:56|12:34:56 PM");
        assert_eq!(f("%e|%j|%u|%w|%U|%W|%V|%G|%g", t), "29|060|4|4|08|09|09|2024|24");
        assert_eq!(f("%I %l %k %p %P", t), "12 12 12 PM pm");
        assert_eq!(f("%s %z %Z %%", t), "1709210096 +0000 UTC %");
        assert_eq!(f("%C %y %n%t", t), "20 24 \n\t");
        assert_eq!(f("%^a %#b %10Y %-d %_5m %05e", t), "THU FEB 0000002024 29     2 00029");
        assert_eq!(f("%Ey %Od %Ea %q %", t), "24 29 %Ea %q %");
        assert_eq!(f("%R", 0), "00:00");
    }

    #[test]
    fn iso_weeks() {
        // 2021-01-01 (Friday) is in week 53 of 2020; 2024-12-30 (Monday) in week 1 of 2025.
        assert_eq!(f("%G-W%V-%u", 1_609_459_200), "2020-W53-5");
        assert_eq!(f("%G-W%V-%u", 1_735_516_800), "2025-W01-1");
    }
}
