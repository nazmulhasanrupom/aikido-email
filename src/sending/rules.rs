//! Pure rule checks. No DB, no clock: every function takes `now` as an input,
//! so tests can use a fake clock.
use super::settings::SendingSettings;
use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use rand::Rng;
use serde::Serialize;
use utoipa::ToSchema;

/// The live counters of one account (a copy of the `account_send_state` columns the rules need).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SendState {
    pub next_send_at: Option<DateTime<Utc>>,
    pub local_day: Option<NaiveDate>,
    pub sent_today: i32,
    pub today_limit: Option<i32>,
    pub warmup_day: i32,
}

/// Counts from the DB that the pure rules need.
#[derive(Clone, Debug, Default)]
pub struct Counts {
    /// Times of the sends in the last 60 minutes (any order).
    pub recent_sends: Vec<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    NotSendDay,
    BlockedDate,
    OutsideWindow,
    DailyLimitReached,
    HourlyLimitReached,
    GapNotElapsed,
    /// A follow-up waits: the inbox was not read lately, so a reply could be missed.
    ReplyCheckStale,
    /// A follow-up waits: the inbox cannot be read (reconnect the account, or set `reply_check_max_age_mins` to 0).
    ReplyCheckUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Ok,
    Wait { reason: WaitReason, until: DateTime<Utc> },
}

// ---------- time helpers ----------

/// Local date and time to a UTC moment. In a DST gap (a local time that does not exist),
/// moves forward to the first time that does. In an overlap (a local time that happens twice),
/// takes the first one.
pub fn resolve_local(tz: Tz, mut naive: NaiveDateTime) -> DateTime<Utc> {
    for _ in 0..100 {
        match tz.from_local_datetime(&naive) {
            LocalResult::Single(t) => return t.with_timezone(&Utc),
            LocalResult::Ambiguous(first, _) => return first.with_timezone(&Utc),
            LocalResult::None => naive += Duration::minutes(30),
        }
    }
    Utc.from_utc_datetime(&naive)
}

pub fn local_date(s: &SendingSettings, now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&s.tz()).date_naive()
}

/// When the window opens on a given local date.
pub fn window_open_time(s: &SendingSettings, date: NaiveDate) -> DateTime<Utc> {
    resolve_local(s.tz(), date.and_time(s.window().0))
}

/// Start of a local date (midnight) as a UTC moment.
pub fn day_start(s: &SendingSettings, date: NaiveDate) -> DateTime<Utc> {
    resolve_local(s.tz(), date.and_hms_opt(0, 0, 0).unwrap())
}

fn is_send_weekday(s: &SendingSettings, date: NaiveDate) -> bool {
    s.send_days.contains(&(date.weekday().number_from_monday() as u8))
}

pub fn is_send_day(s: &SendingSettings, date: NaiveDate) -> bool {
    is_send_weekday(s, date) && !s.blocked_dates.contains(&date)
}

/// Is `now` on a send day and inside the window? (Start is included, end is not.)
pub fn inside_window(s: &SendingSettings, now: DateTime<Utc>) -> bool {
    let local = now.with_timezone(&s.tz());
    let (ws, we) = s.window();
    is_send_day(s, local.date_naive()) && local.time() >= ws && local.time() < we
}

/// The earliest moment at or after `after` that is inside the window of a send day.
pub fn next_window_open(s: &SendingSettings, after: DateTime<Utc>) -> DateTime<Utc> {
    let tz = s.tz();
    let (ws, we) = s.window();
    let mut date = after.with_timezone(&tz).date_naive();
    for _ in 0..800 {
        if is_send_day(s, date) {
            let start = resolve_local(tz, date.and_time(ws));
            let end = resolve_local(tz, date.and_time(we));
            let candidate = start.max(after);
            if candidate < end {
                return candidate;
            }
        }
        date = date.succ_opt().unwrap_or(date + Duration::days(1));
    }
    after + Duration::days(365)
}

// ---------- limits and gaps ----------

/// Cap for one sending day before jitter: the warm-up cap (if on), never above `daily_limit`.
pub fn base_cap(s: &SendingSettings, warmup_day: i32) -> u32 {
    if s.warmup_enabled {
        let cap = s.warmup_start as u64 + s.warmup_step as u64 * warmup_day.max(0) as u64;
        cap.min(s.daily_limit as u64) as u32
    } else {
        s.daily_limit
    }
}

