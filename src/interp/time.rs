// Dates and times (RUNTIME.md section 4): an instant is an Int, the milliseconds
// since 1970-01-01T00:00:00Z, in UTC, between the first moment of the year 0000 and
// the last of the year 9999 (the proleptic Gregorian calendar, without leap seconds).
// Every function here is exact: integer arithmetic only, from Howard Hinnant's
// algorithms for days and civil dates.

/// Milliseconds in a day.
const DAY_MS: i64 = 86_400_000;

/// The first instant there is: 0000-01-01T00:00:00.000Z.
pub const MIN_MS: i64 = -62_167_219_200_000;

/// The last instant there is: 9999-12-31T23:59:59.999Z.
pub const MAX_MS: i64 = 253_402_300_799_999;

/// Days from 1970-01-01 to year `y`, month `m` (1-12), day `d`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The year, month (1-12) and day of the day `z` days after 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Why `ms` is not an instant there is, if it is not.
fn check(ms: i64) -> Result<(), String> {
    if (MIN_MS..=MAX_MS).contains(&ms) {
        Ok(())
    } else {
        Err(format!("{ms} is not a time between the years 0000 and 9999"))
    }
}

/// (year, month, day) of `ms`.
pub fn date_parts(ms: i64) -> Result<(i64, i64, i64), String> {
    check(ms)?;
    Ok(civil_from_days(ms.div_euclid(DAY_MS)))
}

/// (hour, minute, second, millisecond) of `ms`.
pub fn time_parts(ms: i64) -> Result<(i64, i64, i64, i64), String> {
    check(ms)?;
    let t = ms.rem_euclid(DAY_MS);
    Ok((t / 3_600_000, t / 60_000 % 60, t / 1000 % 60, t % 1000))
}

/// The day of the week of `ms`: 1 is Monday, 7 is Sunday (ISO 8601).
pub fn weekday(ms: i64) -> Result<i64, String> {
    check(ms)?;
    // 1970-01-01 was a Thursday.
    Ok((ms.div_euclid(DAY_MS) + 3).rem_euclid(7) + 1)
}

/// `ms` as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn format(ms: i64) -> Result<String, String> {
    let (y, mo, d) = date_parts(ms)?;
    let (h, mi, s, milli) = time_parts(ms)?;
    Ok(format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z"))
}

/// The instant of a date and a time of day, or why there is none.
pub fn make(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> Result<i64, String> {
    if !(0..=9999).contains(&y) || !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) {
        return Err(format!("{y}-{mo}-{d} is not a date between the years 0000 and 9999"));
    }
    if !(0..=23).contains(&h) || !(0..=59).contains(&mi) || !(0..=59).contains(&s) {
        return Err(format!("{h}:{mi}:{s} is not a time of day"));
    }
    Ok(days_from_civil(y, mo, d) * DAY_MS + h * 3_600_000 + mi * 60_000 + s * 1000)
}

/// An instant written as RFC 3339 does: `YYYY-MM-DDTHH:MM:SS`, then optionally `.`
/// and one to three digits of a second, then `Z` or an offset `+HH:MM` or `-HH:MM`.
/// Nothing else, so every text that parses names exactly one instant.
pub fn parse(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let digits = b.get(from..from + len)?;
        digits.iter().all(u8::is_ascii_digit).then(|| digits.iter().fold(0i64, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    if !(at(4, b'-') && at(7, b'-') && at(10, b'T') && at(13, b':') && at(16, b':')) {
        return None;
    }
    let (y, mo, d, h, mi, s) = (num(0, 4)?, num(5, 2)?, num(8, 2)?, num(11, 2)?, num(14, 2)?, num(17, 2)?);
    let mut i = 19;
    let mut milli = 0;
    if at(i, b'.') {
        let digits = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=3).contains(&digits) {
            return None;
        }
        milli = num(i + 1, digits)? * 10i64.pow(3 - digits as u32);
        i += 1 + digits;
    }
    let offset = match b.get(i) {
        Some(b'Z') if b.len() == i + 1 => 0,
        Some(sign @ (b'+' | b'-')) if b.len() == i + 6 && at(i + 3, b':') => {
            let (oh, om) = (num(i + 1, 2)?, num(i + 4, 2)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let o = (oh * 60 + om) * 60_000;
            if *sign == b'+' { o } else { -o }
        }
        _ => return None,
    };
    let ms = make(y, mo, d, h, mi, s).ok()? + milli - offset;
    check(ms).ok()?;
    Some(ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_range_ends_are_the_first_and_last_instants() {
        assert_eq!(make(0, 1, 1, 0, 0, 0), Ok(MIN_MS));
        assert_eq!(make(9999, 12, 31, 23, 59, 59).map(|t| t + 999), Ok(MAX_MS));
        assert_eq!(format(MIN_MS).unwrap(), "0000-01-01T00:00:00.000Z");
        assert_eq!(format(MAX_MS).unwrap(), "9999-12-31T23:59:59.999Z");
        assert!(format(MIN_MS - 1).is_err() && format(MAX_MS + 1).is_err());
    }

    #[test]
    fn every_day_round_trips() {
        // Every day of the whole range: civil date to days and back, and the weekday
        // advancing by one each day.
        let first = days_from_civil(0, 1, 1);
        let last = days_from_civil(9999, 12, 31);
        let mut wd = weekday(MIN_MS).unwrap();
        for z in first..=last {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
            assert!((1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m), "{y}-{m}-{d}");
            let w = weekday(z * DAY_MS).unwrap();
            if z > first {
                assert_eq!(w, wd % 7 + 1);
            }
            wd = w;
        }
    }
}
