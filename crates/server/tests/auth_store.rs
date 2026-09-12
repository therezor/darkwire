//! Passwords, setup codes, session tokens and named secrets.
//!
//! The schema assertions read `fixtures/sqlite`, which the TypeScript suite
//! wrote: `sqlite_master.json` is what SQLite stored after every store
//! initialised a fresh file, and `ddl.json` is every statement the constructors
//! executed. A database an existing install already has must keep opening, so
//! the text has to match byte for byte rather than merely mean the same thing.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use ghostai_core::testkit::ManualClock;
use ghostai_core::{Clock, Database, ErrorKind, Result};
use ghostai_security::random::RandomSource;
use ghostai_server::auth_store::{
    Argon2Hasher, AuthStore, AuthStoreOptions, PasswordHasher, SCHEMA,
};
use rusqlite::params;
use serde_json::Value;

const NOW: i64 = 1_700_000_000_000;

/// A PHC string produced by the TypeScript implementation this replaces, for
/// the password below. An install that upgrades in place must keep signing in,
/// so this is the one assertion that cannot be regenerated: if it stops
/// verifying, every existing password has been invalidated.
const TYPESCRIPT_PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$hyQQzdDdtosV4OWpwpmmJQ$\
                              ps0BAmfkdkhd/Zgn4NdhfGb6DRWjxVAxwHakrWAH/HM";
/// The password `TYPESCRIPT_PHC` was made from.
const TYPESCRIPT_PASSWORD: &str = "correct horse battery staple";

// fixtures

/// Cheap and reversible, so a test can assert *what* was hashed.
struct FakeHasher;

impl PasswordHasher for FakeHasher {
    fn hash(&self, password: &str) -> Result<String> {
        Ok(format!("fake:{password}"))
    }

    fn verify(&self, hash: &str, password: &str) -> bool {
        hash == format!("fake:{password}")
    }
}

/// The same, counting both halves: the username-oracle property is "the key
/// derivation function ran", and only a counter can see that.
#[derive(Default)]
struct CountingHasher {
    hashes: AtomicUsize,
    verifications: AtomicUsize,
}

impl PasswordHasher for CountingHasher {
    fn hash(&self, password: &str) -> Result<String> {
        self.hashes.fetch_add(1, Ordering::SeqCst);
        Ok(format!("fake:{password}"))
    }

    fn verify(&self, hash: &str, password: &str) -> bool {
        self.verifications.fetch_add(1, Ordering::SeqCst);
        hash == format!("fake:{password}")
    }
}

/// Distinct per call, so two tokens never collide, and reproducible.
struct CountingRandom(AtomicU8);

impl RandomSource for CountingRandom {
    fn fill(&self, buf: &mut [u8]) {
        let previous = self.0.fetch_add(1, Ordering::SeqCst);
        buf.fill(previous.wrapping_add(1));
    }
}

struct Built {
    store: AuthStore,
    clock: Arc<ManualClock>,
    db: Database,
}

fn build(ttl_ms: i64, hasher: Arc<dyn PasswordHasher>) -> Built {
    let db = Database::in_memory().unwrap();
    let clock = Arc::new(ManualClock::at(NOW));
    let store = AuthStore::new(AuthStoreOptions {
        db: db.clone(),
        session_ttl_ms: ttl_ms,
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        random: Arc::new(CountingRandom(AtomicU8::new(0))),
        hasher,
    })
    .unwrap();
    Built { store, clock, db }
}

fn fake() -> Built {
    build(60_000, Arc::new(FakeHasher))
}

// passwords

#[test]
fn no_password_is_reported_before_one_is_set() {
    assert!(!fake().store.has_password().unwrap());
}

#[test]
fn the_password_it_was_given_is_accepted_and_nothing_else() {
    let built = fake();
    built.store.set_password("correct horse", None).unwrap();

    assert!(built.store.has_password().unwrap());
    assert!(built.store.verify_password("correct horse").unwrap());
    assert!(!built.store.verify_password("correct horse ").unwrap());
    assert!(!built.store.verify_password("").unwrap());
}

