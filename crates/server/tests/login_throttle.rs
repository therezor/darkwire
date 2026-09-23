//! The two asymmetric scopes that stand between one password and a botnet.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use darkwire_core::testkit::ManualClock;
use darkwire_core::{Clock, Database};
use darkwire_server::login_throttle::{
    ACCOUNT_SCOPE, Admission, DECAY_MS, FREE_ATTEMPTS, LoginThrottle, MAX_ACCOUNT_DELAY_MS,
    MAX_ADDRESS_DELAY_MS, SCHEMA, delay_for,
};
use serde_json::Value;

const NOW: i64 = 1_700_000_000_000;

struct Built {
    throttle: LoginThrottle,
    clock: Arc<ManualClock>,
    db: Database,
}

fn build() -> Built {
    let db = Database::in_memory().unwrap();
    let clock = Arc::new(ManualClock::at(NOW));
    let throttle = LoginThrottle::new(db.clone(), Arc::clone(&clock) as Arc<dyn Clock>).unwrap();
    Built {
        throttle,
        clock,
        db,
    }
}

/// Fails `count` times from one address, ignoring the blocks that result.
fn fail_times(throttle: &LoginThrottle, address: &str, count: usize) {
    for _ in 0..count {
        throttle.fail(address).unwrap();
    }
}

fn advance(clock: &ManualClock, ms: i64) {
    clock.advance(Duration::from_millis(u64::try_from(ms).unwrap()));
}

// delay_for

#[test]
fn nothing_is_charged_for_the_attempts_a_person_actually_mistypes() {
    for failures in 1..=FREE_ATTEMPTS {
        assert_eq!(delay_for(failures, MAX_ADDRESS_DELAY_MS), 0, "{failures}");
    }
    // And zero or a negative count, which no caller produces but the formula
    // must still answer for.
    assert_eq!(delay_for(0, MAX_ADDRESS_DELAY_MS), 0);
}

#[test]
fn the_delay_doubles_from_one_second() {
    assert_eq!(delay_for(FREE_ATTEMPTS + 1, MAX_ADDRESS_DELAY_MS), 1_000);
    assert_eq!(delay_for(FREE_ATTEMPTS + 2, MAX_ADDRESS_DELAY_MS), 2_000);
    assert_eq!(delay_for(FREE_ATTEMPTS + 3, MAX_ADDRESS_DELAY_MS), 4_000);
    assert_eq!(delay_for(FREE_ATTEMPTS + 4, MAX_ADDRESS_DELAY_MS), 8_000);
}

/// The exponent runs away long before the cap matters, and a shift that wrapped
/// would produce a *negative* delay — which is no delay at all.
#[test]
fn the_delay_clamps_at_the_cap_rather_than_wrapping() {
    assert_eq!(delay_for(1_000, MAX_ADDRESS_DELAY_MS), MAX_ADDRESS_DELAY_MS);
    assert_eq!(
        delay_for(i64::MAX, MAX_ACCOUNT_DELAY_MS),
        MAX_ACCOUNT_DELAY_MS
    );
    assert_eq!(
        delay_for(i64::MAX, MAX_ADDRESS_DELAY_MS),
        MAX_ADDRESS_DELAY_MS
    );
}

// the per-address scope

#[test]
fn a_fresh_caller_is_let_through() {
    assert_eq!(build().throttle.check("10.0.0.1").unwrap(), None);
}

#[test]
fn the_block_is_reported_on_the_attempt_that_creates_it_not_the_next_one() {
    let built = build();
    for _ in 0..FREE_ATTEMPTS {
        assert_eq!(built.throttle.fail("10.0.0.1").unwrap(), None);
    }
    assert_eq!(
        built
            .throttle
            .fail("10.0.0.1")
            .unwrap()
            .unwrap()
            .retry_after_ms,
        1_000
    );
}

#[test]
fn an_attempt_is_counted_before_its_password_is_checked() {
    // Guesses sent together, each still inside its 50 ms of argon2. The ones
    // past the free attempts meet the lock the earlier ones wrote, rather than
    // all passing a check that nothing had been counted against yet.
    let built = build();
    for attempt in 1..=FREE_ATTEMPTS {
        assert_eq!(
            built.throttle.admit("10.0.0.1").unwrap(),
            Admission::Admitted(None),
            "{attempt}"
        );
    }
    let Admission::Admitted(Some(created)) = built.throttle.admit("10.0.0.1").unwrap() else {
        panic!("the attempt past the free ones is let through with the lock it created");
    };
    assert_eq!(created.retry_after_ms, 1_000);
    let Admission::Refused(block) = built.throttle.admit("10.0.0.2").unwrap() else {
        panic!("the next guess, from anywhere, is refused before it is hashed");
    };
    assert_eq!(block.scope, ACCOUNT_SCOPE);
}

