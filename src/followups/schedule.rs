//! Due time math for the steps. Pure: no DB, no clock.
use super::model::DelayType;
use crate::sending::rules::resolve_local;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc, Weekday};
use chrono_tz::Tz;

fn is_weekend(d: NaiveDate) -> bool {
    matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
}

/// Move `n` business days forward. Saturday and Sunday do not count.
/// From a weekend day, one business day later is Monday.
pub fn add_business_days(mut date: NaiveDate, n: u32) -> NaiveDate {
    for _ in 0..n {
        date += Duration::days(1);
        while is_weekend(date) {
            date += Duration::days(1);
        }
    }
    date
}

/// The earliest time the next step may go out: `from` plus the delay, counted on the calendar of the
/// account's time zone. The clock time of day stays the same on the local clock, also over a DST change.
/// The sending engine still applies its own rules (days, window, gap, cap) after this time.
pub fn add_delay(tz: Tz, from: DateTime<Utc>, amount: i32, kind: DelayType) -> DateTime<Utc> {
    if amount <= 0 {
        return from;
    }
    let local = from.with_timezone(&tz);
    let date = local.date_naive();
    let target = match kind {
        DelayType::BusinessDays => add_business_days(date, amount as u32),
        DelayType::CalendarDays => date + Duration::days(amount as i64),
    };
    resolve_local(tz, target.and_time(local.time()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn friday_plus_one_business_day_is_monday() {
        // 2026-10-23 is a Friday.
        assert_eq!(add_delay(Tz::UTC, utc(2026, 10, 23, 10, 0), 1, DelayType::BusinessDays), utc(2026, 10, 26, 10, 0));
    }

    #[test]
    fn business_days_skip_the_weekend() {
        let fri = utc(2026, 10, 23, 14, 30);
        assert_eq!(add_delay(Tz::UTC, fri, 3, DelayType::BusinessDays), utc(2026, 10, 28, 14, 30)); // Wednesday
        assert_eq!(add_delay(Tz::UTC, fri, 5, DelayType::BusinessDays), utc(2026, 10, 30, 14, 30)); // next Friday
        assert_eq!(add_delay(Tz::UTC, utc(2026, 10, 19, 9, 0), 4, DelayType::BusinessDays), utc(2026, 10, 23, 9, 0)); // Mon + 4 = Fri
        // From a weekend day, one business day is Monday.
        assert_eq!(add_delay(Tz::UTC, utc(2026, 10, 24, 9, 0), 1, DelayType::BusinessDays), utc(2026, 10, 26, 9, 0));
        assert_eq!(add_delay(Tz::UTC, utc(2026, 10, 25, 9, 0), 1, DelayType::BusinessDays), utc(2026, 10, 26, 9, 0));
    }

    #[test]
    fn calendar_days_do_not_skip_anything() {
        assert_eq!(add_delay(Tz::UTC, utc(2026, 10, 23, 10, 0), 1, DelayType::CalendarDays), utc(2026, 10, 24, 10, 0));
        assert_eq!(add_delay(Tz::UTC, utc(2026, 10, 23, 10, 0), 3, DelayType::CalendarDays), utc(2026, 10, 26, 10, 0));
    }

    #[test]
    fn zero_means_now() {
        let t = utc(2026, 10, 23, 10, 0);
        assert_eq!(add_delay(Tz::UTC, t, 0, DelayType::BusinessDays), t);
    }

    #[test]
    fn the_weekend_is_read_in_the_account_time_zone() {
        // Friday 2026-10-23 23:30 in New York (EDT, UTC-4) is Saturday 03:30 UTC. In New York it is still Friday,
        // so one business day later is Monday 23:30 New York = Tuesday 03:30 UTC.
        let tz: Tz = "America/New_York".parse().unwrap();
        assert_eq!(add_delay(tz, utc(2026, 10, 24, 3, 30), 1, DelayType::BusinessDays), utc(2026, 10, 27, 3, 30));
        // The same moment counted in UTC would be a Saturday, and give Monday 03:30 UTC.
        assert_eq!(add_delay(Tz::UTC, utc(2026, 10, 24, 3, 30), 1, DelayType::BusinessDays), utc(2026, 10, 26, 3, 30));
    }

    #[test]
    fn the_local_time_stays_the_same_over_a_dst_change() {
        // Europe/Berlin leaves summer time on Sunday 2026-10-25 (UTC+2 to UTC+1).
        let tz: Tz = "Europe/Berlin".parse().unwrap();
        // Friday 10:00 Berlin = 08:00 UTC. Three business days later is Wednesday 10:00 Berlin = 09:00 UTC.
        assert_eq!(add_delay(tz, utc(2026, 10, 23, 8, 0), 3, DelayType::BusinessDays), utc(2026, 10, 28, 9, 0));
        // Two calendar days later is Sunday 10:00 Berlin, now UTC+1 = 09:00 UTC (49 hours, not 48).
        assert_eq!(add_delay(tz, utc(2026, 10, 23, 8, 0), 2, DelayType::CalendarDays), utc(2026, 10, 25, 9, 0));
    }

    #[test]
    fn a_time_that_does_not_exist_moves_forward() {
        // America/New_York skips 02:00 to 03:00 on Sunday 2027-03-14. 02:30 local does not exist that day.
        let tz: Tz = "America/New_York".parse().unwrap();
        // Saturday 2027-03-13 02:30 EST = 07:30 UTC. One calendar day later: 02:30 local does not exist on the 14th.
        let due = add_delay(tz, utc(2027, 3, 13, 7, 30), 1, DelayType::CalendarDays);
        assert!(due >= utc(2027, 3, 14, 7, 0) && due <= utc(2027, 3, 14, 8, 30), "{due}");
    }
}