#[test]
fn a_password_below_the_minimum_is_refused_rather_than_hashed() {
    let built = fake();
    for password in ["", "short"] {
        let error = built.store.set_password(password, None).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert!(
            error.message.contains("at least 12 characters"),
            "{password:?}"
        );
    }
    assert!(!built.store.has_password().unwrap());
}

#[test]
fn a_password_above_the_maximum_is_refused_because_it_is_a_work_ceiling() {
    let built = fake();
    let error = built
        .store
        .set_password(&"a".repeat(257), None)
        .unwrap_err();
    assert!(error.message.contains("at most 256 characters"));
}

/// The bound is counted the way the schema on the other side of the wire counts
/// it, so a password of astral-plane characters is accepted or refused
/// identically by both.
#[test]
fn the_length_bound_counts_utf16_units_not_characters() {
    let built = fake();
    // Six characters, twelve UTF-16 units.
    assert!(built.store.set_password("😀😀😀😀😀😀", None).is_ok());
}

#[test]
fn verification_fails_when_no_password_is_set() {
    assert!(!fake().store.verify_password("anything").unwrap());
}

/// The reason to change a password is that the old one may be known. A token
/// minted under it outliving the change makes the change cosmetic.
#[test]
fn every_session_is_revoked_when_the_password_is_rotated() {
    let built = fake();
    built.store.set_password("first password", None).unwrap();
    let token = built.store.issue("").unwrap().token;
    assert!(built.store.verify(&token).unwrap().is_some());

    built.store.set_password("second password", None).unwrap();
    assert!(built.store.verify(&token).unwrap().is_none());
}

#[test]
fn the_stored_hash_is_replaced_rather_than_a_second_row_added() {
    let built = fake();
    built.store.set_password("first password", None).unwrap();
    built.store.set_password("second password", None).unwrap();

    assert!(!built.store.verify_password("first password").unwrap());
    assert!(built.store.verify_password("second password").unwrap());
}

// the username

#[test]
fn the_username_is_the_default_until_one_is_set() {
    assert_eq!(fake().store.username().unwrap(), "ghost");
}

#[test]
fn the_username_moves_only_alongside_a_password() {
    let built = fake();
    built
        .store
        .set_password("a good password", Some("Operator"))
        .unwrap();
    // Normalised on the way in, so the value stored is the value compared.
    assert_eq!(built.store.username().unwrap(), "operator");
}

#[test]
fn the_username_is_left_alone_when_a_rotation_does_not_name_one() {
    let built = fake();
    built
        .store
        .set_password("a good password", Some("operator"))
        .unwrap();
    built.store.set_password("another password", None).unwrap();

    assert_eq!(built.store.username().unwrap(), "operator");
}

#[test]
fn a_name_the_login_route_would_have_refused_is_refused_here_too() {
    let built = fake();
    let error = built
        .store
        .set_password("a good password", Some("has spaces"))
        .unwrap_err();
    assert!(error.message.to_lowercase().contains("username"));
    // Nothing is written on a refusal — a password stored beside a rejected
    // name would be a credential half-applied.
    assert!(!built.store.has_password().unwrap());
    assert_eq!(built.store.username().unwrap(), "ghost");
}

#[test]
fn a_password_that_is_the_username_is_refused() {
    let built = fake();
    let error = built
        .store
        .set_password("operatorname", Some("operatorname"))
        .unwrap_err();
    assert!(error.message.contains("must not be the username"));
}

#[test]
fn the_username_is_neither_handed_out_nor_generated_by_ensure_secret() {
    let built = fake();
    let error = built.store.ensure_secret("username").unwrap_err();
    assert!(error.message.contains("not a readable secret"));
    assert_eq!(built.store.username().unwrap(), "ghost");
}

// verify_login

