//! How many password guesses anyone gets.
//!
//! Two layers. Per client address, in memory ([`Limits`]): bursts of
//! requests from one place answer 429 before any work. Per email address, in
//! the database (`sign_in_failures`): every attempt counts before its
//! password is checked ([`attempt`]), the [`LOCK_AFTER`]th in a series locks
//! the address, [`LOCK_FOR`] the first time in a day, 30 minutes the
//! second, an hour ([`MAX_LOCK`]) from the third on ([`lock_duration`]), and
//! a new series starts when a lock runs out: at most 10 guesses per lock
//! (OWASP ASVS 2.2.1 allows a hundred an hour), however many requests arrive
//! at once, and an owner never locked out for long. An address locked again
//! and again is logged. A success, a password reset or `romulus-api unlock`
//! clears it, and a day without attempts forgets them.
//!
//! The lock is per address whether an account has it or not, and a locked
//! address is answered without checking the password: the answers are the
//! same for every address, so they say nothing about which have accounts.
//!
//! A lock anyone can set could keep an owner out for good (one guess every
//! 15 minutes). So a browser that signed in to the account before carries a
//! device cookie (`sign_in_devices`, OWASP's device cookies): its attempts
//! count against that device, never the address ([`device_attempt`]), and
//! its owner signs in whatever the address's lock.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Duration,
};

use chrono::{DateTime, Utc};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::rate::Limiter;

/// Failed sign-ins in a row before the address locks.
pub const LOCK_AFTER: i32 = 10;
/// How long a lock lasts: the first of a day ([`lock_duration`] doubles
/// the next ones).
pub const LOCK_FOR: Duration = Duration::from_mins(15);
/// The longest a lock lasts: the third of a day and every one after it.
pub const MAX_LOCK: Duration = Duration::from_hours(1);
/// Failures older than this are forgotten at the next one.
pub const FORGET_AFTER: Duration = Duration::from_hours(24);

/// The in-memory limits, per instance (one API instance in the MVP).
pub struct Limits {
    /// Password sign-ins per client address.
    pub sign_in: Limiter<IpAddr>,
    /// Requests that send an email (sign-up, forgotten password) per client.
    pub requests: Limiter<IpAddr>,
    /// Emails per recipient: past it, requests are answered as usual and
    /// nothing is sent (no flood of someone's inbox, nothing said).
    pub mails: Limiter<String>,
    /// Emails anyone may make romulus send (sign-ups, resets), in all: a
    /// cap on what a botnet can send from the operator's domain.
    pub outbound: Limiter<()>,
    /// One-time link checks and uses per client.
    pub links: Limiter<IpAddr>,
    /// Password changes per signed-in person.
    pub change: Limiter<Uuid>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            sign_in: Limiter::new(10, Duration::from_secs(3)),
            requests: Limiter::new(5, Duration::from_secs(20)),
            mails: Limiter::new(3, Duration::from_mins(10)),
            outbound: Limiter::new(60, Duration::from_secs(10)),
            links: Limiter::new(30, Duration::from_secs(2)),
            change: Limiter::new(5, Duration::from_mins(1)),
        }
    }
}

/// The client a limit counts against: an IPv4 address, or an IPv6 /48 (a
/// host or a site gets a /64 to a /48: counting addresses, or /64s, would
/// let it rotate through them). Unknown peers (tests, a socket without an
/// address) share one.
pub fn client_key(client: Option<IpAddr>) -> IpAddr {
    match client.map(|ip| ip.to_canonical()) {
        Some(IpAddr::V6(ip)) => {
            let [a, b, c, ..] = ip.segments();
            IpAddr::V6(Ipv6Addr::new(a, b, c, 0, 0, 0, 0, 0))
        }
        Some(ip) => ip,
        None => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
    }
}

/// Until when `email` is locked, if it is.
pub async fn locked_until(
    db: impl PgExecutor<'_>,
    email: &str,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT locked_until AS "locked_until!" FROM sign_in_failures
           WHERE email_key = lower($1 COLLATE pg_unicode_fast) AND locked_until > now()"#,
        email,
    )
    .fetch_optional(db)
    .await
}

/// An attempt, counted.
pub struct Attempt {
    /// Its number in the current series: the [`LOCK_AFTER`]th locks.
    pub number: i32,
    /// Locks of this address since its last success, within a day, this
    /// attempt's included: a high count means someone keeps at it.
    pub locks: i32,
    /// Set when this attempt locked: until then.
    pub locked_until: Option<DateTime<Utc>>,
}

/// How long the `locks`th lock of a day lasts: [`LOCK_FOR`], then 30
/// minutes, then [`MAX_LOCK`] (an hour) for every one after. (The SQL of
/// [`attempt`] says the same; a test keeps them together.)
pub fn lock_duration(locks: i32) -> Duration {
    let doublings = u32::try_from(locks.saturating_sub(1)).unwrap_or(0).min(16);
    LOCK_FOR.saturating_mul(1 << doublings).min(MAX_LOCK)
}

