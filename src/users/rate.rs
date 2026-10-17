//! Login rate limits. The counters live in the database (`login_attempts`),
//! so every server copy sees the same numbers.
//!
//! - Per IP: 20 failures in 15 minutes, then the IP waits 15 minutes.
//! - Per email: from the 5th failure a growing wait (1 s, 2 s, 4 s ... up to 60 s).
//!   From the 10th failure the email is locked for 15 minutes.
//!   A wrong try during a wait is refused without checking the password.
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;

const IP_LIMIT: i32 = 20;
const IP_WINDOW_MINS: i64 = 15;
const IP_LOCK_MINS: i64 = 15;
const EMAIL_FREE_TRIES: i32 = 4;
const EMAIL_LOCK_AT: i32 = 10;
const EMAIL_LOCK_MINS: i64 = 15;
const EMAIL_WINDOW_MINS: i64 = 60;

pub fn ip_key(ip: &str) -> String {
    format!("ip:{ip}")
}

pub fn email_key(email: &str) -> String {
    format!("email:{}", email.trim().to_lowercase())
}

/// How long to wait after the n-th failure of one email.
pub fn email_wait(count: i32) -> Option<Duration> {
    if count <= EMAIL_FREE_TRIES {
        None
    } else if count >= EMAIL_LOCK_AT {
        Some(Duration::minutes(EMAIL_LOCK_MINS))
    } else {
        Some(Duration::seconds((1i64 << (count - EMAIL_FREE_TRIES - 1)).min(60)))
    }
}

pub fn ip_wait(count: i32) -> Option<Duration> {
    (count >= IP_LIMIT).then(|| Duration::minutes(IP_LOCK_MINS))
}

/// `Some(seconds)` when one of the keys is waiting. Checked before any password work.
pub async fn blocked_for(pool: &PgPool, keys: &[String], now: DateTime<Utc>) -> Result<Option<i64>, sqlx::Error> {
    let latest: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT max(locked_until) FROM login_attempts WHERE key = ANY($1) AND locked_until > $2")
            .bind(keys)
            .bind(now)
            .fetch_one(pool)
            .await?;
    Ok(latest.map(|t| (t - now).num_seconds().max(1)))
}

/// Count one failure on a key and set its wait. Returns the new count.
async fn bump(pool: &PgPool, key: &str, window: Duration, wait: fn(i32) -> Option<Duration>, now: DateTime<Utc>) -> Result<i32, sqlx::Error> {
    let (count,): (i32,) = sqlx::query_as(
        "INSERT INTO login_attempts (key, count, window_start) VALUES ($1, 1, $2)
         ON CONFLICT (key) DO UPDATE SET
           count = CASE WHEN login_attempts.window_start < $3 THEN 1 ELSE login_attempts.count + 1 END,
           window_start = CASE WHEN login_attempts.window_start < $3 THEN $2 ELSE login_attempts.window_start END
         RETURNING count",
    )
    .bind(key)
    .bind(now)
    .bind(now - window)
    .fetch_one(pool)
    .await?;
    let locked = wait(count).map(|d| now + d);
    sqlx::query("UPDATE login_attempts SET locked_until = $2 WHERE key = $1").bind(key).bind(locked).execute(pool).await?;
    Ok(count)
}

pub async fn record_failure(pool: &PgPool, ip: &str, email: &str, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    bump(pool, &ip_key(ip), Duration::minutes(IP_WINDOW_MINS), ip_wait, now).await?;
    let count = bump(pool, &email_key(email), Duration::minutes(EMAIL_WINDOW_MINS), email_wait, now).await?;
    // Show the lock on the Team page. The counters above are what decides.
    sqlx::query("UPDATE users SET failed_logins = $2, locked_until = $3 WHERE email = $1")
        .bind(email.trim().to_lowercase())
        .bind(count)
        .bind(email_wait(count).map(|d| now + d))
        .execute(pool)
        .await?;
    Ok(())
}

/// A good login clears the counter of the email (not the one of the IP).
pub async fn record_success(pool: &PgPool, email: &str) -> Result<(), sqlx::Error> {
    clear_email(pool, email).await
}

pub async fn clear_email(pool: &PgPool, email: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM login_attempts WHERE key = $1").bind(email_key(email)).execute(pool).await?;
    sqlx::query("UPDATE users SET failed_logins = 0, locked_until = NULL WHERE email = $1")
        .bind(email.trim().to_lowercase())
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_wait_grows_then_locks() {
        assert_eq!(email_wait(4), None);
        assert_eq!(email_wait(5), Some(Duration::seconds(1)));
        assert_eq!(email_wait(6), Some(Duration::seconds(2)));
        assert_eq!(email_wait(7), Some(Duration::seconds(4)));
        assert_eq!(email_wait(9), Some(Duration::seconds(16)));
        assert_eq!(email_wait(10), Some(Duration::minutes(15)));
        assert_eq!(email_wait(30), Some(Duration::minutes(15)));
    }

    #[test]
    fn ip_waits_at_twenty() {
        assert_eq!(ip_wait(19), None);
        assert_eq!(ip_wait(20), Some(Duration::minutes(15)));
    }
}
