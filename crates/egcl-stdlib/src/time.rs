// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! CL universal-time / decoded-time arithmetic.
//!
//! Universal time is the number of seconds since 1900-01-01 00:00:00 GMT (CLHS
//! 25.1.4). The date math uses Howard Hinnant's `days_from_civil` /
//! `civil_from_days` algorithms, which are exact for the full proleptic
//! Gregorian range — no table lookups, no leap-year special-casing at the call
//! site.
//!
//! Time zones are hours *west* of GMT (so GMT itself is 0, and e.g. EST is 5).
//! The checked Lisp encoder accepts rational offsets at whole-second precision;
//! the integer arithmetic helpers and decoder use whole-hour offsets.
//! When a caller omits the zone we
//! default to GMT (0) rather than the host's local zone: it keeps results
//! deterministic and host-independent, and the libraries that care about local
//! zones (e.g. local-time) carry their own zone machinery and always pass an
//! explicit offset.

use egcl_rt::bignum::{
    BigInt, BigRat, as_bigrat, big_add, big_divmod, big_mul, bigint_from_val, bigrat_cmp,
};
use egcl_rt::error::EgclError;
use egcl_rt::value::EgclVal;
use std::cmp::Ordering;

/// Days between the CL universal-time epoch (1900-01-01) and the Unix epoch
/// (1970-01-01). 1900 is not a leap year (divisible by 100, not 400).
const DAYS_1900_TO_1970: i64 = 25567;
const SECS_PER_DAY: i64 = 86_400;

