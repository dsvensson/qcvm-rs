// SPDX-License-Identifier: MIT OR Apache-2.0

//! The standard builtins: everything FTE provides that does not need an engine.
//!
//! [`Builtins::standard`] registers them all under FTE's numbers for the chosen VM kind. The
//! implementations are ordinary [`BuiltinFn`](crate::BuiltinFn)s generic over the host, so a host
//! can wrap or replace any of them. Behaviour follows `docs/spec/strings.md` and
//! `docs/spec/builtins.md`, with the fixes listed in `docs/spec/deviations.md`.

pub mod charset;
pub mod convert;
pub mod digest;
pub mod entity;
pub mod format;
pub mod hash;
pub mod hostcalls;
pub mod introspect;
pub mod json;
pub mod math;
pub mod memory;
pub mod progs;
pub mod reflect;
pub mod strbuf;
pub mod strftime;
pub mod string;
pub mod time;
pub mod tokenize;
pub(crate) mod util;
pub mod vector;

use crate::builtins::{Builtins, Numbering};
use crate::host::{CalendarTime, Host};

/// Per-VM state of the standard builtins.
#[derive(Clone, Debug, Default)]
pub(crate) struct StdState {
    pub(crate) tokens: tokenize::Tokens,
    pub(crate) hash: hash::Tables,
    pub(crate) bufs: strbuf::Buffers,
}

/// Registers every standard builtin.
pub(crate) fn register_all<H: Host>(b: &mut Builtins<H>) {
    charset::register(b);
    convert::register(b);
    digest::register(b);
    entity::register(b);
    format::register(b);
    hash::register(b);
    hostcalls::register(b);
    introspect::register(b);
    json::register(b);
    math::register(b);
    memory::register(b);
    progs::register(b);
    reflect::register(b);
    strbuf::register(b);
    strftime::register(b);
    string::register(b);
    time::register(b);
    tokenize::register(b);
    vector::register(b);
}

impl<H: Host> Builtins<H> {
    /// A registry with every standard builtin, numbered for `numbering`.
    #[must_use]
    pub fn standard(numbering: Numbering) -> Self {
        let mut b = Self::empty(numbering);
        register_all(&mut b);
        b
    }
}

/// FTE extensions whose builtins the standard library implements completely (what the default
/// [`Host::check_extension`] reports).
pub const EXTENSIONS: &[&str] = &[];

/// Whether `name` is one of [`EXTENSIONS`].
#[must_use]
pub fn has_extension(name: &[u8]) -> bool {
    EXTENSIONS.iter().any(|e| e.as_bytes() == name)
}

/// The current UTC calendar time from the system clock.
#[must_use]
pub fn utc_now() -> Option<CalendarTime> {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(calendar_from_unix(i64::try_from(secs).ok()?))
}

/// Converts seconds since the Unix epoch to a UTC calendar time.
#[must_use]
#[allow(clippy::arithmetic_side_effects)] // bounded civil-date arithmetic
pub fn calendar_from_unix(secs: i64) -> CalendarTime {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let is_leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    const START: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let month0 = usize::try_from(month - 1).unwrap_or(0);
    let yearday =
        START.get(month0).copied().unwrap_or(0) + day - 1 + i64::from(is_leap && month > 2);
    CalendarTime {
        year: i32::try_from(year).unwrap_or(i32::MAX),
        month: u32::try_from(month - 1).unwrap_or(0),
        day: u32::try_from(day).unwrap_or(1),
        hour: u32::try_from(rem / 3600).unwrap_or(0),
        minute: u32::try_from(rem % 3600 / 60).unwrap_or(0),
        second: u32::try_from(rem % 60).unwrap_or(0),
        weekday: u32::try_from(days.saturating_add(4).rem_euclid(7)).unwrap_or(0),
        yearday: u32::try_from(yearday).unwrap_or(0),
        utc_offset: 0,
        zone: Some("UTC"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        let t = calendar_from_unix(0);
        assert_eq!((t.year, t.month, t.day, t.weekday, t.yearday), (1970, 0, 1, 4, 0));
        // 2024-02-29 12:34:56 UTC, a Thursday.
        let t = calendar_from_unix(1_709_210_096);
        assert_eq!((t.year, t.month, t.day, t.hour, t.minute, t.second), (2024, 1, 29, 12, 34, 56));
        assert_eq!((t.weekday, t.yearday), (4, 59));
        // 2026-12-31 is day 364 of a non-leap year.
        let t = calendar_from_unix(1_798_675_200);
        assert_eq!((t.year, t.month, t.day, t.yearday), (2026, 11, 31, 364));
    }
}