/// The cap picked for one day: random between `cap - jitter%` and `cap`.
pub fn pick_today_limit(s: &SendingSettings, warmup_day: i32, rng: &mut impl Rng) -> u32 {
    let cap = base_cap(s, warmup_day);
    let drop = (cap as u64 * s.daily_limit_jitter_pct as u64 / 100) as u32;
    let low = cap.saturating_sub(drop).max(1).min(cap);
    rng.gen_range(low..=cap)
}

/// Gap after a send: `base_delay_secs + random(jitter_min ..= jitter_max)`.
pub fn gap_secs(s: &SendingSettings, rng: &mut impl Rng) -> i64 {
    s.base_delay_secs as i64 + rng.gen_range(s.jitter_min_secs..=s.jitter_max_secs) as i64
}

/// Moves the counters to the local day of `now`. Does nothing if the day is the same.
/// - `sent_today = 0`
/// - `warmup_day + 1` if the old day had sends
/// - pick `today_limit`
/// - `next_send_at` is at least today's window start plus a random 0 to `window_start_jitter_mins`
pub fn rollover(s: &SendingSettings, state: &SendState, now: DateTime<Utc>, rng: &mut impl Rng) -> SendState {
    let today = local_date(s, now);
    if state.local_day == Some(today) {
        return state.clone();
    }
    let had_sends = state.local_day.is_some() && state.sent_today > 0;
    let warmup_day = state.warmup_day + i32::from(had_sends);
    let limit = pick_today_limit(s, warmup_day, rng) as i32;
    let jitter = rng.gen_range(0..=s.window_start_jitter_mins as i64 * 60);
    let earliest = window_open_time(s, today) + Duration::seconds(jitter);
    SendState {
        next_send_at: Some(state.next_send_at.map_or(earliest, |n| n.max(earliest))),
        local_day: Some(today),
        sent_today: 0,
        today_limit: Some(limit),
        warmup_day,
    }
}

// ---------- the gate ----------

/// Can this account send an email right now? All rules must pass.
/// On `Wait`, `until` is the earliest moment all rules could pass.
pub fn can_send(s: &SendingSettings, state: &SendState, counts: &Counts, now: DateTime<Utc>) -> Decision {
    let tz = s.tz();
    let local = now.with_timezone(&tz);
    let today = local.date_naive();
    let time = local.time();
    let (ws, we) = s.window();

    let mut waits: Vec<(WaitReason, DateTime<Utc>)> = Vec::new();

    if !is_send_weekday(s, today) {
        waits.push((WaitReason::NotSendDay, next_window_open(s, now)));
    } else if s.blocked_dates.contains(&today) {
        waits.push((WaitReason::BlockedDate, next_window_open(s, now)));
    } else if time < ws {
        waits.push((WaitReason::OutsideWindow, window_open_time(s, today).max(now)));
    } else if time >= we {
        waits.push((WaitReason::OutsideWindow, next_window_open(s, now)));
    }

    // A counter from an older day does not count. Rollover will reset it.
    let same_day = state.local_day == Some(today);
    let sent_today = if same_day { state.sent_today } else { 0 };
    let limit = match (same_day, state.today_limit) {
        (true, Some(l)) => l,
        _ => base_cap(s, state.warmup_day) as i32,
    };
    if sent_today >= limit {
        let next_day = day_start(s, today.succ_opt().unwrap_or(today));
        waits.push((WaitReason::DailyLimitReached, next_window_open(s, next_day.max(now))));
    }

    let since = now - Duration::hours(1);
    let mut recent: Vec<DateTime<Utc>> = counts.recent_sends.iter().copied().filter(|t| *t > since).collect();
    recent.sort_unstable();
    let hourly = s.hourly_limit as usize;
    if recent.len() >= hourly {
        let frees_up = recent[recent.len() - hourly] + Duration::hours(1);
        waits.push((WaitReason::HourlyLimitReached, next_window_open(s, frees_up.max(now))));
    }

    if let Some(next) = state.next_send_at {
        if next > now {
            waits.push((WaitReason::GapNotElapsed, next_window_open(s, next)));
        }
    }

    // The latest `until` wins: only then can all rules pass.
    let mut best: Option<(WaitReason, DateTime<Utc>)> = None;
    for w in waits {
        if best.map_or(true, |b| w.1 > b.1) {
            best = Some(w);
        }
    }
    match best {
        None => Decision::Ok,
        Some((reason, until)) => Decision::Wait { reason, until },
    }
}

