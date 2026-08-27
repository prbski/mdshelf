//! Instants, without a date library.
//!
//! mdshelf already formats access-log timestamps by hand rather than taking on a date
//! dependency (`src/cli.rs`). `--until` needs the inverse, and the recipient banner
//! needs a human interval, so both live here with the tests that pin them.

use anyhow::{Result, bail};

/// Parse `--until`, returning milliseconds since the Unix epoch.
///
/// Accepts `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM`, and `YYYY-MM-DDTHH:MM:SS`, each optionally
/// followed by `Z` or `±HH:MM`. Without an offset the value is read as UTC (US-1), which
/// is stated in `--help` rather than guessed at from the machine's timezone: a share
/// that expires a day early or late because of where the server happens to sit is not a
/// failure anybody would look for.
pub fn parse_until(raw: &str) -> Result<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("--until is empty; expected a date such as 2026-09-01");
    }

    let (body, offset_minutes) = split_offset(trimmed)?;
    let (date_part, time_part) = match body.split_once(['T', ' ']) {
        Some((date, time)) => (date, Some(time)),
        None => (body, None),
    };

    let (year, month, day) = parse_date(date_part, raw)?;
    let (hour, minute, second) = match time_part {
        Some(time) => parse_time(time, raw)?,
        // A bare date means the end of that day, so `--until 2026-09-01` covers the
        // whole of the first rather than expiring as it begins.
        None => (23, 59, 59),
    };

    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + (hour as i64) * 3600 + (minute as i64) * 60 + second as i64
        - (offset_minutes as i64) * 60;
    Ok(seconds * 1000)
}

/// Split a trailing `Z` or `±HH:MM` from the timestamp body.
fn split_offset(raw: &str) -> Result<(&str, i32)> {
    if let Some(body) = raw.strip_suffix('Z').or_else(|| raw.strip_suffix('z')) {
        return Ok((body, 0));
    }
    // Only look for a sign after the date, so the `-` separators in `2026-09-01` are
    // never mistaken for an offset.
    let search_from = raw.find(['T', ' ']).unwrap_or(0);
    if let Some(position) = raw[search_from..]
        .rfind(['+', '-'])
        .map(|p| p + search_from)
        && position > 0
        && search_from > 0
    {
        let (body, offset) = raw.split_at(position);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let digits = &offset[1..];
        let (hours, minutes) = match digits.split_once(':') {
            Some((h, m)) => (h, m),
            None if digits.len() == 4 => (&digits[..2], &digits[2..]),
            None => bail!("`{raw}` has an offset that is not ±HH:MM"),
        };
        let hours: i32 = hours
            .parse()
            .map_err(|_| anyhow::anyhow!("`{raw}` has an offset that is not ±HH:MM"))?;
        let minutes: i32 = minutes
            .parse()
            .map_err(|_| anyhow::anyhow!("`{raw}` has an offset that is not ±HH:MM"))?;
        if hours > 23 || minutes > 59 {
            bail!("`{raw}` has an out-of-range UTC offset");
        }
        return Ok((body, sign * (hours * 60 + minutes)));
    }
    Ok((raw, 0))
}

fn parse_date(part: &str, raw: &str) -> Result<(i64, u32, u32)> {
    let fields: Vec<&str> = part.split('-').collect();
    if fields.len() != 3 || fields[0].len() != 4 || fields[1].len() != 2 || fields[2].len() != 2 {
        bail!("`{raw}` is not a date; expected YYYY-MM-DD");
    }
    let year: i64 = fields[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("`{raw}` is not a date; expected YYYY-MM-DD"))?;
    let month: u32 = fields[1]
        .parse()
        .map_err(|_| anyhow::anyhow!("`{raw}` is not a date; expected YYYY-MM-DD"))?;
    let day: u32 = fields[2]
        .parse()
        .map_err(|_| anyhow::anyhow!("`{raw}` is not a date; expected YYYY-MM-DD"))?;
    if !(1..=12).contains(&month) {
        bail!("`{raw}` has month {month}, which does not exist");
    }
    if day == 0 || day > days_in_month(year, month) {
        bail!("`{raw}` has a day that does not exist in that month");
    }
    Ok((year, month, day))
}