#[test]
fn a_right_password_on_the_attempt_that_created_the_lock_still_clears_it() {
    let built = build();
    for _ in 0..=FREE_ATTEMPTS {
        built.throttle.admit("10.0.0.1").unwrap();
    }
    built.throttle.succeed("10.0.0.1").unwrap();
    assert_eq!(built.throttle.check("10.0.0.1").unwrap(), None);
}

#[test]
fn failures_at_the_same_moment_are_all_counted() {
    let built = build();
    let throttle = Arc::new(built.throttle);
    let threads: Vec<_> = (0..8)
        .map(|index| {
            let throttle = Arc::clone(&throttle);
            std::thread::spawn(move || {
                for _ in 0..4 {
                    throttle.fail(&format!("10.0.1.{index}")).unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let failures: i64 = built
        .db
        .lock()
        .query_row(
            "SELECT failures FROM auth_throttle WHERE scope = ?",
            [ACCOUNT_SCOPE],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(failures, 32);
}

#[test]
fn a_caller_is_refused_until_the_delay_has_passed_then_allowed_again() {
    let built = build();
    fail_times(&built.throttle, "10.0.0.1", 5);

    assert!(built.throttle.check("10.0.0.1").unwrap().is_some());
    advance(&built.clock, 999);
    assert!(built.throttle.check("10.0.0.1").unwrap().is_some());
    advance(&built.clock, 2);
    assert_eq!(built.throttle.check("10.0.0.1").unwrap(), None);
}

#[test]
fn one_address_is_never_held_longer_than_its_cap() {
    let built = build();
    fail_times(&built.throttle, "10.0.0.1", 40);

    let block = built.throttle.check("10.0.0.1").unwrap().unwrap();
    assert!(block.retry_after_ms <= MAX_ADDRESS_DELAY_MS);
    assert_eq!(block.scope, "ip:10.0.0.1");
}

#[test]
fn a_bucket_that_has_gone_quiet_for_the_decay_window_is_forgotten() {
    let built = build();
    fail_times(&built.throttle, "10.0.0.1", 12);
    assert!(built.throttle.check("10.0.0.1").unwrap().is_some());

    advance(&built.clock, DECAY_MS + 1);
    assert_eq!(built.throttle.check("10.0.0.1").unwrap(), None);

    // And the counter restarted rather than resuming where it left off: the
    // next four failures are free again.
    assert_eq!(built.throttle.fail("10.0.0.1").unwrap(), None);
}

#[test]
fn a_login_that_worked_forgives_both_scopes() {
    let built = build();
    fail_times(&built.throttle, "10.0.0.1", 7);
    assert!(built.throttle.check("10.0.0.1").unwrap().is_some());

    built.throttle.succeed("10.0.0.1").unwrap();
    assert_eq!(built.throttle.check("10.0.0.1").unwrap(), None);
    assert_eq!(built.throttle.check("10.0.0.2").unwrap(), None);
}

// the account scope

/// The reason this module exists.
///
/// Every address here is used exactly once, so no per-address bucket ever
/// reaches its free allowance and a per-address rate limit would see nothing at
/// all. The account bucket is what notices.
#[test]
fn a_distributed_attack_no_per_address_bucket_would_catch_is_caught() {
    let built = build();
    for index in 0..=FREE_ATTEMPTS {
        built.throttle.fail(&format!("10.0.0.{index}")).unwrap();
    }

    // A brand new address, which has never failed and is refused anyway.
    let block = built.throttle.check("203.0.113.7").unwrap().unwrap();
    assert_eq!(block.scope, ACCOUNT_SCOPE);
    assert!(block.retry_after_ms > 0);
}

/// An unbounded account lockout on a single-account server is a denial of
/// service an attacker can trigger on purpose.
#[test]
fn the_account_cap_is_far_below_the_per_address_ceiling() {
    let built = build();
    for index in 0..100 {
        built.throttle.fail(&format!("10.0.0.{index}")).unwrap();
    }

    let block = built.throttle.check("203.0.113.7").unwrap().unwrap();
    assert!(block.retry_after_ms <= MAX_ACCOUNT_DELAY_MS);
    // The two caps are deliberately different orders of magnitude: an address
    // can be shared by a whole office, so locking one out for fifteen minutes
    // has to be worth more than locking the single account out for thirty
    // seconds. `const_assert`-style comparison, stated as a runtime check so it
    // reads beside the assertion it explains.
    assert_eq!(
        MAX_ACCOUNT_DELAY_MS.min(MAX_ADDRESS_DELAY_MS),
        MAX_ACCOUNT_DELAY_MS
    );
}

/// Being released by one scope while the other still holds is not being
/// released, so the wait reported is the longer of the two.
#[test]
fn the_longer_of_the_two_blocks_is_the_one_reported() {
    let built = build();
    // One address doing all the failing drives its own bucket past the account
    // cap, so the address block is the one that has to be reported.
    fail_times(&built.throttle, "10.0.0.1", 20);

    let block = built.throttle.check("10.0.0.1").unwrap().unwrap();
    assert!(block.retry_after_ms > MAX_ACCOUNT_DELAY_MS);
    assert_eq!(block.scope, "ip:10.0.0.1");
}

#[test]
fn the_account_block_is_reported_when_it_is_the_longer_one() {
    let built = build();
    // Five failures spread over five addresses: each address bucket is at one
    // failure and free, the account bucket is at five and locked.
    for index in 0..5 {
        built.throttle.fail(&format!("10.0.0.{index}")).unwrap();
    }
    let block = built.throttle.check("10.0.0.0").unwrap().unwrap();
    assert_eq!(block.scope, ACCOUNT_SCOPE);
}

// persistence and pruning

/// An in-memory counter would hand the attacker a clean slate on a restart, and
/// a self-hosted agent that runs out of memory under load restarts on its own.
#[test]
fn the_counters_survive_a_restart_on_the_same_database() {
    let built = build();
    fail_times(&built.throttle, "10.0.0.1", 8);
    assert!(built.throttle.check("10.0.0.1").unwrap().is_some());

    let restarted =
        LoginThrottle::new(built.db.clone(), Arc::clone(&built.clock) as Arc<dyn Clock>).unwrap();
    assert!(restarted.check("10.0.0.1").unwrap().is_some());
}

#[test]
fn decayed_address_rows_are_dropped_without_touching_the_account_row() {
    let built = build();
    fail_times(&built.throttle, "10.0.0.1", 3);
    advance(&built.clock, DECAY_MS + 1);
    // Any failure prunes; this one is from a different address.
    built.throttle.fail("10.0.0.2").unwrap();

    let guard = built.db.lock();
    let mut statement = guard
        .prepare("SELECT scope FROM auth_throttle ORDER BY scope")
        .unwrap();
    let scopes: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(scopes, [ACCOUNT_SCOPE.to_owned(), "ip:10.0.0.2".to_owned()]);
}

#[test]
fn everything_can_be_cleared_for_an_operator_who_locked_themselves_out() {
    let built = build();
    fail_times(&built.throttle, "10.0.0.1", 20);

    assert!(built.throttle.reset().unwrap() > 0);
    assert_eq!(built.throttle.check("10.0.0.1").unwrap(), None);
    assert_eq!(built.throttle.reset().unwrap(), 0);
}

// schema parity with what the TypeScript store wrote

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sqlite")
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixtures().join(name)).unwrap()).unwrap()
}

#[test]
fn no_ddl_carries_a_comment() {
    // SQLite stores the text verbatim and `DROP COLUMN` rewrites it by byte
    // offset; a comment inside a column list can leave the schema unreadable.
    for ddl in SCHEMA {
        assert!(!ddl.contains("--"), "comment in DDL: {ddl}");
    }
}

#[test]
fn a_fresh_database_stores_the_same_schema_text_the_typescript_store_did() {
    let built = build();

    let expected: Vec<Value> = fixture_json("sqlite_master.json")["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["tbl_name"] == "auth_throttle")
        .cloned()
        .collect();
    assert_eq!(expected.len(), 2, "{expected:#?}");

    let guard = built.db.lock();
    let mut statement = guard
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_master
              WHERE sql IS NOT NULL AND tbl_name = 'auth_throttle' ORDER BY name",
        )
        .unwrap();
    let actual: Vec<Value> = statement
        .query_map([], |row| {
            Ok(serde_json::json!({
                "type": row.get::<_, String>("type")?,
                "name": row.get::<_, String>("name")?,
                "tbl_name": row.get::<_, String>("tbl_name")?,
                "sql": row.get::<_, String>("sql")?,
            }))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn every_statement_the_typescript_constructor_ran_is_one_this_code_runs() {
    let ddl = fixture_json("ddl.json");
    let mut expected: Vec<String> = Vec::new();
    for entry in ddl["statements"].as_array().unwrap() {
        if entry["store"] == "LoginThrottle" {
            expected.extend(
                entry["sql"]
                    .as_str()
                    .unwrap()
                    .split(';')
                    .map(str::trim)
                    .filter(|statement| !statement.is_empty())
                    .map(str::to_owned),
            );
        }
    }

    let actual: Vec<String> = SCHEMA
        .iter()
        .map(|ddl| ddl.trim().trim_end_matches(';').to_owned())
        .collect();
    assert_eq!(actual, expected);
}
