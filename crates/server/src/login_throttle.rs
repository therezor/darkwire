//! What stands between one password and everyone who can reach the port.
//!
//! The rate-limit layer is already on the login at ten attempts a minute, and
//! that limit is keyed by the peer address. Against one host hammering the form
//! it is the right tool and this module would be redundant. Against the attack
//! that is actually run against an internet-facing box it does nothing at all:
//! a botnet with a thousand addresses gets ten thousand guesses a minute and
//! never trips a single bucket, because no individual address ever makes an
//! eleventh request.
//!
//! So there are two scopes here, and they are asymmetric on purpose:
//!
//!  - **Per address**, capped at fifteen minutes. One host that guesses wrong
//!    repeatedly is not making a mistake, and locking it out for a long time
//!    costs nothing that matters.
//!  - **Per account**, capped at thirty seconds. This is the scope the botnet
//!    cannot spread out of — every guess against the single account lands in
//!    the same bucket regardless of where it came from, which caps the
//!    *aggregate* guess rate at roughly two a minute no matter how many
//!    addresses are in play. A twelve-character password is out of reach at
//!    that rate for longer than the universe has been around.
//!
//! The thirty seconds is the whole design, and it is a ceiling rather than an
//! escalation because of what this server is: a single-account install. An
//! account lockout that grows without bound is a denial of service an attacker
//! can trigger *deliberately* — fail four logins an hour and the operator can
//! never sign in again, and there is no second admin to appeal to. A short cap
//! makes the operator's worst case "wait half a minute" while leaving the
//! attacker's throughput just as dead, because the attacker needs millions of
//! guesses and the operator needs one.
//!
//! State is on the shared connection rather than in memory, for the same reason
//! the sessions table is: a counter that resets when the process does is a
//! counter an attacker clears by arranging a restart, and a self-hosted agent
//! that runs out of memory under load restarts on its own.
//!
//! A note on what is *not* here: this never answers "was the username right".
//! Both scopes are recorded on any failed attempt, so the throttle cannot be
//! used to distinguish a wrong name from a wrong password.

use std::sync::Arc;

use darkwire_core::{Clock, Database, Result, RowReader};
use rusqlite::params;

/// The `auth_throttle` table.
pub const AUTH_THROTTLE_TABLE: &str = "CREATE TABLE IF NOT EXISTS auth_throttle (
  scope           TEXT    PRIMARY KEY,
  failures        INTEGER NOT NULL,
  last_failed_ms  INTEGER NOT NULL,
  locked_until_ms INTEGER NOT NULL
) STRICT;";

/// The index the pruning sweep runs on.
pub const AUTH_THROTTLE_LAST_FAILED_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS auth_throttle_last_failed ON auth_throttle(last_failed_ms);";

/// Every statement the throttle runs on construction, in order.
pub const SCHEMA: &[&str] = &[AUTH_THROTTLE_TABLE, AUTH_THROTTLE_LAST_FAILED_INDEX];

const ROWS: RowReader = RowReader::new("auth_throttle");

/// The bucket every attempt lands in, wherever it came from.
pub const ACCOUNT_SCOPE: &str = "account";

/// Failures that cost nothing.
///
/// People mistype passwords, and a form that starts punishing on the second
/// attempt is a form that punishes its owner far more often than an attacker —
/// who is not inconvenienced by the first four guesses either way.
pub const FREE_ATTEMPTS: i64 = 4;

/// The first delay imposed, doubling from there.
const BASE_DELAY_MS: i64 = 1_000;

/// The per-address ceiling. Long, because a single address that has guessed
/// wrong a dozen times is not a person who forgot their password.
pub const MAX_ADDRESS_DELAY_MS: i64 = 15 * 60_000;

/// The account-wide ceiling, and the reason it is nowhere near the one above.
/// See the module header: an unbounded lockout on a single-account server is a
/// denial of service an attacker can trigger on purpose.
pub const MAX_ACCOUNT_DELAY_MS: i64 = 30_000;

/// How long a bucket survives without a new failure.
///
/// Without decay a counter only ever climbs, and an install that has been up
/// for a year is one that locks its owner out on their first typo. An hour of
/// silence is not an attack in progress.
pub const DECAY_MS: i64 = 60 * 60_000;