/// Simulates the next `count` send times with the current rules. Sends nothing.
pub fn simulate(
    s: &SendingSettings,
    state: &SendState,
    recent_sends: &[DateTime<Utc>],
    now: DateTime<Utc>,
    count: usize,
    rng: &mut impl Rng,
) -> Vec<DateTime<Utc>> {
    let mut out = Vec::with_capacity(count);
    let mut state = state.clone();
    let mut recent = recent_sends.to_vec();
    let mut t = now;
    for _ in 0..(count * 50 + 1000) {
        if out.len() >= count {
            break;
        }
        state = rollover(s, &state, t, rng);
        match can_send(s, &state, &Counts { recent_sends: recent.clone() }, t) {
            Decision::Ok => {
                out.push(t);
                state.sent_today += 1;
                state.next_send_at = Some(t + Duration::seconds(gap_secs(s, rng)));
                recent.push(t);
                recent.retain(|r| *r > t - Duration::hours(1));
            }
            Decision::Wait { until, .. } => t = until.max(t + Duration::seconds(1)),
        }
    }
    out
}

/// Pause when hard bounces pass `pause_bounce_rate_pct` of the last sends.
/// Checked only when there are enough sends (at least 20, or `bounce_window` if that is lower).
pub fn bounce_rate_exceeded(s: &SendingSettings, total: u32, bounces: u32) -> bool {
    total >= s.bounce_window.min(20) && (bounces as u64) * 100 > (s.pause_bounce_rate_pct as u64) * (total as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    /// State for a fresh day: nothing sent, no gap, the daily cap picked.
    fn fresh(s: &SendingSettings, now: DateTime<Utc>) -> SendState {
        SendState {
            local_day: Some(local_date(s, now)),
            today_limit: Some(s.daily_limit as i32),
            ..Default::default()
        }
    }

    fn check(s: &SendingSettings, now: DateTime<Utc>) -> Decision {
        can_send(s, &fresh(s, now), &Counts::default(), now)
    }

    fn wait_until(d: Decision) -> DateTime<Utc> {
        match d {
            Decision::Wait { until, .. } => until,
            Decision::Ok => panic!("expected Wait"),
        }
    }

    // 2026-10-17 is a Saturday. 2026-10-19 is a Monday.

    #[test]
    fn weekend_waits_and_monday_is_ok() {
        let s = SendingSettings::default();
        let sat = utc(2026, 10, 17, 10, 0);
        let sun = utc(2026, 10, 18, 10, 0);
        let mon = utc(2026, 10, 19, 10, 0);
        assert!(matches!(check(&s, sat), Decision::Wait { reason: WaitReason::NotSendDay, .. }));
        assert!(matches!(check(&s, sun), Decision::Wait { reason: WaitReason::NotSendDay, .. }));
        assert_eq!(check(&s, mon), Decision::Ok);
        // The wait ends on Monday when the window opens.
        assert_eq!(wait_until(check(&s, sat)), utc(2026, 10, 19, 9, 0));
        assert_eq!(wait_until(check(&s, sun)), utc(2026, 10, 19, 9, 0));
    }

    #[test]
    fn window_start_included_end_excluded() {
        let s = SendingSettings::default();
        let before = check(&s, utc(2026, 10, 19, 8, 59));
        assert!(matches!(before, Decision::Wait { reason: WaitReason::OutsideWindow, .. }));
        assert_eq!(wait_until(before), utc(2026, 10, 19, 9, 0));
        assert_eq!(check(&s, utc(2026, 10, 19, 9, 0)), Decision::Ok);
        assert_eq!(check(&s, utc(2026, 10, 19, 16, 59)), Decision::Ok);
        let after = check(&s, utc(2026, 10, 19, 17, 0));
        assert!(matches!(after, Decision::Wait { reason: WaitReason::OutsideWindow, .. }));
        assert_eq!(wait_until(after), utc(2026, 10, 20, 9, 0));
    }

    #[test]
    fn friday_evening_waits_until_monday() {
        let s = SendingSettings::default();
        assert_eq!(wait_until(check(&s, utc(2026, 10, 23, 18, 0))), utc(2026, 10, 26, 9, 0));
    }

    #[test]
    fn same_utc_moment_differs_by_time_zone() {
        // 04:00 UTC on Monday 19 Oct = 10:00 in Dhaka (UTC+6), but 00:00 in New York (UTC-4).
        let now = utc(2026, 10, 19, 4, 0);
        let dhaka = SendingSettings { timezone: "Asia/Dhaka".into(), ..Default::default() };
        let ny = SendingSettings { timezone: "America/New_York".into(), ..Default::default() };
        assert_eq!(check(&dhaka, now), Decision::Ok);
        let d = check(&ny, now);
        assert!(matches!(d, Decision::Wait { reason: WaitReason::OutsideWindow, .. }));
        assert_eq!(wait_until(d), utc(2026, 10, 19, 13, 0)); // 09:00 EDT
    }

    fn all_days() -> SendingSettings {
        SendingSettings { timezone: "America/New_York".into(), send_days: (1..=7).collect(), ..Default::default() }
    }

    #[test]
    fn dst_start_in_march() {
        // Sunday 8 March 2026: clocks jump from 02:00 to 03:00. After that New York is UTC-4.
        let s = all_days();
        assert!(matches!(check(&s, utc(2026, 3, 8, 12, 59)), Decision::Wait { .. })); // 08:59 EDT
        assert_eq!(check(&s, utc(2026, 3, 8, 13, 0)), Decision::Ok); // 09:00 EDT
        assert_eq!(wait_until(check(&s, utc(2026, 3, 8, 12, 0))), utc(2026, 3, 8, 13, 0));
        // Saturday evening waits for the Sunday window, which is one hour earlier in UTC.
        assert_eq!(wait_until(check(&s, utc(2026, 3, 8, 3, 0))), utc(2026, 3, 8, 13, 0));
        assert_eq!(wait_until(check(&s, utc(2026, 3, 8, 21, 0))), utc(2026, 3, 9, 13, 0));
    }

    #[test]
    fn dst_end_in_november() {
        // Sunday 1 November 2026: clocks go back from 02:00 to 01:00. After that New York is UTC-5.
        let s = all_days();
        assert!(matches!(check(&s, utc(2026, 11, 1, 13, 59)), Decision::Wait { .. })); // 08:59 EST
        assert_eq!(check(&s, utc(2026, 11, 1, 14, 0)), Decision::Ok); // 09:00 EST
        assert_eq!(wait_until(check(&s, utc(2026, 11, 1, 22, 0))), utc(2026, 11, 2, 14, 0));
    }

    #[test]
    fn window_inside_a_dst_gap_does_not_crash() {
        // 02:30 does not exist on 8 March in New York. The window opens at the next valid time.
        let s = SendingSettings {
            window_start: "02:30".into(),
            window_end: "05:00".into(),
            ..all_days()
        };
        let open = window_open_time(&s, NaiveDate::from_ymd_opt(2026, 3, 8).unwrap());
        assert!(open >= utc(2026, 3, 8, 7, 0) && open <= utc(2026, 3, 8, 8, 30));
        let _ = check(&s, utc(2026, 3, 8, 8, 0));
    }

    #[test]
    fn daily_limit_waits_until_next_allowed_window() {
        let s = SendingSettings::default();
        let now = utc(2026, 10, 19, 12, 0);
        let mut st = fresh(&s, now);
        st.sent_today = 30;
        let d = can_send(&s, &st, &Counts::default(), now);
        assert!(matches!(d, Decision::Wait { reason: WaitReason::DailyLimitReached, .. }));
        assert_eq!(wait_until(d), utc(2026, 10, 20, 9, 0));
        // On Friday the next allowed day is Monday.
        let fri = utc(2026, 10, 23, 12, 0);
        let mut st = fresh(&s, fri);
        st.sent_today = 30;
        assert_eq!(wait_until(can_send(&s, &st, &Counts::default(), fri)), utc(2026, 10, 26, 9, 0));
        // 29 sent is still fine.
        st.sent_today = 29;
        assert_eq!(can_send(&s, &st, &Counts::default(), fri), Decision::Ok);
    }

    #[test]
    fn blocked_date_waits() {
        let s = SendingSettings {
            blocked_dates: vec![NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()],
            ..Default::default()
        };
        let now = utc(2026, 10, 19, 10, 0);
        let d = check(&s, now);
        assert!(matches!(d, Decision::Wait { reason: WaitReason::BlockedDate, .. }));
        assert_eq!(wait_until(d), utc(2026, 10, 20, 9, 0));
    }

    #[test]
    fn blocked_date_uses_the_account_time_zone() {
        // 20:00 UTC Sunday 18 Oct is already Monday 19 Oct 02:00 in Dhaka.
        let s = SendingSettings {
            timezone: "Asia/Dhaka".into(),
            blocked_dates: vec![NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()],
            ..Default::default()
        };
        assert_eq!(wait_until(check(&s, utc(2026, 10, 18, 20, 0))), utc(2026, 10, 20, 3, 0));
    }

    #[test]
    fn gap_is_enforced_and_wait_until_is_the_gap_end() {
        let s = SendingSettings::default();
        let now = utc(2026, 10, 19, 10, 0);
        let mut st = fresh(&s, now);
        st.next_send_at = Some(utc(2026, 10, 19, 10, 7));
        let d = can_send(&s, &st, &Counts::default(), now);
        assert!(matches!(d, Decision::Wait { reason: WaitReason::GapNotElapsed, .. }));
        assert_eq!(wait_until(d), utc(2026, 10, 19, 10, 7));
        assert_eq!(can_send(&s, &st, &Counts::default(), utc(2026, 10, 19, 10, 7)), Decision::Ok);
    }

    #[test]
    fn gap_that_ends_after_closing_waits_for_next_day() {
        let s = SendingSettings::default();
        let now = utc(2026, 10, 19, 16, 55);
        let mut st = fresh(&s, now);
        st.next_send_at = Some(utc(2026, 10, 19, 17, 5));
        assert_eq!(wait_until(can_send(&s, &st, &Counts::default(), now)), utc(2026, 10, 20, 9, 0));
    }

    #[test]
    fn hourly_limit_waits_until_the_oldest_send_ages_out() {
        let s = SendingSettings { hourly_limit: 3, ..Default::default() };
        let now = utc(2026, 10, 19, 11, 0);
        let counts = Counts {
            recent_sends: vec![utc(2026, 10, 19, 10, 40), utc(2026, 10, 19, 10, 10), utc(2026, 10, 19, 10, 25)],
        };
        let d = can_send(&s, &fresh(&s, now), &counts, now);
        assert!(matches!(d, Decision::Wait { reason: WaitReason::HourlyLimitReached, .. }));
        assert_eq!(wait_until(d), utc(2026, 10, 19, 11, 10));
        // Two in the last hour is fine.
        let counts = Counts { recent_sends: vec![utc(2026, 10, 19, 10, 40), utc(2026, 10, 19, 10, 25)] };
        assert_eq!(can_send(&s, &fresh(&s, now), &counts, now), Decision::Ok);
        // Sends older than 60 minutes do not count.
        let counts = Counts { recent_sends: vec![utc(2026, 10, 19, 9, 0); 5] };
        assert_eq!(can_send(&s, &fresh(&s, now), &counts, now), Decision::Ok);
    }

    #[test]
    fn wait_until_is_the_latest_blocker() {
        // Saturday with a gap that ends Saturday afternoon: the answer is still Monday.
        let s = SendingSettings::default();
        let now = utc(2026, 10, 17, 10, 0);
        let mut st = fresh(&s, now);
        st.next_send_at = Some(utc(2026, 10, 17, 10, 7));
        assert_eq!(wait_until(can_send(&s, &st, &Counts::default(), now)), utc(2026, 10, 19, 9, 0));
    }

    #[test]
    fn gap_draws_stay_between_420_and_900_seconds() {
        let s = SendingSettings::default();
        let mut rng = StdRng::seed_from_u64(1);
        let mut seen_low = i64::MAX;
        let mut seen_high = 0;
        for _ in 0..1000 {
            let g = gap_secs(&s, &mut rng);
            assert!((420..=900).contains(&g), "gap {g} out of range");
            seen_low = seen_low.min(g);
            seen_high = seen_high.max(g);
        }
        assert!(seen_high - seen_low > 300, "the gap should really be random");
    }

    #[test]
    fn warmup_caps() {
        let s = SendingSettings { warmup_enabled: true, ..Default::default() };
        assert_eq!(base_cap(&s, 0), 5); // day 1
        assert_eq!(base_cap(&s, 1), 7); // day 2
        assert_eq!(base_cap(&s, 2), 9);
        assert_eq!(base_cap(&s, 1000), 30); // never above daily_limit
        let off = SendingSettings::default();
        assert_eq!(base_cap(&off, 0), 30);
    }

    #[test]
    fn daily_jitter_stays_between_27_and_30() {
        let s = SendingSettings { daily_limit_jitter_pct: 10, ..Default::default() };
        let mut rng = StdRng::seed_from_u64(2);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let l = pick_today_limit(&s, 0, &mut rng);
            assert!((27..=30).contains(&l), "limit {l}");
            seen.insert(l);
        }
        assert_eq!(seen.len(), 4);
    }

    #[test]
    fn rollover_resets_counters_and_adds_start_jitter() {
        let s = SendingSettings { warmup_enabled: true, ..Default::default() };
        let mut rng = StdRng::seed_from_u64(3);
        let old = SendState {
            local_day: Some(NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()),
            sent_today: 4,
            today_limit: Some(5),
            warmup_day: 0,
            next_send_at: Some(utc(2026, 10, 19, 15, 0)),
        };
        let now = utc(2026, 10, 20, 3, 0);
        let new = rollover(&s, &old, now, &mut rng);
        assert_eq!(new.sent_today, 0);
        assert_eq!(new.warmup_day, 1); // the old day had sends
        assert_eq!(new.today_limit, Some(7)); // day 2
        let n = new.next_send_at.unwrap();
        assert!(n >= utc(2026, 10, 20, 9, 0) && n <= utc(2026, 10, 20, 9, 15), "{n}");
        // Same day again: nothing changes.
        assert_eq!(rollover(&s, &new, now + Duration::hours(1), &mut rng), new);
    }

    #[test]
    fn rollover_without_sends_keeps_warmup_day() {
        let s = SendingSettings { warmup_enabled: true, ..Default::default() };
        let mut rng = StdRng::seed_from_u64(4);
        let first = rollover(&s, &SendState::default(), utc(2026, 10, 19, 3, 0), &mut rng);
        assert_eq!(first.warmup_day, 0);
        assert_eq!(first.today_limit, Some(5));
        let second = rollover(&s, &first, utc(2026, 10, 20, 3, 0), &mut rng);
        assert_eq!(second.warmup_day, 0); // nothing was sent on the first day
    }

    #[test]
    fn rollover_keeps_a_later_gap() {
        let s = SendingSettings::default();
        let mut rng = StdRng::seed_from_u64(5);
        let old = SendState {
            local_day: Some(NaiveDate::from_ymd_opt(2026, 10, 19).unwrap()),
            next_send_at: Some(utc(2026, 10, 20, 12, 0)),
            ..Default::default()
        };
        let new = rollover(&s, &old, utc(2026, 10, 20, 8, 0), &mut rng);
        assert_eq!(new.next_send_at, Some(utc(2026, 10, 20, 12, 0)));
    }

    #[test]
    fn preview_respects_every_rule() {
        let s = SendingSettings { daily_limit: 6, ..Default::default() };
        let mut rng = StdRng::seed_from_u64(6);
        let now = utc(2026, 10, 17, 12, 0); // Saturday
        let times = simulate(&s, &SendState::default(), &[], now, 20, &mut rng);
        assert_eq!(times.len(), 20);
        assert!(times[0] >= utc(2026, 10, 19, 9, 0));
        let mut per_day = std::collections::HashMap::new();
        for w in times.windows(2) {
            let gap = (w[1] - w[0]).num_seconds();
            let same_day = w[0].date_naive() == w[1].date_naive();
            if same_day {
                assert!((420..=900).contains(&gap), "gap {gap}");
            }
        }
        for t in &times {
            let local = t.with_timezone(&s.tz());
            assert!(local.weekday().number_from_monday() <= 5);
            assert!(local.time() >= s.window().0 && local.time() < s.window().1);
            *per_day.entry(local.date_naive()).or_insert(0) += 1;
        }
        assert!(per_day.values().all(|n| *n <= 6));
    }

    #[test]
    fn bounce_rate() {
        let s = SendingSettings::default(); // 5% of the last 50, at least 20 sends
        assert!(!bounce_rate_exceeded(&s, 19, 19));
        assert!(!bounce_rate_exceeded(&s, 40, 2)); // exactly 5%
        assert!(bounce_rate_exceeded(&s, 40, 3));
        assert!(!bounce_rate_exceeded(&s, 50, 0));
    }
}