/// Counts an attempt to sign in with `email` before its password is
/// checked, or `None` when the address is locked (refused unchecked). The
/// attempt that reaches [`LOCK_AFTER`] locks the address at once, so however
/// many arrive together, at most [`LOCK_AFTER`] are checked; when the lock
/// runs out a new series starts, and the locks of a day grow from
/// [`LOCK_FOR`] to [`MAX_LOCK`] ([`lock_duration`]): a patient guesser gets
/// ten tries an hour at most, and its owner, back after a success, starts
/// from [`LOCK_FOR`] again. A reset link, or a device that signed in before, works through
/// any lock. A success lifts it ([`clear`]).
pub async fn attempt(db: impl PgExecutor<'_>, email: &str) -> Result<Option<Attempt>, sqlx::Error> {
    let row = sqlx::query!(
        r#"INSERT INTO sign_in_failures AS f (email_key, failures, locks, last_failure_at, locked_until)
           VALUES (lower($1 COLLATE pg_unicode_fast), 1, CASE WHEN 1 >= $3 THEN 1 ELSE 0 END, now(),
                   CASE WHEN 1 >= $3 THEN now() + make_interval(secs => $4) END)
           ON CONFLICT (email_key) DO UPDATE SET
               -- The series: a new one after a day of quiet or a lock that ran out.
               failures = CASE
                   WHEN f.last_failure_at < now() - make_interval(secs => $2)
                        OR f.locked_until IS NOT NULL THEN 1
                   ELSE f.failures + 1 END,
               -- Today's locks: forgotten after a day of quiet.
               locks = CASE WHEN f.last_failure_at < now() - make_interval(secs => $2)
                            THEN 0 ELSE f.locks END
                     + CASE WHEN (CASE
                           WHEN f.last_failure_at < now() - make_interval(secs => $2)
                                OR f.locked_until IS NOT NULL THEN 1
                           ELSE f.failures + 1 END) >= $3 THEN 1 ELSE 0 END,
               last_failure_at = now(),
               -- 15 minutes, 30, then an hour ($5) for every lock after.
               locked_until = CASE
                   WHEN (CASE
                           WHEN f.last_failure_at < now() - make_interval(secs => $2)
                                OR f.locked_until IS NOT NULL THEN 1
                           ELSE f.failures + 1 END) >= $3
                   THEN now() + make_interval(secs => least(
                       $4 * power(2, CASE WHEN f.last_failure_at < now() - make_interval(secs => $2)
                                          THEN 0 ELSE f.locks END),
                       $5)) END
           -- Not while locked: those are refused unchecked, uncounted.
           WHERE f.locked_until IS NULL OR f.locked_until <= now()
           RETURNING failures, locks, locked_until"#,
        email,
        FORGET_AFTER.as_secs_f64(),
        LOCK_AFTER,
        LOCK_FOR.as_secs_f64(),
        MAX_LOCK.as_secs_f64(),
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| Attempt {
        number: row.failures,
        locks: row.locks,
        locked_until: row.locked_until,
    }))
}

/// What a browser's device cookie makes of an attempt.
pub enum DeviceAttempt {
    /// Not a device of the account with this address: the address counts.
    Unknown,
    /// This device guessed wrong too often: locked until then.
    Locked(DateTime<Utc>),
    /// Counted against the device (its locks stay [`LOCK_FOR`]).
    Counted(Attempt),
}

/// Counts an attempt on `email` against the device `token_hash` names, when
/// it is one of that address's account's devices; as [`attempt`] does for
/// addresses (locked at [`LOCK_AFTER`], a new series once the lock runs
/// out, forgotten after [`FORGET_AFTER`]).
pub async fn device_attempt(
    db: &PgPool,
    token_hash: &[u8],
    email: &str,
) -> Result<DeviceAttempt, sqlx::Error> {
    let counted = sqlx::query!(
        r#"UPDATE sign_in_devices d SET
               failures = CASE WHEN d.last_used_at < now() - make_interval(secs => $3)
                                    OR d.locked_until IS NOT NULL
                               THEN 1 ELSE d.failures + 1 END,
               last_used_at = now(),
               locked_until = CASE
                   WHEN (CASE WHEN d.last_used_at < now() - make_interval(secs => $3)
                                   OR d.locked_until IS NOT NULL
                              THEN 1 ELSE d.failures + 1 END) >= $4
                   THEN now() + make_interval(secs => $5) END
           FROM users u
           WHERE d.token_hash = $1 AND u.id = d.user_id AND u.oidc_subject LIKE 'password|%'
             AND lower(u.email COLLATE pg_unicode_fast) = lower($2 COLLATE pg_unicode_fast)
             AND (d.locked_until IS NULL OR d.locked_until <= now())
           RETURNING d.failures, d.locked_until"#,
        token_hash,
        email,
        FORGET_AFTER.as_secs_f64(),
        LOCK_AFTER,
        LOCK_FOR.as_secs_f64(),
    )
    .fetch_optional(db)
    .await?;
    if let Some(row) = counted {
        return Ok(DeviceAttempt::Counted(Attempt {
            number: row.failures,
            locks: 0,
            locked_until: row.locked_until,
        }));
    }
    let locked = sqlx::query_scalar!(
        r#"SELECT d.locked_until AS "locked_until!" FROM sign_in_devices d JOIN users u ON u.id = d.user_id
           WHERE d.token_hash = $1 AND d.locked_until > now()
             AND lower(u.email COLLATE pg_unicode_fast) = lower($2 COLLATE pg_unicode_fast)"#,
        token_hash,
        email,
    )
    .fetch_optional(db)
    .await?;
    Ok(locked.map_or(DeviceAttempt::Unknown, DeviceAttempt::Locked))
}

