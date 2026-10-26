//! When a schedule or cron trigger fires next (build map 7.1 and 9.2). Pure functions: no database.
//!
//! The stored `config` of such a trigger is its params with the variables filled in, plus the resolved
//! `timezone`. Interval mode counts in fixed steps (a day is 24 hours). Daily mode and cron use the
//! calendar of the time zone, so they follow daylight saving time.
use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use croner::Cron;
use serde_json::Value;
use std::str::FromStr;

/// The shortest step of an interval (the map: "Minimum interval 1 minute").
const MIN_STEP_SECS: i64 = 60;
/// How many minutes a daily time may be pushed on when a daylight saving change skips it.
const MAX_GAP_MINUTES: i64 = 180;

pub fn parse_tz(name: &str) -> Result<Tz, String> {
    name.trim().parse::<Tz>().map_err(|_| format!("`{name}` is not a time zone name (use an IANA name like `Europe/Berlin`)."))
}

/// A cron expression with 5 fields: minute, hour, day of month, month, day of week. No seconds field.
pub fn parse_cron(expression: &str) -> Result<Cron, String> {
    let fields = expression.split_whitespace().count();
    if fields != 5 {
        return Err(format!("A cron expression has 5 fields (minute hour day-of-month month day-of-week); this one has {fields}."));
    }
    Cron::from_str(expression.trim()).map_err(|e| format!("Invalid cron expression: {e}"))
}

/// The next time after `after` that the cron expression matches, in the time zone `tz`.
pub fn cron_next(expression: &str, tz: Tz, after: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let cron = parse_cron(expression)?;
    let next = cron.find_next_occurrence(&after.with_timezone(&tz), false).map_err(|e| format!("This cron expression never matches: {e}"))?;
    Ok(next.with_timezone(&Utc))
}

/// The next `n` times, for the editor ("next 5 times").
pub fn cron_preview(expression: &str, tz: Tz, after: DateTime<Utc>, n: usize) -> Result<Vec<DateTime<Utc>>, String> {
    let mut out = Vec::with_capacity(n);
    let mut at = after;
    for _ in 0..n {
        at = cron_next(expression, tz, at)?;
        out.push(at);
    }
    Ok(out)
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "mon" => Weekday::Mon,
        "tue" => Weekday::Tue,
        "wed" => Weekday::Wed,
        "thu" => Weekday::Thu,
        "fri" => Weekday::Fri,
        "sat" => Weekday::Sat,
        "sun" => Weekday::Sun,
        _ => return None,
    })
}

/// A wall-clock time on a date in a zone, as UTC. A time that does not exist (daylight saving gap) moves on to the
/// next minute that does. A time that happens twice takes the first.
fn local_at(tz: Tz, date: chrono::NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    let mut naive = date.and_time(time);
    for _ in 0..=MAX_GAP_MINUTES {
        match tz.from_local_datetime(&naive) {
            LocalResult::Single(t) => return Some(t.with_timezone(&Utc)),
            LocalResult::Ambiguous(first, _) => return Some(first.with_timezone(&Utc)),
            LocalResult::None => naive += Duration::minutes(1),
        }
    }
    None
}

/// The next daily time after `after`.
pub fn daily_next(time: &str, days: &[Weekday], tz: Tz, after: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let t = NaiveTime::parse_from_str(time.trim(), "%H:%M").map_err(|_| format!("`{time}` is not a time. Use HH:MM, for example 09:30."))?;
    let start = after.with_timezone(&tz).date_naive();
    for offset in 0..=8 {
        let Some(date) = start.checked_add_signed(Duration::days(offset)) else { break };
        if !days.is_empty() && !days.contains(&date.weekday()) {
            continue;
        }
        if let Some(at) = local_at(tz, date, t).filter(|at| *at > after) {
            return Ok(at);
        }
    }
    Err("No day matches the daily schedule.".into())
}

/// The step of an interval trigger.
pub fn interval_step(config: &Value) -> Result<Duration, String> {
    let every = config.get("every").and_then(Value::as_i64).filter(|n| *n >= 1).ok_or("`every` must be a whole number, 1 or more.")?;
    let unit_secs = match config.get("unit").and_then(Value::as_str) {
        Some("minutes") => 60,
        Some("hours") => 3600,
        Some("days") => 86400,
        _ => return Err("`unit` must be minutes, hours or days.".into()),
    };
    let secs = every.checked_mul(unit_secs).filter(|s| *s <= 366 * 86400).ok_or("The interval is longer than a year.")?;
    if secs < MIN_STEP_SECS {
        return Err("The shortest interval is 1 minute.".into());
    }
    Ok(Duration::seconds(secs))
}

