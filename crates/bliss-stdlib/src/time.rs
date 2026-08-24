//! CL universal-time / decoded-time arithmetic.
//!
//! Universal time is the number of seconds since 1900-01-01 00:00:00 GMT (CLHS
//! 25.1.4). The date math uses Howard Hinnant's `days_from_civil` /
//! `civil_from_days` algorithms, which are exact for the full proleptic
//! Gregorian range — no table lookups, no leap-year special-casing at the call
//! site.
//!
//! Time zones follow the CL convention: an integer number of hours *west* of
//! GMT (so GMT itself is 0, and e.g. EST is 5). When a caller omits the zone we
//! default to GMT (0) rather than the host's local zone: it keeps results
//! deterministic and host-independent, and the libraries that care about local
//! zones (e.g. local-time) carry their own zone machinery and always pass an
//! explicit offset.

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
/// of GMT; `None` = GMT) → universal time. Two-digit years are NOT adjusted here
/// (the interpreter applies the CLHS 1900/2000 rule before calling).
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

#[cfg(test)]
mod tests {
    use super::*;

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