/// A device's series ends: it signed in.
pub async fn device_clear(db: impl PgExecutor<'_>, token_hash: &[u8]) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE sign_in_devices SET failures = 0, locked_until = NULL, last_used_at = now()
         WHERE token_hash = $1",
        token_hash,
    )
    .execute(db)
    .await
    .map(drop)
}

/// How long a device is remembered unused.
pub const DEVICE_TTL: Duration = Duration::from_hours(180 * 24);
/// Devices remembered per account (the oldest go).
pub const DEVICES_PER_ACCOUNT: i64 = 10;

/// Remembers a new device of `user_id` by the hash of its cookie, keeping
/// the account's [`DEVICES_PER_ACCOUNT`] latest.
pub async fn remember_device(
    db: &PgPool,
    user_id: uuid::Uuid,
    token_hash: &[u8],
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO sign_in_devices (token_hash, user_id) VALUES ($1, $2)",
        token_hash,
        user_id,
    )
    .execute(db)
    .await?;
    sqlx::query!(
        "DELETE FROM sign_in_devices WHERE user_id = $1 AND token_hash NOT IN (
             SELECT token_hash FROM sign_in_devices WHERE user_id = $1
             ORDER BY last_used_at DESC LIMIT $2)",
        user_id,
        DEVICES_PER_ACCOUNT,
    )
    .execute(db)
    .await
    .map(drop)
}

/// Whether `token_hash` is a device of `user_id` (then it is touched).
pub async fn known_device(
    db: &PgPool,
    user_id: uuid::Uuid,
    token_hash: &[u8],
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query!(
        "UPDATE sign_in_devices SET last_used_at = now() WHERE token_hash = $1 AND user_id = $2",
        token_hash,
        user_id,
    )
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

/// Forgets `email`'s failures and lock (a success, a reset, an admin).
/// Whether there was anything to forget.
pub async fn clear(db: impl PgExecutor<'_>, email: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query!(
        "DELETE FROM sign_in_failures WHERE email_key = lower($1 COLLATE pg_unicode_fast)",
        email,
    )
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

/// Rows nobody tried for [`FORGET_AFTER`] and not locked, and devices
/// unused for [`DEVICE_TTL`]: the sweeper's.
pub async fn sweep(db: &PgPool) -> Result<u64, sqlx::Error> {
    let failures = sqlx::query!(
        "DELETE FROM sign_in_failures
         WHERE last_failure_at < now() - make_interval(secs => $1)
           AND (locked_until IS NULL OR locked_until < now())",
        FORGET_AFTER.as_secs_f64(),
    )
    .execute(db)
    .await?
    .rows_affected();
    let devices = sqlx::query!(
        "DELETE FROM sign_in_devices WHERE last_used_at < now() - make_interval(secs => $1)",
        DEVICE_TTL.as_secs_f64(),
    )
    .execute(db)
    .await?
    .rows_affected();
    Ok(failures + devices)
}

/// How long until `until`, at least a second.
pub fn remaining(until: DateTime<Utc>) -> Duration {
    (until - Utc::now())
        .to_std()
        .unwrap_or_default()
        .max(Duration::from_secs(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ipv6_site_counts_as_its_whole_48() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        // Another /64 of the same /48.
        let b: IpAddr = "2001:db8:1:ffff:bbbb::9".parse().unwrap();
        let other: IpAddr = "2001:db8:2:2::1".parse().unwrap();
        assert_eq!(client_key(Some(a)), client_key(Some(b)));
        assert_ne!(client_key(Some(a)), client_key(Some(other)));
        let v4: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(client_key(Some(v4)), v4);
        let mapped: IpAddr = "::ffff:203.0.113.7".parse().unwrap();
        assert_eq!(client_key(Some(mapped)), v4);
    }

    #[test]
    fn locks_grow_to_an_hour_and_stay_there() {
        let minutes = |locks| lock_duration(locks).as_secs() / 60;
        assert_eq!(
            [0, 1, 2, 3, 4, 7, 40].map(minutes),
            [15, 15, 30, 60, 60, 60, 60]
        );
    }
}