#[test]
fn a_login_takes_both_halves_and_nothing_less() {
    let built = fake();
    built
        .store
        .set_password("a good password", Some("operator"))
        .unwrap();

    assert!(
        built
            .store
            .verify_login("operator", "a good password")
            .unwrap()
    );
    assert!(!built.store.verify_login("operator", "wrong").unwrap());
    assert!(
        !built
            .store
            .verify_login("ghost", "a good password")
            .unwrap()
    );
}

#[test]
fn the_case_and_surrounding_space_of_the_name_are_folded_and_only_the_name() {
    let built = fake();
    built
        .store
        .set_password("a good password", Some("operator"))
        .unwrap();

    assert!(
        built
            .store
            .verify_login("  OPERATOR ", "a good password")
            .unwrap()
    );
    assert!(
        !built
            .store
            .verify_login("operator", " a good password")
            .unwrap()
    );
}

#[test]
fn a_name_too_malformed_to_normalise_is_refused_without_failing() {
    let built = fake();
    built
        .store
        .set_password("a good password", Some("operator"))
        .unwrap();

    // The route would have rejected these at the schema, but the store is also
    // reachable from a test, a channel and anything added later. A failure here
    // would be a 500 where a 401 belongs.
    for name in ["", "has spaces", "-leading-dash"] {
        assert!(
            !built.store.verify_login(name, "a good password").unwrap(),
            "{name:?}"
        );
    }
}

/// The property that stops the form being a username oracle.
///
/// A wrong name must cost the same as a wrong password, which means the key
/// derivation function has to run either way. A `verify_login` that returned
/// early on the name would show up here as a call that never happened.
#[test]
fn the_hasher_runs_even_when_the_username_is_wrong() {
    let counting = Arc::new(CountingHasher::default());
    let built = build(60_000, Arc::clone(&counting) as Arc<dyn PasswordHasher>);
    built
        .store
        .set_password("a good password", Some("operator"))
        .unwrap();

    counting.verifications.store(0, Ordering::SeqCst);
    assert!(
        !built
            .store
            .verify_login("nobody", "a good password")
            .unwrap()
    );
    assert_eq!(counting.verifications.load(Ordering::SeqCst), 1);
}

#[test]
fn the_hasher_runs_even_when_no_password_is_set_at_all() {
    let counting = Arc::new(CountingHasher::default());
    let built = build(60_000, Arc::clone(&counting) as Arc<dyn PasswordHasher>);

    // An unclaimed install must not answer faster than a claimed one, or the
    // difference tells an attacker which servers are worth coming back to.
    assert!(!built.store.verify_login("ghost", "anything").unwrap());
    assert_eq!(counting.verifications.load(Ordering::SeqCst), 1);
}

/// The decoy exists so an unclaimed install costs what a claimed one does, and
/// it is computed lazily so a server that never sees such a login never pays
/// the 50 ms. Both halves of that are visible only as counts.
#[test]
fn the_decoy_is_hashed_once_however_many_logins_fail_against_an_unclaimed_install() {
    let counting = Arc::new(CountingHasher::default());
    let built = build(60_000, Arc::clone(&counting) as Arc<dyn PasswordHasher>);

    for _ in 0..5 {
        assert!(!built.store.verify_login("ghost", "anything").unwrap());
    }
    assert_eq!(counting.verifications.load(Ordering::SeqCst), 5);
    assert_eq!(counting.hashes.load(Ordering::SeqCst), 1);
}

#[test]
fn the_decoy_is_not_computed_until_a_login_needs_it() {
    let counting = Arc::new(CountingHasher::default());
    let built = build(60_000, Arc::clone(&counting) as Arc<dyn PasswordHasher>);

    built.store.issue("web").unwrap();
    built.store.has_password().unwrap();
    assert_eq!(counting.hashes.load(Ordering::SeqCst), 0);
}

// argon2id: the real hasher, and compatibility with what is already on disk