/// How many address buckets are kept.
///
/// A distributed attack writes one row per address, and the table is on the
/// same file the transcript is. Past the cap the least recently active
/// addresses are dropped — which forgives them, and is safe precisely because
/// the account bucket is the one holding the aggregate rate down and is never
/// evicted.
const MAX_TRACKED_ADDRESSES: i64 = 4096;

/// How long the caller must wait, and which scope is asking them to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThrottleBlock {
    /// Which bucket refused: [`ACCOUNT_SCOPE`], or `ip:<address>`.
    pub scope: String,
    /// How long to wait, in milliseconds.
    pub retry_after_ms: i64,
}

/// `1s`, `2s`, `4s` … capped, and zero for the attempts a person mistypes.
///
/// Public because the delay is the security property: a test that asserts the
/// sequence is asserting on the thing the module exists for, and one that
/// recomputed the formula alongside it would agree with a bug.
pub fn delay_for(failures: i64, max_delay_ms: i64) -> i64 {
    if failures <= FREE_ATTEMPTS {
        return 0;
    }
    // Clamped before the shift rather than after: the caps are well under a
    // minute and an hour, so any exponent past a handful already saturates, and
    // shifting by 63 or more is what would wrap to a negative delay.
    let steps = (failures - FREE_ATTEMPTS - 1).clamp(0, 62);
    BASE_DELAY_MS
        .saturating_mul(1_i64 << steps)
        .min(max_delay_ms)
}

/// The failure counters, on the shared connection.
pub struct LoginThrottle {
    db: Database,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for LoginThrottle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoginThrottle")
    }
}

impl LoginThrottle {
    /// Opens the throttle, creating its table.
    pub fn new(db: Database, clock: Arc<dyn Clock>) -> Result<LoginThrottle> {
        for statement in SCHEMA {
            db.execute_batch(statement)?;
        }
        Ok(LoginThrottle { db, clock })
    }

    /// The block in force for this attempt, or `None` to let it through.
    ///
    /// Called *before* the password is checked, so a locked-out caller never
    /// reaches argon2id — which is the second thing this buys. A key derivation
    /// function tuned to cost 50 ms and 19 MiB is a denial-of-service amplifier
    /// when an anonymous caller can invoke it at will.
    pub fn check(&self, address: &str) -> Result<Option<ThrottleBlock>> {
        let now = self.clock.now_ms();
        // Both scopes, and the longer of them — not the first one that happens
        // to be locked. `Retry-After` is a promise: a caller told to come back
        // in thirty seconds because the account bucket said so, while their own
        // address is locked for fifteen minutes, is a caller who returns on
        // time and is refused again. Answering with the real wait is the
        // difference between a throttle and a lie.
        let account = self.block_for(ACCOUNT_SCOPE, now)?;
        let per_address = self.block_for(&address_scope(address), now)?;
        Ok(longest(account, per_address))
    }

    /// Records a failure against both scopes and returns the block it creates.
    ///
    /// Returning it rather than making the caller check again is what lets a
    /// login answer 429 on the attempt that crossed the line instead of on the
    /// next one: an attacker who gets a 401 back learns the guess was wrong and
    /// is free to send another, and the delay only becomes real when the
    /// response says so.
    pub fn fail(&self, address: &str) -> Result<Option<ThrottleBlock>> {
        let now = self.clock.now_ms();
        let account = self.record(ACCOUNT_SCOPE, now, MAX_ACCOUNT_DELAY_MS)?;
        let per_address = self.record(&address_scope(address), now, MAX_ADDRESS_DELAY_MS)?;
        self.prune(now)?;
        Ok(longest(account, per_address))
    }

    /// Clears both scopes after a login that worked.
    ///
    /// The address is forgiven along with the account, and that is deliberate:
    /// whoever just proved they know the password is entitled to mistype it on
    /// their next four attempts too.
    pub fn succeed(&self, address: &str) -> Result<()> {
        self.db.lock().execute(
            "DELETE FROM auth_throttle WHERE scope IN (?, ?)",
            params![ACCOUNT_SCOPE, address_scope(address)],
        )?;
        Ok(())
    }

    /// For the operator who locked themselves out and has a shell on the host.
    pub fn reset(&self) -> Result<usize> {
        Ok(self.db.lock().execute("DELETE FROM auth_throttle", [])?)
    }