/// Days from 1970-01-01 to the given proleptic-Gregorian date (may be negative).
/// `days_from_civil` per Hinnant; `m` in 1..=12, `d` in 1..=31.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 }; // Mar=0..Feb=11
    let doy = (153 * mp + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`]: (year, month, day) for a day count since
/// 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + i64::from(m <= 2), m, d)
}

/// `encode-universal-time`: local decoded time (in zone `time_zone`, hours west
/// of GMT; `None` = GMT) → universal time. This low-level helper expects validated
/// fields and a full year; Lisp callers use [`encode_universal_time_checked`].
pub fn encode_universal_time(
    second: i64,
    minute: i64,
    hour: i64,
    date: i64,
    month: i64,
    year: i64,
    time_zone: Option<i64>,
) -> i64 {
    let days = days_from_civil(year, month, date) + DAYS_1900_TO_1970;
    let local = days * SECS_PER_DAY + hour * 3600 + minute * 60 + second;
    local + time_zone.unwrap_or(0) * 3600
}

/// Validate Lisp decoded-time arguments and encode without floating-point
/// coercion or host-integer overflow. Omitted/NIL zones retain the GMT default.
pub fn encode_universal_time_checked(args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    if !(6..=7).contains(&args.len()) {
        return Err(EgclError::ProgramError(
            "ENCODE-UNIVERSAL-TIME requires six or seven arguments".into(),
        ));
    }
    let bounded = |value: EgclVal, min: i64, max: i64| {
        if value.is_fixnum() && (min..=max).contains(&value.as_fixnum()) {
            Ok(value.as_fixnum())
        } else {
            Err(EgclError::TypeError {
                datum: value,
                expected: format!("(integer {min} {max})"),
            })
        }
    };
    let second = bounded(args[0], 0, 59)?;
    let minute = bounded(args[1], 0, 59)?;
    let hour = bounded(args[2], 0, 23)?;
    let date = bounded(args[3], 1, 31)?;
    let month = bounded(args[4], 1, 12)?;
    let mut year = bigint_from_val(args[5])
        .filter(|year| year.sign >= 0)
        .ok_or_else(|| EgclError::TypeError {
            datum: args[5],
            expected: "(integer 0 *)".into(),
        })?;
    if year.mag.len() <= 1 && year.mag.first().copied().unwrap_or(0) < 100 {
        let current = decode_universal_time(get_universal_time(), Some(0)).5;
        let base = current - 50;
        let short = year.mag.first().copied().unwrap_or(0) as i64;
        year = BigInt::from_i64(base + (short - base).rem_euclid(100));
    }
    let zone_seconds = match args.get(6).copied().filter(|zone| !zone.is_nil()) {
        None => 0,
        Some(value) => {
            let zone = as_bigrat(value)
                .filter(|zone| {
                    bigrat_cmp(zone, &BigRat::from_i64(-24)) != Ordering::Less
                        && bigrat_cmp(zone, &BigRat::from_i64(24)) != Ordering::Greater
                })
                .ok_or_else(|| EgclError::TypeError {
                    datum: value,
                    expected: "(or null (rational -24 24))".into(),
                })?;
            let (seconds, remainder) =
                big_divmod(&big_mul(&zone.num, &BigInt::from_i64(3600)), &zone.den);
            if !remainder.is_zero() {
                return Err(EgclError::ProgramError(
                    "time zone must represent a whole number of seconds".into(),
                ));
            }
            i64::from(seconds.sign) * seconds.mag.first().copied().unwrap_or(0) as i64
        }
    };
    // Split off complete Gregorian cycles so even bignum years use the same
    // bounded civil-date arithmetic as ordinary years.
    let (era, year_in_era) = big_divmod(&year, &BigInt::from_i64(400));
    let year_in_era = year_in_era.mag.first().copied().unwrap_or(0) as i64;
    let within_era = days_from_civil(year_in_era, month, date) + DAYS_1900_TO_1970;
    let days = big_add(
        &big_mul(&era, &BigInt::from_i64(146097)),
        &BigInt::from_i64(within_era),
    );
    let universal = big_add(
        &big_mul(&days, &BigInt::from_i64(SECS_PER_DAY)),
        &BigInt::from_i64(hour * 3600 + minute * 60 + second + zone_seconds),
    );
    // All argument reads are complete before constructing a heap result.
    let value = universal.to_val();
    if universal.sign < 0 {
        return Err(EgclError::TypeError {
            datum: value,
            expected: "unsigned-byte".into(),
        });
    }
    Ok(value)
}

/// Decoded universal time. Returns
/// `(second, minute, hour, date, month, year, day_of_week, daylight_p, zone)`
/// where `day_of_week` is 0=Monday … 6=Sunday (CLHS), `daylight_p` is always
/// false (no DST modelling), and `zone` is the zone actually used.
pub fn decode_universal_time(
    universal_time: i64,
    time_zone: Option<i64>,
) -> (i64, i64, i64, i64, i64, i64, i64, bool, i64) {
    let zone = time_zone.unwrap_or(0);
    let local = universal_time - zone * 3600;
    // Floor-divide so a time before the epoch still lands on the right day.
    let days = local.div_euclid(SECS_PER_DAY);
    let rem = local.rem_euclid(SECS_PER_DAY);
    let (year, month, date) = civil_from_days(days - DAYS_1900_TO_1970);
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    // 1900-01-01 was a Monday, which is CL day-of-week 0, and `days` counts from
    // there, so a plain modulo gives the weekday.
    let day_of_week = days.rem_euclid(7);
    (
        second,
        minute,
        hour,
        date,
        month,
        year,
        day_of_week,
        false,
        zone,
    )
}

/// `get-universal-time`: the current time as a CL universal time. `None` only if
/// the host clock is before the Unix epoch.
pub fn get_universal_time() -> i64 {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    unix + DAYS_1900_TO_1970 * SECS_PER_DAY
}

/// High-resolution wall-clock time as `(CL universal seconds, nanoseconds
/// within the second)`. Sampling both values from one `SystemTime` instant
/// prevents a second-boundary mismatch in consumers that need fractional
/// absolute deadlines.
pub fn get_precise_time() -> (i64, i64) {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (
        unix.as_secs() as i64 + DAYS_1900_TO_1970 * SECS_PER_DAY,
        unix.subsec_nanos() as i64,
    )
}

/// `get-internal-real-time`: milliseconds of wall-clock time since an
/// arbitrary fixed origin (process start), matching egcl's
/// `INTERNAL-TIME-UNITS-PER-SECOND` of 1000 (CLHS 25.1.4.3 leaves the origin
/// unspecified; a process-start origin keeps values small fixnums). Monotonic,
/// so benchmark deltas are immune to wall-clock adjustments (bliss-jpd0).
pub fn get_internal_real_time() -> i64 {
    origin().elapsed().as_millis() as i64
}

fn origin() -> std::time::Instant {
    use std::sync::OnceLock;
    static ORIGIN: OnceLock<std::time::Instant> = OnceLock::new();
    *ORIGIN.get_or_init(std::time::Instant::now)
}

/// Monotonic time in NANOSECONDS since process start (same origin as
/// `get-internal-real-time`). The ms-resolution internal-time clock is too
/// coarse for per-call deterministic profiling (bliss-xgr5); this exposes the
/// full resolution of the underlying `Instant`. Wraps after ~292 years.
pub fn get_real_time_nanos() -> i64 {
    origin().elapsed().as_nanos() as i64
}

/// `get-internal-run-time`: user and kernel CPU time consumed by this process,
/// including its worker threads, in milliseconds. Sleeping is not CPU work.
/// The runtime supplies the platform clock without adding a libc dependency
/// to the static x86-64 build. Report clock failure rather than substituting a
/// wall clock or a constant that would silently corrupt timing results.
pub fn get_internal_run_time() -> Result<i64, egcl_rt::error::EgclError> {
    egcl_rt::syscall::process_cpu_time_ns()
        .map(|nanoseconds| (nanoseconds / 1_000_000) as i64)
        .map_err(|error| {
            egcl_rt::error::EgclError::Internal(format!(
                "cannot read process CPU time: OS error {error}"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_encoding_rejects_invalid_field_types_and_ranges() {
        let good = [0, 0, 4, 13, 5, 2026, 0].map(EgclVal::from_fixnum);
        for (index, min, max) in [(0, 0, 59), (1, 0, 59), (2, 0, 23), (3, 1, 31), (4, 1, 12)] {
            for bad in [
                EgclVal::from_fixnum(min - 1),
                EgclVal::from_fixnum(max + 1),
                EgclVal::from_single_float(1.0),
                egcl_rt::value::NIL,
            ] {
                let mut args = good;
                args[index] = bad;
                match encode_universal_time_checked(&args) {
                    Err(EgclError::TypeError { datum, expected }) => {
                        assert_eq!(datum, bad);
                        assert_eq!(expected, format!("(integer {min} {max})"));
                    }
                    result => panic!("field {index}: expected a type error, got {result:?}"),
                }
            }
        }
        assert!(matches!(
            encode_universal_time_checked(&good[..5]),
            Err(EgclError::ProgramError(_))
        ));
        let mut extra = good.to_vec();
        extra.push(EgclVal::from_fixnum(0));
        assert!(matches!(
            encode_universal_time_checked(&extra),
            Err(EgclError::ProgramError(_))
        ));
    }

    #[test]
    fn checked_encoding_defaults_to_gmt_and_rejects_subsecond_zones() {
        let mut args = [0, 0, 4, 13, 5, 2026, 0].map(EgclVal::from_fixnum);
        let expected = EgclVal::from_fixnum(3987633600);
        assert_eq!(encode_universal_time_checked(&args[..6]).unwrap(), expected);
        args[6] = egcl_rt::value::NIL;
        assert_eq!(encode_universal_time_checked(&args).unwrap(), expected);
        args[6] = BigRat::new(BigInt::from_i64(1), BigInt::from_i64(7)).to_val();
        assert!(matches!(
            encode_universal_time_checked(&args),
            Err(EgclError::ProgramError(_))
        ));
    }

    #[test]
    fn checked_encoding_agrees_across_gregorian_cycles() {
        for year in [1900, 1901, 1999, 2000, 2024, 2100, 2400, 9999, 10000] {
            for month in 1..=12 {
                for date in [1, 28, 31] {
                    for zone in [-24, 0, 24] {
                        let args = [59, 59, 23, date, month, year, zone].map(EgclVal::from_fixnum);
                        let expected =
                            encode_universal_time(59, 59, 23, date, month, year, Some(zone));
                        if expected >= 0 {
                            assert_eq!(
                                encode_universal_time_checked(&args).unwrap(),
                                EgclVal::from_fixnum(expected)
                            );
                        } else {
                            assert!(matches!(
                                encode_universal_time_checked(&args),
                                Err(EgclError::TypeError { .. })
                            ));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn epoch_is_zero() {
        assert_eq!(encode_universal_time(0, 0, 0, 1, 1, 1900, Some(0)), 0);
    }

    #[test]
    fn unix_epoch_constant() {
        // The value local-time embeds as #.(encode-universal-time 0 0 0 1 1 1970 0).
        assert_eq!(
            encode_universal_time(0, 0, 0, 1, 1, 1970, Some(0)),
            DAYS_1900_TO_1970 * SECS_PER_DAY
        );
        assert_eq!(
            encode_universal_time(0, 0, 0, 1, 1, 1970, Some(0)),
            2_208_988_800
        );
    }

    #[test]
    fn precise_time_is_normalized_and_matches_universal_time() {
        let (seconds, nanoseconds) = get_precise_time();
        assert!((0..1_000_000_000).contains(&nanoseconds));
        assert!((seconds - get_universal_time()).abs() <= 1);
    }

    #[test]
    fn round_trips() {
        // A known instant: 2020-01-01 12:30:45 GMT.
        let ut = encode_universal_time(45, 30, 12, 1, 1, 2020, Some(0));
        let (s, mi, h, d, mo, y, _dow, dst, z) = decode_universal_time(ut, Some(0));
        assert_eq!(
            (s, mi, h, d, mo, y, dst, z),
            (45, 30, 12, 1, 1, 2020, false, 0)
        );
    }

    #[test]
    fn weekday_monday_is_zero() {
        // 1900-01-01 is a Monday → day-of-week 0.
        let ut = encode_universal_time(0, 0, 0, 1, 1, 1900, Some(0));
        assert_eq!(decode_universal_time(ut, Some(0)).6, 0);
        // 2020-01-01 was a Wednesday → 2.
        let ut = encode_universal_time(0, 0, 0, 1, 1, 2020, Some(0));
        assert_eq!(decode_universal_time(ut, Some(0)).6, 2);
    }

    #[test]
    fn time_zone_shifts_gmt() {
        // Noon in zone 5 (EST) is 17:00 GMT.
        let ut = encode_universal_time(0, 0, 12, 1, 1, 2020, Some(5));
        assert_eq!(decode_universal_time(ut, Some(0)).2, 17);
        // And decoding back in zone 5 recovers noon.
        assert_eq!(decode_universal_time(ut, Some(5)).2, 12);
    }
}
