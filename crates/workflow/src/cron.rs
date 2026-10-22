//! Cron schedules as GitHub Actions writes them: five fields (minute, hour, day of month, month,
//! day of week) in UTC, with `*`, lists, ranges and steps, and month and day names.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: BTreeSet<u32>,
    hours: BTreeSet<u32>,
    days: BTreeSet<u32>,
    months: BTreeSet<u32>,
    weekdays: BTreeSet<u32>,
    /// Day of month and day of week were both restricted: either matching runs it (as cron does).
    either_day: bool,
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

fn field(spec: &str, lo: u32, hi: u32, names: &[&str], name_base: u32) -> Result<(BTreeSet<u32>, bool), String> {
    let value = |s: &str| -> Result<u32, String> {
        let l = s.to_ascii_lowercase();
        if let Some(i) = names.iter().position(|n| *n == l) {
            return Ok(i as u32 + name_base);
        }
        s.parse::<u32>().map_err(|_| format!("`{s}` isn't a number"))
    };
    let mut out = BTreeSet::new();
    let star = spec == "*";
    for part in spec.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().map_err(|_| format!("`{s}` isn't a step"))?),
            None => (part, 1),
        };
        if step == 0 {
            return Err("a step can't be 0".into());
        }
        let (a, b) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (value(a)?, value(b)?)
        } else {
            let v = value(range)?;
            (v, if part.contains('/') { hi } else { v })
        };
        // Sunday may also be 7 (`7`, `5-7`).
        if hi == 6 && b == 7 && a <= 7 && (7 - a) % step == 0 {
            out.insert(0);
        }
        let b = if hi == 6 && b == 7 { 6 } else { b };
        if a == 7 && b == 6 {
            continue;
        }
        if a < lo || b > hi || a > b {
            return Err(format!("`{part}` is outside {lo}–{hi}"));
        }
        out.extend((a..=b).step_by(step as usize));
    }
    Ok((out, star))
}

impl Cron {
    pub fn parse(spec: &str) -> Result<Cron, String> {
        let f: Vec<&str> = spec.split_whitespace().collect();
        let [m, h, dom, mon, dow] = f[..] else {
            return Err(format!("`{spec}` should have five fields: minute hour day month weekday"));
        };
        let (minutes, _) = field(m, 0, 59, &[], 0)?;
        let (hours, _) = field(h, 0, 23, &[], 0)?;
        let (days, any_day) = field(dom, 1, 31, &[], 0)?;
        let (months, _) = field(mon, 1, 12, &MONTHS, 1)?;
        let (weekdays, any_weekday) = field(dow, 0, 6, &DAYS, 0)?;
        Ok(Cron { minutes, hours, days, months, weekdays, either_day: !any_day && !any_weekday })
    }

    fn matches(&self, t: &Civil) -> bool {
        let day = self.days.contains(&t.day);
        let weekday = self.weekdays.contains(&t.weekday);
        let day_ok = if self.either_day { day || weekday } else { day && weekday };
        self.minutes.contains(&t.minute) && self.hours.contains(&t.hour) && self.months.contains(&t.month) && day_ok
    }

    /// The first time strictly after `after_ms` (milliseconds since the epoch, UTC) it runs;
    /// `None` if it never does within four years (31 February).
    pub fn next_after(&self, after_ms: i64) -> Option<i64> {
        let mut minute = after_ms.div_euclid(60_000) + 1;
        let end = minute + 4 * 366 * 24 * 60;
        while minute < end {
            let t = civil(minute * 60_000);
            if !self.months.contains(&t.month) {
                // To the first minute of next month.
                minute = start_of_next_month(&t);
                continue;
            }
            if self.matches(&t) {
                return Some(minute * 60_000);
            }
            minute += if self.hours.contains(&t.hour) { 1 } else { 60 - t.minute as i64 };
        }
        None
    }
}

struct Civil {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    weekday: u32,
}

/// UTC calendar fields of a time (Howard Hinnant's days-from-civil, inverted).
fn civil(ms: i64) -> Civil {
    let days = ms.div_euclid(86_400_000);
    let secs = ms.div_euclid(1000).rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    Civil {
        year,
        month,
        day,
        hour: (secs / 3600) as u32,
        minute: (secs % 3600 / 60) as u32,
        weekday: (days + 4).rem_euclid(7) as u32,
    }
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn start_of_next_month(t: &Civil) -> i64 {
    let (y, m) = if t.month == 12 { (t.year + 1, 1) } else { (t.year, t.month + 1) };
    days_from_civil(y, m, 1) * 24 * 60
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i64, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        (days_from_civil(y, mo, d) * 1440 + h as i64 * 60 + mi as i64) * 60_000
    }

    #[test]
    fn next_times_like_cron() {
        let c = Cron::parse("0 3 * * *").unwrap();
        assert_eq!(c.next_after(at(2026, 10, 21, 12, 0)), Some(at(2026, 10, 22, 3, 0)));
        let c = Cron::parse("*/15 * * * *").unwrap();
        assert_eq!(c.next_after(at(2026, 10, 21, 12, 7)), Some(at(2026, 10, 21, 12, 15)));
        let c = Cron::parse("30 9 * * mon-fri").unwrap();
        // 2026-10-24 is a Saturday: Monday next.
        assert_eq!(c.next_after(at(2026, 10, 23, 10, 0)), Some(at(2026, 10, 26, 9, 30)));
        let c = Cron::parse("0 0 1 jan,jul *").unwrap();
        assert_eq!(c.next_after(at(2026, 10, 21, 0, 0)), Some(at(2027, 1, 1, 0, 0)));
        let c = Cron::parse("0 12 13 * 5").unwrap();
        // Day 13 or a Friday, as cron does when both are given.
        assert_eq!(c.next_after(at(2026, 10, 21, 0, 0)), Some(at(2026, 10, 23, 12, 0)));
        assert_eq!(Cron::parse("0 0 31 2 *").unwrap().next_after(at(2026, 1, 1, 0, 0)), None);
        assert_eq!(Cron::parse("0 0 * * 7").unwrap(), Cron::parse("0 0 * * 0").unwrap());
    }

    #[test]
    fn says_what_is_wrong() {
        assert!(Cron::parse("0 3 * *").unwrap_err().contains("five fields"));
        assert!(Cron::parse("61 * * * *").unwrap_err().contains("0–59"));
        assert!(Cron::parse("*/0 * * * *").is_err());
        assert!(Cron::parse("x * * * *").is_err());
    }
}