/// The assertion that says an existing install upgrades in place.
#[test]
fn a_digest_written_by_the_typescript_implementation_still_verifies() {
    assert!(Argon2Hasher.verify(TYPESCRIPT_PHC, TYPESCRIPT_PASSWORD));
    assert!(!Argon2Hasher.verify(TYPESCRIPT_PHC, "wrong"));
    assert!(!Argon2Hasher.verify(TYPESCRIPT_PHC, ""));
}

#[test]
fn a_freshly_minted_digest_carries_the_owasp_baseline_parameters() {
    let digest = Argon2Hasher.hash("a real password").unwrap();
    assert!(
        digest.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
        "{digest}"
    );
    assert!(Argon2Hasher.verify(&digest, "a real password"));
    assert!(!Argon2Hasher.verify(&digest, "a real passwore"));
}

#[test]
fn the_hasher_salts_so_the_same_password_twice_is_two_digests() {
    let first = Argon2Hasher.hash("same").unwrap();
    let second = Argon2Hasher.hash("same").unwrap();
    assert_ne!(first, second);
}

#[test]
fn a_corrupt_stored_value_is_a_failed_verification_not_a_failure() {
    for stored in [
        "not an argon2 encoding",
        "",
        "$argon2id$v=19$m=19456,t=2,p=1$notbase64$notbase64",
    ] {
        assert!(!Argon2Hasher.verify(stored, "password"), "{stored:?}");
    }
}

/// The store defaults to the real hasher, and a password set through it comes
/// back out as an argon2id encoding rather than anything cheaper.
#[test]
fn the_default_wiring_is_argon2id() {
    let db = Database::in_memory().unwrap();
    let store = AuthStore::new(AuthStoreOptions::new(db.clone(), 60_000)).unwrap();
    store.set_password("a real password", None).unwrap();

    let stored: String = db
        .lock()
        .query_row(
            "SELECT value FROM auth_secrets WHERE name = 'password'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        stored.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
        "{stored}"
    );
    assert!(store.verify_password("a real password").unwrap());
}

// tokens

#[test]
fn a_token_it_issued_verifies() {
    let built = fake();
    let issued = built.store.issue("web").unwrap();

    let session = built.store.verify(&issued.token).unwrap().unwrap();
    assert_eq!(session.id, issued.id);
    assert_eq!(session.label, "web");
    assert_eq!(session.expires_at_ms, NOW + 60_000);
    assert_eq!(session.created_at_ms, NOW);
}

#[test]
fn issued_tokens_are_distinct() {
    let built = fake();
    assert_ne!(
        built.store.issue("").unwrap().token,
        built.store.issue("").unwrap().token
    );
}

#[test]
fn a_malformed_token_is_refused() {
    let built = fake();
    for token in ["", "abcdef", ".secret", "id.", "nosuchid.secret"] {
        assert!(built.store.verify(token).unwrap().is_none(), "{token:?}");
    }
}