fn config_tz(config: &Value) -> Result<Tz, String> {
    parse_tz(config.get("timezone").and_then(Value::as_str).unwrap_or("UTC"))
}

/// The next time a `schedule` or `cron` trigger fires after `now`. `scheduled` is the time it was due
/// (for an interval it keeps the rhythm: no drift, and no burst after a long stop).
pub fn next_after(kind: &str, config: &Value, scheduled: DateTime<Utc>, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    match kind {
        "cron" => {
            let expr = config.get("expression").and_then(Value::as_str).ok_or("The cron trigger has no `expression`.")?;
            cron_next(expr, config_tz(config)?, now)
        }
        "schedule" => match config.get("mode").and_then(Value::as_str) {
            Some("interval") => {
                let step = interval_step(config)?;
                let next = scheduled + step;
                if next > now {
                    return Ok(next);
                }
                // after a stop, jump to the first step in the future
                let behind = (now - scheduled).num_seconds();
                let k = behind / step.num_seconds() + 1;
                Ok(scheduled + step * i32::try_from(k).map_err(|_| "The schedule is too far behind.")?)
            }
            Some("daily") => {
                let time = config.get("time").and_then(Value::as_str).ok_or("The daily schedule needs a `time`.")?;
                let days = match config.get("days").and_then(Value::as_array) {
                    Some(list) => list.iter().map(|d| d.as_str().and_then(weekday).ok_or_else(|| format!("`{d}` is not a day (mon, tue, wed, thu, fri, sat, sun)."))).collect::<Result<Vec<_>, _>>()?,
                    None => vec![],
                };
                daily_next(time, &days, config_tz(config)?, now)
            }
            _ => Err("`mode` must be `interval` or `daily`.".into()),
        },
        other => Err(format!("`{other}` is not a time trigger.")),
    }
}