fn parse_time(part: &str, raw: &str) -> Result<(u32, u32, u32)> {
    let fields: Vec<&str> = part.split(':').collect();
    if fields.len() < 2 || fields.len() > 3 {
        bail!("`{raw}` is not a time; expected HH:MM or HH:MM:SS");
    }
    let mut values = [0u32; 3];
    for (index, field) in fields.iter().enumerate() {
        if field.len() != 2 {
            bail!("`{raw}` is not a time; expected HH:MM or HH:MM:SS");
        }
        values[index] = field
            .parse()
            .map_err(|_| anyhow::anyhow!("`{raw}` is not a time; expected HH:MM or HH:MM:SS"))?;
    }
    if values[0] > 23 || values[1] > 59 || values[2] > 59 {
        bail!("`{raw}` has an out-of-range time");
    }
    Ok((values[0], values[1], values[2]))
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Howard Hinnant's days-from-civil algorithm.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = month as i64;
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// A short, human interval for the recipient banner: "20h", "3d", "45m" (S26).
///
/// Rounds down, so the banner never promises time the link does not have.
pub fn humanize_remaining(milliseconds: i64) -> String {
    if milliseconds <= 0 {
        return "less than a minute".to_string();
    }
    let seconds = milliseconds / 1000;
    let (value, unit) = if seconds >= 86_400 {
        (seconds / 86_400, "day")
    } else if seconds >= 3_600 {
        (seconds / 3_600, "hour")
    } else if seconds >= 60 {
        (seconds / 60, "minute")
    } else {
        return "less than a minute".to_string();
    };
    if value == 1 {
        format!("1 {unit}")
    } else {
        format!("{value} {unit}s")
    }
}

/// Format a millisecond instant as `YYYY-MM-DD HH:MM:SSZ`.
pub fn format_instant(milliseconds: i64) -> String {
    let seconds = milliseconds.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60
    )
}

/// Howard Hinnant's days-from-civil algorithm, inverted.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days_round_trip() {
        for (year, month, day) in [
            (1970, 1, 1),
            (2000, 2, 29),
            (2026, 9, 1),
            (2026, 12, 31),
            (2100, 3, 1),
        ] {
            let days = days_from_civil(year, month, day);
            assert_eq!(civil_from_days(days), (year, month, day));
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn a_bare_date_is_read_as_the_end_of_that_day_in_utc() {
        let ms = parse_until("2026-09-01").unwrap();
        assert_eq!(format_instant(ms), "2026-09-01 23:59:59Z");
    }

    #[test]
    fn an_explicit_time_is_read_as_utc() {
        assert_eq!(
            format_instant(parse_until("2026-09-01T08:30").unwrap()),
            "2026-09-01 08:30:00Z"
        );
        assert_eq!(
            format_instant(parse_until("2026-09-01T08:30:15Z").unwrap()),
            "2026-09-01 08:30:15Z"
        );
    }

    #[test]
    fn an_offset_is_honoured_rather_than_ignored() {
        // 08:30+02:00 is 06:30 UTC. Ignoring the offset would hand the recipient two
        // extra hours of access.
        assert_eq!(
            format_instant(parse_until("2026-09-01T08:30:00+02:00").unwrap()),
            "2026-09-01 06:30:00Z"
        );
        assert_eq!(
            format_instant(parse_until("2026-09-01T08:30:00-05:00").unwrap()),
            "2026-09-01 13:30:00Z"
        );
        assert_eq!(
            format_instant(parse_until("2026-09-01T08:30:00+0200").unwrap()),
            "2026-09-01 06:30:00Z"
        );
    }

    #[test]
    fn nonsense_dates_are_refused_rather_than_rounded() {
        for bad in [
            "",
            "tomorrow",
            "2026-13-01",
            "2026-02-30",
            "2026-9-1",
            "26-09-01",
            "2026-09-01T25:00",
            "2026-09-01T08:60",
            "2026-09-01T08:30:00+99:00",
        ] {
            assert!(parse_until(bad).is_err(), "`{bad}` should be refused");
        }
        // A leap day exists in 2028 but not in 2026.
        assert!(parse_until("2028-02-29").is_ok());
        assert!(parse_until("2026-02-29").is_err());
    }

    #[test]
    fn remaining_time_rounds_down() {
        assert_eq!(humanize_remaining(0), "less than a minute");
        assert_eq!(humanize_remaining(59_000), "less than a minute");
        assert_eq!(humanize_remaining(60_000), "1 minute");
        assert_eq!(humanize_remaining(3_600_000), "1 hour");
        // 20h59m must not be announced as 21 hours.
        assert_eq!(humanize_remaining(20 * 3_600_000 + 59 * 60_000), "20 hours");
        assert_eq!(humanize_remaining(3 * 86_400_000 + 1), "3 days");
    }
}