/// The digest comparison is the check that matters: an id is a row address, and
/// knowing one must not be enough to pass.
#[test]
fn a_real_id_with_the_wrong_secret_is_refused() {
    let built = fake();
    let issued = built.store.issue("").unwrap();

    assert!(
        built
            .store
            .verify(&format!("{}.wrong", issued.id))
            .unwrap()
            .is_none()
    );
    assert!(
        built
            .store
            .verify(&format!("{}.", issued.id))
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_token_past_its_expiry_is_refused_and_its_row_is_dropped() {
    let built = build(1_000, Arc::new(FakeHasher));
    let issued = built.store.issue("").unwrap();

    built.clock.advance(Duration::from_secs(1));
    assert!(built.store.verify(&issued.token).unwrap().is_none());
    let remaining: i64 = built
        .db
        .lock()
        .query_row("SELECT COUNT(*) FROM auth_sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(remaining, 0);
}

/// A stored digest of the wrong length must not turn a bad credential into a
/// failure the caller sees as a 500.
#[test]
fn a_stored_digest_of_the_wrong_length_is_refused() {
    let built = fake();
    let issued = built.store.issue("").unwrap();
    built
        .db
        .lock()
        .execute(
            "UPDATE auth_sessions SET token_sha256 = ?",
            params![vec![0u8; 8]],
        )
        .unwrap();

    assert!(built.store.verify(&issued.token).unwrap().is_none());
}

#[test]
fn one_session_is_revoked_without_touching_the_others() {
    let built = fake();
    let first = built.store.issue("").unwrap();
    let second = built.store.issue("").unwrap();

    assert!(built.store.revoke_by_id(&first.id).unwrap());
    assert!(!built.store.revoke_by_id(&first.id).unwrap());
    assert!(built.store.verify(&first.token).unwrap().is_none());
    assert!(built.store.verify(&second.token).unwrap().is_some());
}

#[test]
fn every_session_is_revoked_at_once() {
    let built = fake();
    built.store.issue("").unwrap();
    built.store.issue("").unwrap();

    assert_eq!(built.store.revoke_all().unwrap(), 2);
    assert_eq!(built.store.revoke_all().unwrap(), 0);
}

#[test]
fn purging_drops_only_what_has_expired() {
    let built = build(1_000, Arc::new(FakeHasher));
    let early = built.store.issue("").unwrap();
    built.clock.advance(Duration::from_millis(600));
    let late = built.store.issue("").unwrap();

    // 1100ms: `early` is dead, `late` has 500ms left.
    built.clock.advance(Duration::from_millis(500));
    assert_eq!(built.store.purge_expired().unwrap(), 1);
    assert!(built.store.verify(&early.token).unwrap().is_none());
    assert!(built.store.verify(&late.token).unwrap().is_some());
}

// last seen

fn last_seen(db: &Database) -> i64 {
    db.lock()
        .query_row("SELECT last_seen_at_ms FROM auth_sessions", [], |row| {
            row.get(0)
        })
        .unwrap()
}

/// A read that writes on every request would put every authenticated `GET` in
/// the same write-ahead log a turn is streaming into.
#[test]
fn a_verification_moments_after_the_last_one_does_not_write() {
    let built = build(600_000, Arc::new(FakeHasher));
    let issued = built.store.issue("").unwrap();
    let before = last_seen(&built.db);

    built.clock.advance(Duration::from_secs(30));
    built.store.verify(&issued.token).unwrap();
    assert_eq!(last_seen(&built.db), before);
}

#[test]
fn a_verification_writes_once_the_record_is_stale() {
    let built = build(600_000, Arc::new(FakeHasher));
    let issued = built.store.issue("").unwrap();

    built.clock.advance(Duration::from_secs(61));
    built.store.verify(&issued.token).unwrap();
    assert_eq!(last_seen(&built.db), built.clock.now_ms());
}

// named secrets

#[test]
fn a_named_secret_is_created_on_first_use_and_stable_afterwards() {
    let built = fake();
    let first = built.store.ensure_secret("media_signing_key").unwrap();
    assert!(!first.is_empty());
    assert_eq!(
        built.store.ensure_secret("media_signing_key").unwrap(),
        first
    );
}

#[test]
fn the_one_way_secrets_are_refused_by_name() {
    let built = fake();
    built.store.issue_setup_code().unwrap();
    for name in ["password", "setup_code", "username"] {
        let error = built.store.ensure_secret(name).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert!(error.message.contains("not a readable secret"), "{name}");
    }
}

// the shared connection

#[test]
fn the_tables_are_created_on_a_connection_something_else_already_opened() {
    let db = Database::in_memory().unwrap();
    db.execute_batch("CREATE TABLE unrelated (id TEXT PRIMARY KEY) STRICT")
        .unwrap();

    let store = AuthStore::new(AuthStoreOptions {
        db: db.clone(),
        session_ttl_ms: 1_000,
        clock: Arc::new(ManualClock::at(NOW)),
        random: Arc::new(CountingRandom(AtomicU8::new(0))),
        hasher: Arc::new(FakeHasher),
    })
    .unwrap();
    let token = store.issue("").unwrap().token;

    assert!(store.verify(&token).unwrap().is_some());
    let kept: i64 = db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'unrelated'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kept, 1);
}

#[test]
fn a_second_store_on_the_same_connection_is_fine() {
    let built = fake();
    let issued = built.store.issue("").unwrap();
    let second = AuthStore::new(AuthStoreOptions {
        db: built.db.clone(),
        session_ttl_ms: 1_000,
        clock: Arc::clone(&built.clock) as Arc<dyn Clock>,
        random: Arc::new(CountingRandom(AtomicU8::new(0))),
        hasher: Arc::new(FakeHasher),
    })
    .unwrap();

    assert!(second.verify(&issued.token).unwrap().is_some());
}

// setup codes

/// Randomness that walks the alphabet, so the grouping is visible rather than
/// twelve of the same character.
struct WalkingRandom(AtomicU8);

impl RandomSource for WalkingRandom {
    fn fill(&self, buf: &mut [u8]) {
        for slot in buf.iter_mut() {
            *slot = self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

fn with_walking_random() -> Built {
    let db = Database::in_memory().unwrap();
    let clock = Arc::new(ManualClock::at(NOW));
    let store = AuthStore::new(AuthStoreOptions {
        db: db.clone(),
        session_ttl_ms: 60_000,
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        random: Arc::new(WalkingRandom(AtomicU8::new(0))),
        hasher: Arc::new(FakeHasher),
    })
    .unwrap();
    Built { store, clock, db }
}

#[test]
fn a_setup_code_is_grouped_for_transcription_and_reported_as_outstanding() {
    let built = with_walking_random();
    assert!(!built.store.has_setup_code().unwrap());

    let code = built.store.issue_setup_code().unwrap();

    // Grouped for someone reading it off a terminal and typing it into a
    // browser, which is the entire use case.
    let groups: Vec<&str> = code.split('-').collect();
    assert_eq!(groups.len(), 3, "{code}");
    for group in &groups {
        assert_eq!(group.len(), 4, "{code}");
        assert!(
            group
                .bytes()
                .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b)),
            "{code}"
        );
    }
    assert!(built.store.has_setup_code().unwrap());
}

#[test]
fn a_setup_code_is_accepted_however_it_was_transcribed() {
    let built = with_walking_random();
    let code = built.store.issue_setup_code().unwrap();

    // The dashes and the case are presentation. Someone who pasted it without
    // the grouping has entered the right code.
    let typed = code.replace('-', "").to_lowercase();
    assert!(built.store.consume_setup_code(&typed).unwrap());
}

#[test]
fn a_setup_code_is_single_use() {
    let built = with_walking_random();
    let code = built.store.issue_setup_code().unwrap();

    assert!(built.store.consume_setup_code(&code).unwrap());
    assert!(!built.store.consume_setup_code(&code).unwrap());
    assert!(!built.store.has_setup_code().unwrap());
}

/// A typo must not lock the operator out of their own install.
#[test]
fn a_wrong_code_leaves_the_real_one_alone() {
    let built = with_walking_random();
    let code = built.store.issue_setup_code().unwrap();

    assert!(!built.store.consume_setup_code("ZZZZ-ZZZZ-ZZZZ").unwrap());
    // Different length, which is the branch that answers before the comparison.
    assert!(!built.store.consume_setup_code("ZZZZ").unwrap());
    assert!(built.store.consume_setup_code(&code).unwrap());
}

#[test]
fn issuing_replaces_an_outstanding_code_rather_than_keeping_both() {
    let built = with_walking_random();
    let first = built.store.issue_setup_code().unwrap();
    let second = built.store.issue_setup_code().unwrap();
    assert_ne!(first, second);

    assert!(!built.store.consume_setup_code(&first).unwrap());
    assert!(built.store.consume_setup_code(&second).unwrap());
}

/// Once a password exists the code is a second way in that nobody is watching —
/// and it was printed to a terminal whose scrollback outlives it.
#[test]
fn setting_a_password_invalidates_an_outstanding_code() {
    let built = with_walking_random();
    let code = built.store.issue_setup_code().unwrap();

    built
        .store
        .set_password("chosen-in-the-wizard", None)
        .unwrap();

    assert!(!built.store.has_setup_code().unwrap());
    assert!(!built.store.consume_setup_code(&code).unwrap());
}

#[test]
fn minting_a_code_for_an_install_that_already_has_a_password_is_refused() {
    let built = fake();
    built
        .store
        .set_password("already-claimed-here", None)
        .unwrap();

    let error = built.store.issue_setup_code().unwrap_err();
    assert!(error.message.contains("already set"));
}

#[test]
fn an_install_that_never_had_a_code_reports_none() {
    assert!(!fake().store.consume_setup_code("ANYT-HING-HERE").unwrap());
}

#[test]
fn a_corrupt_stored_code_is_a_wrong_code_not_a_failure() {
    let built = with_walking_random();
    built.store.issue_setup_code().unwrap();
    built
        .db
        .lock()
        .execute(
            "UPDATE auth_secrets SET value = 'not base64!!' WHERE name = 'setup_code'",
            [],
        )
        .unwrap();

    assert!(!built.store.consume_setup_code("ANYT-HING-HERE").unwrap());
}

// schema parity with what the TypeScript stores wrote

/// The tables this module owns, as `sqlite_master` groups them.
const TABLES: [&str; 2] = ["auth_secrets", "auth_sessions"];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sqlite")
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixtures().join(name)).unwrap()).unwrap()
}