/// The first time after the trigger is switched on.
pub fn first_fire(kind: &str, config: &Value, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    next_after(kind, config, now, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn t(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn cron_needs_five_fields_and_a_real_pattern() {
        assert!(parse_cron("0 9 * * 1-5").is_ok());
        assert!(parse_cron("0 0 9 * * 1-5").unwrap_err().contains("5 fields"));
        assert!(parse_cron("").unwrap_err().contains("5 fields"));
        assert!(parse_cron("61 9 * * *").unwrap_err().contains("Invalid cron"));
    }

    #[test]
    fn cron_runs_in_the_time_zone_of_the_trigger() {
        // 09:00 in Dhaka (UTC+6, no daylight saving) is 03:00 UTC
        let dhaka = parse_tz("Asia/Dhaka").unwrap();
        assert_eq!(cron_next("0 9 * * *", dhaka, t(2026, 10, 26, 0, 0)).unwrap(), t(2026, 10, 26, 3, 0));
        assert_eq!(cron_next("0 9 * * *", dhaka, t(2026, 10, 26, 3, 0)).unwrap(), t(2026, 10, 27, 3, 0));
        // the next 5 times
        let next = cron_preview("*/15 * * * *", dhaka, t(2026, 10, 26, 0, 7), 5).unwrap();
        assert_eq!(next, vec![t(2026, 10, 26, 0, 15), t(2026, 10, 26, 0, 30), t(2026, 10, 26, 0, 45), t(2026, 10, 26, 1, 0), t(2026, 10, 26, 1, 15)]);
    }

    #[test]
    fn cron_follows_daylight_saving_time() {
        let berlin = parse_tz("Europe/Berlin").unwrap();
        // 2026-03-29: the clocks go from 02:00 to 03:00. 09:00 local is 07:00 UTC after, 08:00 UTC before.
        assert_eq!(cron_next("0 9 * * *", berlin, t(2026, 3, 28, 12, 0)).unwrap(), t(2026, 3, 29, 7, 0));
        assert_eq!(cron_next("0 9 * * *", berlin, t(2026, 3, 27, 12, 0)).unwrap(), t(2026, 3, 28, 8, 0));
        // 2026-10-25: the clocks go back from 03:00 to 02:00. 09:00 local is 08:00 UTC after.
        assert_eq!(cron_next("0 9 * * *", berlin, t(2026, 10, 24, 12, 0)).unwrap(), t(2026, 10, 25, 8, 0));
        assert_eq!(cron_next("0 9 * * *", berlin, t(2026, 10, 25, 8, 0)).unwrap(), t(2026, 10, 26, 8, 0));
    }

    #[test]
    fn daily_runs_on_the_chosen_days_in_the_zone() {
        let dhaka = parse_tz("Asia/Dhaka").unwrap();
        // 2026-10-26 is a Monday
        let days = [Weekday::Mon, Weekday::Wed];
        assert_eq!(daily_next("09:30", &days, dhaka, t(2026, 10, 26, 0, 0)).unwrap(), t(2026, 10, 26, 3, 30));
        // past the time today: the next allowed day is Wednesday
        assert_eq!(daily_next("09:30", &days, dhaka, t(2026, 10, 26, 3, 30)).unwrap(), t(2026, 10, 28, 3, 30));
        // no days: every day
        assert_eq!(daily_next("09:30", &[], dhaka, t(2026, 10, 26, 3, 30)).unwrap(), t(2026, 10, 27, 3, 30));
        assert!(daily_next("25:00", &[], dhaka, t(2026, 10, 26, 0, 0)).is_err());
    }

    #[test]
    fn daily_on_a_day_that_skips_the_time_runs_at_the_next_valid_minute() {
        let berlin = parse_tz("Europe/Berlin").unwrap();
        // 02:30 does not exist on 2026-03-29 in Berlin. The next valid minute is 03:00 local = 01:00 UTC.
        assert_eq!(daily_next("02:30", &[], berlin, t(2026, 3, 28, 12, 0)).unwrap(), t(2026, 3, 29, 1, 0));
        // 02:30 happens twice on 2026-10-25: the first one is 00:30 UTC
        assert_eq!(daily_next("02:30", &[], berlin, t(2026, 10, 24, 12, 0)).unwrap(), t(2026, 10, 25, 0, 30));
    }

    #[test]
    fn an_interval_keeps_its_rhythm_and_never_bursts() {
        let cfg = json!({ "mode": "interval", "every": 15, "unit": "minutes" });
        let at = t(2026, 10, 26, 10, 0);
        assert_eq!(first_fire("schedule", &cfg, at).unwrap(), t(2026, 10, 26, 10, 15));
        // on time: the next step after the due time
        assert_eq!(next_after("schedule", &cfg, at, at + Duration::seconds(3)).unwrap(), t(2026, 10, 26, 10, 15));
        // the server was down for 2 hours: one jump to the next step in the future, on the same rhythm
        assert_eq!(next_after("schedule", &cfg, at, t(2026, 10, 26, 12, 7)).unwrap(), t(2026, 10, 26, 12, 15));
        // exactly on a step: the next one
        assert_eq!(next_after("schedule", &cfg, at, t(2026, 10, 26, 12, 15)).unwrap(), t(2026, 10, 26, 12, 30));
    }

    #[test]
    fn interval_units_and_limits() {
        assert_eq!(interval_step(&json!({ "every": 2, "unit": "hours" })).unwrap(), Duration::hours(2));
        assert_eq!(interval_step(&json!({ "every": 1, "unit": "days" })).unwrap(), Duration::days(1));
        assert!(interval_step(&json!({ "every": 0, "unit": "hours" })).is_err());
        assert!(interval_step(&json!({ "every": 1, "unit": "weeks" })).is_err());
        assert!(interval_step(&json!({ "every": 100000, "unit": "days" })).unwrap_err().contains("year"));
        assert!(interval_step(&json!({ "unit": "hours" })).is_err());
    }

    #[test]
    fn next_after_reads_the_stored_config() {
        let cron = json!({ "expression": "30 8 * * 1-5", "timezone": "Asia/Dhaka" });
        // Monday 2026-10-26 02:00 UTC = 08:00 Dhaka, so 08:30 Dhaka (02:30 UTC) is next
        assert_eq!(next_after("cron", &cron, t(2026, 10, 26, 0, 0), t(2026, 10, 26, 2, 0)).unwrap(), t(2026, 10, 26, 2, 30));
        // Friday evening: the next one is Monday
        assert_eq!(next_after("cron", &cron, t(2026, 10, 30, 0, 0), t(2026, 10, 30, 20, 0)).unwrap(), t(2026, 11, 2, 2, 30));
        let daily = json!({ "mode": "daily", "time": "08:30", "days": ["mon"], "timezone": "Asia/Dhaka" });
        assert_eq!(next_after("schedule", &daily, t(2026, 10, 26, 0, 0), t(2026, 10, 26, 2, 30)).unwrap(), t(2026, 11, 2, 2, 30));
        assert!(next_after("schedule", &json!({ "mode": "hourly" }), t(2026, 10, 26, 0, 0), t(2026, 10, 26, 0, 0)).is_err());
        assert!(next_after("cron", &json!({ "expression": "0 9 * * *", "timezone": "Mars/Base" }), t(2026, 10, 26, 0, 0), t(2026, 10, 26, 0, 0)).is_err());
        assert!(next_after("webhook", &json!({}), t(2026, 10, 26, 0, 0), t(2026, 10, 26, 0, 0)).is_err());
        assert!(parse_tz("Mars/Base").is_err());
    }
}