    fn block_for(&self, scope: &str, now: i64) -> Result<Option<ThrottleBlock>> {
        let guard = self.db.lock();
        let mut statement = guard
            .prepare("SELECT last_failed_ms, locked_until_ms FROM auth_throttle WHERE scope = ?")?;
        let mut rows = statement.query(params![scope])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let last_failed_ms = ROWS.int(row, "last_failed_ms")?;
        let locked_until_ms = ROWS.int(row, "locked_until_ms")?;
        // A decayed bucket is not consulted even though the row is still there
        // — pruning is opportunistic, and a lock that outlived its window must
        // not be enforced just because nothing has swept it yet.
        if now - last_failed_ms > DECAY_MS {
            return Ok(None);
        }
        Ok(if locked_until_ms > now {
            Some(ThrottleBlock {
                scope: scope.to_owned(),
                retry_after_ms: locked_until_ms - now,
            })
        } else {
            None
        })
    }

    fn record(&self, scope: &str, now: i64, max_delay_ms: i64) -> Result<Option<ThrottleBlock>> {
        let previous = {
            let guard = self.db.lock();
            let mut statement = guard
                .prepare("SELECT failures, last_failed_ms FROM auth_throttle WHERE scope = ?")?;
            let mut rows = statement.query(params![scope])?;
            match rows.next()? {
                None => 0,
                Some(row) => {
                    let last_failed_ms = ROWS.int(row, "last_failed_ms")?;
                    if now - last_failed_ms > DECAY_MS {
                        0
                    } else {
                        ROWS.int(row, "failures")?
                    }
                }
            }
        };

        let failures = previous + 1;
        let delay = delay_for(failures, max_delay_ms);
        let locked_until = now + delay;

        self.db.lock().execute(
            "INSERT INTO auth_throttle (scope, failures, last_failed_ms, locked_until_ms)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(scope) DO UPDATE SET
               failures = excluded.failures,
               last_failed_ms = excluded.last_failed_ms,
               locked_until_ms = excluded.locked_until_ms",
            params![scope, failures, now, locked_until],
        )?;

        Ok(if delay == 0 {
            None
        } else {
            Some(ThrottleBlock {
                scope: scope.to_owned(),
                retry_after_ms: delay,
            })
        })
    }

    /// Drops decayed buckets, then the oldest addresses past the cap.
    ///
    /// Only on failure, which is the only path that grows the table, and which
    /// is itself throttled by everything above.
    fn prune(&self, now: i64) -> Result<()> {
        let guard = self.db.lock();
        guard.execute(
            "DELETE FROM auth_throttle WHERE scope <> ? AND last_failed_ms < ?",
            params![ACCOUNT_SCOPE, now - DECAY_MS],
        )?;
        // `LIMIT -1 OFFSET n` is SQLite's spelling of "everything after the
        // first n rows", which here is every address past the cap once they are
        // ordered most recently active first.
        guard.execute(
            "DELETE FROM auth_throttle WHERE scope IN (
               SELECT scope FROM auth_throttle WHERE scope <> ?
               ORDER BY last_failed_ms DESC LIMIT -1 OFFSET ?
             )",
            params![ACCOUNT_SCOPE, MAX_TRACKED_ADDRESSES],
        )?;
        Ok(())
    }
}

/// Prefixed, so an address can never collide with [`ACCOUNT_SCOPE`].
///
/// The address is the socket peer — the server does not trust a forwarding
/// header — so it is not attacker-supplied, but the prefix costs nothing and
/// makes that a property of this module rather than of a setting somewhere
/// else.
fn address_scope(address: &str) -> String {
    format!("ip:{address}")
}

/// The block a caller is actually subject to.
///
/// Both scopes refuse independently, so the wait is the longer of them — being
/// released by one while the other still holds is not being released.
fn longest(left: Option<ThrottleBlock>, right: Option<ThrottleBlock>) -> Option<ThrottleBlock> {
    match (left, right) {
        (Some(a), Some(b)) => Some(if b.retry_after_ms > a.retry_after_ms {
            b
        } else {
            a
        }),
        (Some(block), None) | (None, Some(block)) => Some(block),
        (None, None) => None,
    }
}