/// `sqlite_master` rows for this module's tables and their indexes, as the
/// fixture records them. The `TEXT PRIMARY KEY` auto-indexes carry no SQL and
/// are not in the fixture.
fn schema_rows(db: &Database) -> Vec<Value> {
    let guard = db.lock();
    let mut statement = guard
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_master
              WHERE sql IS NOT NULL ORDER BY name",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok(serde_json::json!({
                "type": row.get::<_, String>("type")?,
                "name": row.get::<_, String>("name")?,
                "tbl_name": row.get::<_, String>("tbl_name")?,
                "sql": row.get::<_, String>("sql")?,
            }))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .filter(|row| TABLES.contains(&row["tbl_name"].as_str().unwrap()))
        .collect()
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
    let built = fake();

    let expected: Vec<Value> = fixture_json("sqlite_master.json")["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| TABLES.contains(&row["tbl_name"].as_str().unwrap()))
        .cloned()
        .collect();

    // Two tables and one index; `auth_secrets`'s primary key is an auto-index
    // with no SQL of its own.
    assert_eq!(expected.len(), 3, "{expected:#?}");
    assert_eq!(schema_rows(&built.db), expected);
}

#[test]
fn constructing_the_store_twice_changes_nothing() {
    let built = fake();
    let before = schema_rows(&built.db);
    drop(
        AuthStore::new(AuthStoreOptions {
            db: built.db.clone(),
            session_ttl_ms: 60_000,
            clock: Arc::clone(&built.clock) as Arc<dyn Clock>,
            random: Arc::new(CountingRandom(AtomicU8::new(0))),
            hasher: Arc::new(FakeHasher),
        })
        .unwrap(),
    );
    assert_eq!(schema_rows(&built.db), before);
}

/// The `CREATE` statements in one fixture block, comments stripped.
fn statements_in(block: &str) -> Vec<String> {
    block
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_statement_the_typescript_constructor_ran_is_one_this_code_runs() {
    let ddl = fixture_json("ddl.json");
    let mut expected = Vec::new();
    for entry in ddl["statements"].as_array().unwrap() {
        if entry["store"] == "AuthStore" {
            expected.extend(statements_in(entry["sql"].as_str().unwrap()));
        }
    }

    let actual: Vec<String> = SCHEMA
        .iter()
        .map(|ddl| ddl.trim().trim_end_matches(';').to_owned())
        .collect();
    assert_eq!(actual, expected);
}
