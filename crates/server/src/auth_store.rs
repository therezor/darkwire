//! Passwords and session tokens, on the connection everything else shares.
//!
//! Two storage decisions worth stating, because they look inconsistent until
//! the reason is visible:
//!
//!  - **The password lives here, not in the credential vault.** The vault
//!    exists for secrets that have to be *recovered* — an API key is useless
//!    unless it can be read back and put in a header. A password is never read
//!    back; only a one-way argon2id digest is stored, and encrypting a digest
//!    adds a key-management dependency to something that is already unreadable.
//!
//!  - **A session token is hashed with SHA-256, not argon2id.** A key
//!    derivation function's cost exists to make guessing a *low-entropy* human
//!    secret expensive. A token is 32 bytes of operating-system randomness, so
//!    there is nothing to guess, and paying ~50 ms per request to prove it
//!    would turn every authenticated request into a rate limit.
//!
//! The token is `<id>.<secret>`. Splitting it is what makes the comparison
//! timing-safe in a way a single opaque string cannot be: the row is found by
//! `id`, which is not a credential, and the secret is then compared in constant
//! time against a digest of the same length. Looking a token up by its own
//! value would put the secret in a SQL `=`, which short-circuits on the first
//! differing byte.
//!
//! Every method here is synchronous, like every other store in this workspace.
//! argon2id costs tens of milliseconds, so the caller wraps a login in
//! `spawn_blocking` rather than holding the async executor for the duration.

use std::sync::{Arc, OnceLock};

use argon2::{Algorithm, Argon2, Params, PasswordHash, PasswordVerifier as _, Version};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use darkwire_core::{Clock, Database, ErrorKind, Result, RowReader, SystemClock, WireError};
use darkwire_protocol::json::js_trim;
use darkwire_protocol::rest::{
    DEFAULT_USERNAME, PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH, Username,
};
use darkwire_security::random::{OsRandom, RandomSource};
use garde::Validate as _;
use rusqlite::params;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;

/// Bytes of entropy in the secret half of a token.
const TOKEN_SECRET_BYTES: usize = 32;
/// Bytes in the lookup half. Not a secret — only a row address.
const TOKEN_ID_BYTES: usize = 12;
/// Bytes in a named server secret — an HMAC key, not a password.
const SECRET_BYTES: usize = 32;

/// How stale `last_seen_at_ms` may get before a read writes.
///
/// Touching the row on every request would turn an authenticated `GET` into a
/// write and put every request in the same write-ahead log the turn is
/// streaming into.
const TOUCH_INTERVAL_MS: i64 = 60_000;

/// The `auth_secrets` table.
pub const AUTH_SECRETS_TABLE: &str = "CREATE TABLE IF NOT EXISTS auth_secrets (
  name          TEXT PRIMARY KEY,
  value         TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL
) STRICT;";

/// The `auth_sessions` table.
pub const AUTH_SESSIONS_TABLE: &str = "CREATE TABLE IF NOT EXISTS auth_sessions (
  id              TEXT    PRIMARY KEY,
  token_sha256    BLOB    NOT NULL,
  label           TEXT    NOT NULL DEFAULT '',
  created_at_ms   INTEGER NOT NULL,
  expires_at_ms   INTEGER NOT NULL,
  last_seen_at_ms INTEGER NOT NULL
) STRICT;";

/// The index the boot-time purge runs on.
pub const AUTH_SESSIONS_EXPIRY_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS auth_sessions_expiry ON auth_sessions(expires_at_ms);";

/// Every statement the store runs on construction, in order.
pub const SCHEMA: &[&str] = &[
    AUTH_SECRETS_TABLE,
    AUTH_SESSIONS_TABLE,
    AUTH_SESSIONS_EXPIRY_INDEX,
];

const ROWS: RowReader = RowReader::new("auth_sessions");

/// The `auth_secrets` row holding the argon2id digest.
const PASSWORD_SECRET: &str = "password";

/// The login name, stored beside the digest it goes with.
///
/// Absent until someone changes it, and [`DEFAULT_USERNAME`] stands in — which
/// is what makes a fresh install signable-into with a name nobody had to
/// choose. Storing the default eagerly would work equally well right up until
/// the default changed, at which point every install that never touched it
/// would be pinned to the old one for no reason it could explain.
///
/// Unlike the password this is stored in the clear, because it is not a secret:
/// it is half of a credential whose other half is the thing under argon2id.
/// What it buys is that guessing the password is not enough — an attacker who
/// reads the database has both anyway, and one who does not has neither.
const USERNAME_SECRET: &str = "username";

/// The one-time code that claims an install with no password.
///
/// Stored in the same table as the password and hashed the same way a session
/// token is — SHA-256, not argon2id. It carries 12 bytes of entropy, so there
/// is nothing to guess and a key derivation function would buy nothing; what
/// matters is that a copy of the database does not yield a usable code.
const SETUP_CODE_SECRET: &str = "setup_code";

/// Bytes behind a setup code. 12 bytes becomes 12 characters in three groups.
const SETUP_CODE_BYTES: usize = 12;

/// Crockford base32 without `I`, `L`, `O` and `U`: the characters a person
/// mistypes when copying a code out of a terminal, and the one that forms
/// words.
const CODE_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// argon2id's memory cost in kibibytes. OWASP's baseline: 19 MiB.
const ARGON2_M_COST: u32 = 19 * 1024;
/// argon2id's iteration count.
const ARGON2_T_COST: u32 = 2;
/// argon2id's degree of parallelism.
const ARGON2_P_COST: u32 = 1;

/// What is hashed, on both sides of a setup code.
///
/// The dashes and the case are presentation. Someone who pastes the code
/// without its grouping, or types it in lower case, has entered the right code,
/// and a comparison that said otherwise would be rejecting a correct answer.
fn normalise_code(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_uppercase()
}

/// The SHA-256 of a string's bytes.
fn sha256(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}

/// The hashing half, injected.
///
/// argon2id is deliberately expensive — around 50 ms per call — which is
/// correct in production and ruinous in a test suite that logs in a few hundred
/// times. Tests and the end-to-end harness substitute a cheap implementation;
/// the real one is exercised by the tests that are about hashing.
pub trait PasswordHasher: Send + Sync {
    /// Hashes `password` into a storable encoding.
    fn hash(&self, password: &str) -> Result<String>;
    /// Whether `password` is the one `hash` was made from.
    ///
    /// Answers rather than fails: a stored value that is not a valid encoding
    /// is a corrupt row, not a correct password, and failing closed is the only
    /// safe reading.
    fn verify(&self, hash: &str, password: &str) -> bool;
}

/// OWASP's argon2id baseline: m=19 MiB, t=2, p=1, version 0x13.
///
/// The parameters are spelled out rather than taken from the library's defaults
/// so that a change upstream is a change in behaviour this codebase has to
/// make deliberately. Verification reads the parameters out of the *stored*
/// encoding, which is what lets a digest written by an older install — or by
/// the TypeScript implementation this replaces — keep verifying unchanged.
#[derive(Debug, Clone, Copy, Default)]
pub struct Argon2Hasher;

impl Argon2Hasher {
    /// The configured hasher. Falls back to the library's own defaults, which
    /// are the same three numbers, if the parameters are ever made invalid.
    fn argon2() -> Argon2<'static> {
        let params = Params::new(ARGON2_M_COST, ARGON2_T_COST, ARGON2_P_COST, None)
            .unwrap_or(Params::DEFAULT);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
    }
}

impl PasswordHasher for Argon2Hasher {
    fn hash(&self, password: &str) -> Result<String> {
        use argon2::PasswordHasher as _;
        Argon2Hasher::argon2()
            .hash_password(password.as_bytes())
            .map(|hash| hash.to_string())
            .map_err(|error| {
                WireError::new(ErrorKind::Internal, "Could not hash the password")
                    .with_detail("reason", error.to_string())
            })
    }

    fn verify(&self, hash: &str, password: &str) -> bool {
        let Ok(parsed) = PasswordHash::new(hash) else {
            return false;
        };
        Argon2Hasher::argon2()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    }
}

/// A verified session. Never carries the token it was verified from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSession {
    /// The row address, which is the non-secret half of the token.
    pub id: String,
    /// What minted it: a browser login, or a token issued for CI.
    pub label: String,
    /// When it was minted.
    pub created_at_ms: i64,
    /// When it stops being accepted.
    pub expires_at_ms: i64,
}

/// A freshly minted token, and the only moment its secret half exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedToken {
    /// The credential to hand to the caller. Only its digest is stored.
    pub token: String,
    /// The row address.
    pub id: String,
    /// When it stops being accepted.
    pub expires_at_ms: i64,
}

/// How an [`AuthStore`] is wired.
pub struct AuthStoreOptions {
    /// Shared with the session store and the scheduler.
    pub db: Database,
    /// How long a newly issued token lives.
    pub session_ttl_ms: i64,
    /// Wall-clock time.
    pub clock: Arc<dyn Clock>,
    /// Where token and setup-code entropy comes from.
    pub random: Arc<dyn RandomSource>,
    /// How a password becomes a digest.
    pub hasher: Arc<dyn PasswordHasher>,
}

impl AuthStoreOptions {
    /// The production wiring: the host clock, the operating system's
    /// randomness, and argon2id.
    pub fn new(db: Database, session_ttl_ms: i64) -> AuthStoreOptions {
        AuthStoreOptions {
            db,
            session_ttl_ms,
            clock: Arc::new(SystemClock),
            random: Arc::new(OsRandom),
            hasher: Arc::new(Argon2Hasher),
        }
    }
}

/// Passwords, setup codes, session tokens and named server secrets.
pub struct AuthStore {
    db: Database,
    clock: Arc<dyn Clock>,
    random: Arc<dyn RandomSource>,
    hasher: Arc<dyn PasswordHasher>,
    ttl_ms: i64,
    /// Computed at most once, on the first login against an install that has no
    /// password.
    decoy_digest: OnceLock<String>,
}

impl std::fmt::Debug for AuthStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthStore")
            .field("ttl_ms", &self.ttl_ms)
            .finish_non_exhaustive()
    }
}

impl AuthStore {
    /// Opens the store, creating its tables.
    pub fn new(options: AuthStoreOptions) -> Result<AuthStore> {
        for statement in SCHEMA {
            options.db.execute_batch(statement)?;
        }
        Ok(AuthStore {
            db: options.db,
            clock: options.clock,
            random: options.random,
            hasher: options.hasher,
            ttl_ms: options.session_ttl_ms,
            decoy_digest: OnceLock::new(),
        })
    }

    /// Whether this install has been claimed.
    pub fn has_password(&self) -> Result<bool> {
        Ok(self.read_secret(PASSWORD_SECRET)?.is_some())
    }

    /// The login name in force, which is [`DEFAULT_USERNAME`] until one is set.
    pub fn username(&self) -> Result<String> {
        Ok(self
            .read_secret(USERNAME_SECRET)?
            .unwrap_or_else(|| DEFAULT_USERNAME.to_owned()))
    }

    /// Sets or rotates the password, and optionally the login name with it.
    ///
    /// One method for both because they share the consequence below, and
    /// because a separate setter for the name would be a way to change half a
    /// credential without proving knowledge of the other half — which is
    /// exactly the thing the route above it asks for a current password to
    /// prevent.
    ///
    /// Every existing session is revoked, because the reason to change a
    /// password is that the old one may be known — and a token minted under it
    /// outliving the rotation makes the rotation cosmetic.
    pub fn set_password(&self, password: &str, username: Option<&str>) -> Result<()> {
        assert_password_policy(password)?;
        let now = self.clock.now_ms();

        // Validated before anything is written, against the same rules the
        // route body is parsed with, so a name the HTTP layer would have
        // refused cannot arrive through a command-line flag instead.
        let mut normalised_name: Option<String> = None;
        if let Some(raw) = username {
            let parsed = Username::new(raw);
            if let Err(report) = parsed.validate() {
                let reason = report.iter().next().map_or_else(
                    || "does not meet the rules".to_owned(),
                    |(_, e)| e.to_string(),
                );
                return Err(WireError::new(
                    ErrorKind::InvalidInput,
                    format!("Invalid username: {reason}"),
                ));
            }
            if parsed.as_str() == js_trim(password).to_lowercase() {
                // Not a strength heuristic — those belong in a password
                // manager, not here. This is the one case where a "password" is
                // a value the operator has already typed into a field that is
                // not masked and may be in a log.
                return Err(WireError::new(
                    ErrorKind::InvalidInput,
                    "The password must not be the username",
                ));
            }
            normalised_name = Some(parsed.as_str().to_owned());
        }

        let digest = self.hasher.hash(password)?;
        self.db.transaction(|conn| {
            let mut write = conn.prepare(
                "INSERT INTO auth_secrets (name, value, updated_at_ms) VALUES (?, ?, ?)
                 ON CONFLICT(name) DO UPDATE SET value = excluded.value,
                   updated_at_ms = excluded.updated_at_ms",
            )?;
            write.execute(params![PASSWORD_SECRET, digest, now])?;
            if let Some(name) = &normalised_name {
                write.execute(params![USERNAME_SECRET, name, now])?;
            }
            // The setup code is a stand-in for a password that does not exist
            // yet. The moment one does, an outstanding code is a second way in
            // that nobody is watching — and it was printed to a terminal whose
            // scrollback outlives it.
            conn.execute(
                "DELETE FROM auth_secrets WHERE name = ?",
                params![SETUP_CODE_SECRET],
            )?;
            conn.execute("DELETE FROM auth_sessions", [])?;
            Ok(())
        })
    }

    /// Mints the one-time code that claims an unclaimed install, replacing any
    /// outstanding one.
    ///
    /// Replacing rather than reusing: a restarted server prints a fresh code,
    /// and the one in the previous run's scrollback stops working. That is the
    /// weaker of the two properties — the stronger one is that the code exists
    /// at all, which is what lets the server come up on a bare machine instead
    /// of refusing to start and leaving the interface that would set a password
    /// unreachable.
    ///
    /// Refuses once a password exists, because there is nothing left to claim
    /// and an alternative credential would only widen the ways in.
    pub fn issue_setup_code(&self) -> Result<String> {
        if self.has_password()? {
            return Err(WireError::new(
                ErrorKind::InvalidInput,
                "A password is already set; there is nothing to claim",
            ));
        }

        let mut bytes = [0u8; SETUP_CODE_BYTES];
        self.random.fill(&mut bytes);
        let mut code = String::with_capacity(SETUP_CODE_BYTES + 2);
        for (index, byte) in bytes.iter().enumerate() {
            // Grouped for transcription, not for entropy: a person reading this
            // off a terminal and typing it into a browser is the whole use
            // case.
            if index > 0 && index % 4 == 0 {
                code.push('-');
            }
            let position = usize::from(*byte) % CODE_ALPHABET.len();
            code.push(char::from(
                CODE_ALPHABET.get(position).copied().unwrap_or(b'0'),
            ));
        }

        let stored = STANDARD.encode(sha256(&normalise_code(&code)));
        self.db.lock().execute(
            "INSERT INTO auth_secrets (name, value, updated_at_ms) VALUES (?, ?, ?)
             ON CONFLICT(name) DO UPDATE SET value = excluded.value,
               updated_at_ms = excluded.updated_at_ms",
            params![SETUP_CODE_SECRET, stored, self.clock.now_ms()],
        )?;
        Ok(code)
    }

    /// Whether an unspent code is outstanding — never the code itself.
    pub fn has_setup_code(&self) -> Result<bool> {
        Ok(self.read_secret(SETUP_CODE_SECRET)?.is_some())
    }

    /// Spends the code, if it is the right one.
    ///
    /// Single use either way it is read: a correct code is deleted here, and a
    /// wrong one leaves the real code alone so a typo does not lock the
    /// operator out of their own install.
    pub fn consume_setup_code(&self, code: &str) -> Result<bool> {
        let Some(stored) = self.read_secret(SETUP_CODE_SECRET)? else {
            return Ok(false);
        };

        let Ok(expected) = STANDARD.decode(&stored) else {
            // A corrupt row must not turn a bad code into a 500.
            return Ok(false);
        };
        let presented = sha256(&normalise_code(code));
        if expected.len() != presented.len() {
            return Ok(false);
        }
        if !bool::from(expected.ct_eq(&presented)) {
            return Ok(false);
        }

        self.db.lock().execute(
            "DELETE FROM auth_secrets WHERE name = ?",
            params![SETUP_CODE_SECRET],
        )?;
        Ok(true)
    }

    /// The password alone, for the one caller that already knows who it is
    /// talking to: the rotation route proving that the holder of a session also
    /// knows the password they are replacing. A login must use
    /// [`AuthStore::verify_login`] instead.
    pub fn verify_password(&self, password: &str) -> Result<bool> {
        let Some(digest) = self.read_secret(PASSWORD_SECRET)? else {
            return Ok(false);
        };
        Ok(self.hasher.verify(&digest, password))
    }

    /// Both halves, in time that does not depend on which half was wrong.
    ///
    /// The obvious implementation returns early when the username does not
    /// match, and that early return is a username oracle: a wrong name answers
    /// in under a millisecond and a wrong password answers in fifty, so an
    /// attacker learns the account name for free and has only the password left
    /// to guess. So the key derivation function runs on every attempt — against
    /// the stored digest when there is one and against a decoy when there is
    /// not — and the two answers are combined only once both exist.
    pub fn verify_login(&self, username: &str, password: &str) -> Result<bool> {
        let digest = self.read_secret(PASSWORD_SECRET)?;
        let name_matches = constant_time_eq(&self.username()?, &normalised_username(username));
        // Deliberately not short-circuited on `name_matches`, and deliberately
        // not skipped when no password is set: an unclaimed install must not
        // answer faster than a claimed one.
        let against = match &digest {
            Some(stored) => stored.as_str(),
            None => self.decoy(),
        };
        let password_matches = self.hasher.verify(against, password);
        Ok(digest.is_some() && name_matches && password_matches)
    }

    /// An argon2 encoding of a value nobody knows, hashed once and kept.
    ///
    /// It exists so that "no password is set" costs the same as "the password
    /// is wrong". Computed lazily rather than in the constructor because every
    /// server builds a store and almost none of them ever see a login against
    /// an unclaimed install — paying 50 ms at every boot to cover that case
    /// would be the more expensive mistake.
    fn decoy(&self) -> &str {
        self.decoy_digest.get_or_init(|| {
            let mut bytes = [0u8; TOKEN_SECRET_BYTES];
            self.random.fill(&mut bytes);
            // A hasher that cannot hash leaves an encoding nothing verifies
            // against, which is the fail-closed answer; the alternative would
            // be to accept an unclaimed install.
            self.hasher
                .hash(&URL_SAFE_NO_PAD.encode(bytes))
                .unwrap_or_default()
        })
    }

    /// Mints a token. The returned string is never recoverable afterwards.
    ///
    /// `label` distinguishes a browser login from a token minted for CI, which
    /// is the only thing that makes a session list worth showing.
    pub fn issue(&self, label: &str) -> Result<IssuedToken> {
        let now = self.clock.now_ms();
        let mut id_bytes = [0u8; TOKEN_ID_BYTES];
        self.random.fill(&mut id_bytes);
        let mut secret_bytes = [0u8; TOKEN_SECRET_BYTES];
        self.random.fill(&mut secret_bytes);
        let id = URL_SAFE_NO_PAD.encode(id_bytes);
        let secret = URL_SAFE_NO_PAD.encode(secret_bytes);
        let expires_at_ms = now + self.ttl_ms;

        self.db.lock().execute(
            "INSERT INTO auth_sessions
               (id, token_sha256, label, created_at_ms, expires_at_ms, last_seen_at_ms)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![id, sha256(&secret), label, now, expires_at_ms, now],
        )?;

        Ok(IssuedToken {
            token: format!("{id}.{secret}"),
            id,
            expires_at_ms,
        })
    }

    /// A named server secret, created on first use.
    ///
    /// The media URL signer needs a key that survives a restart — a signature
    /// minted before a reload has to still verify after it, or every image in
    /// an open tab breaks on deploy — and that is the same durable,
    /// never-transmitted storage the password already has. Generating it lazily
    /// rather than at boot means an install that never serves a file never
    /// writes one.
    ///
    /// Three names are refused. The password and the setup code are stored as
    /// one-way digests, and a caller that got one back here would be handing a
    /// hash to whatever asked for a signing key. The username is refused for
    /// the opposite reason: it is *not* a secret, and worse, this method
    /// generates what it does not find — asking for it on an install that never
    /// changed it would replace the login name with 32 random bytes.
    pub fn ensure_secret(&self, name: &str) -> Result<String> {
        if matches!(name, PASSWORD_SECRET | SETUP_CODE_SECRET | USERNAME_SECRET) {
            return Err(WireError::new(
                ErrorKind::InvalidInput,
                format!("{name} is not a readable secret"),
            ));
        }
        if let Some(existing) = self.read_secret(name)? {
            return Ok(existing);
        }

        let mut bytes = [0u8; SECRET_BYTES];
        self.random.fill(&mut bytes);
        let secret = URL_SAFE_NO_PAD.encode(bytes);
        // `DO NOTHING` rather than `DO UPDATE`: two requests racing to serve the
        // first signed URL must end up with the same key, not the second one's.
        self.db.lock().execute(
            "INSERT INTO auth_secrets (name, value, updated_at_ms) VALUES (?, ?, ?)
             ON CONFLICT(name) DO NOTHING",
            params![name, secret, self.clock.now_ms()],
        )?;
        Ok(self.read_secret(name)?.unwrap_or(secret))
    }

    /// `None` for anything that is not a live session — never a reason why.
    pub fn verify(&self, token: &str) -> Result<Option<AuthSession>> {
        let Some(separator) = token.find('.') else {
            return Ok(None);
        };
        if separator == 0 || separator == token.len() - 1 {
            return Ok(None);
        }
        let (Some(id), Some(secret)) = (token.get(..separator), token.get(separator + 1..)) else {
            return Ok(None);
        };

        let found = {
            let guard = self.db.lock();
            let mut statement = guard.prepare(
                "SELECT id, token_sha256, label, created_at_ms, expires_at_ms, last_seen_at_ms
                 FROM auth_sessions WHERE id = ?",
            )?;
            let mut rows = statement.query(params![id])?;
            match rows.next()? {
                None => None,
                Some(row) => Some((
                    row.get::<_, Vec<u8>>("token_sha256").unwrap_or_default(),
                    ROWS.string(row, "label")?,
                    ROWS.int(row, "created_at_ms")?,
                    ROWS.int(row, "expires_at_ms")?,
                    ROWS.int(row, "last_seen_at_ms")?,
                )),
            }
        };
        let Some((stored, label, created_at_ms, expires_at_ms, last_seen_at_ms)) = found else {
            return Ok(None);
        };

        // The column is `BLOB NOT NULL` in a `STRICT` table, so the length
        // check is belt and braces — but a row that somehow got there must not
        // turn a bad credential into a 500.
        let presented = sha256(secret);
        if stored.len() != presented.len() {
            return Ok(None);
        }
        if !bool::from(stored.ct_eq(&presented)) {
            return Ok(None);
        }

        let now = self.clock.now_ms();
        if expires_at_ms <= now {
            self.revoke_by_id(id)?;
            return Ok(None);
        }

        if now - last_seen_at_ms > TOUCH_INTERVAL_MS {
            self.db.lock().execute(
                "UPDATE auth_sessions SET last_seen_at_ms = ? WHERE id = ?",
                params![now, id],
            )?;
        }

        Ok(Some(AuthSession {
            id: id.to_owned(),
            label,
            created_at_ms,
            expires_at_ms,
        }))
    }

    /// Drops one session. `false` when there was nothing to drop.
    pub fn revoke_by_id(&self, id: &str) -> Result<bool> {
        let changed = self
            .db
            .lock()
            .execute("DELETE FROM auth_sessions WHERE id = ?", params![id])?;
        Ok(changed > 0)
    }

    /// Drops every session, and reports how many.
    pub fn revoke_all(&self) -> Result<usize> {
        Ok(self.db.lock().execute("DELETE FROM auth_sessions", [])?)
    }

    /// Called at boot so a long-down instance does not accumulate dead rows.
    pub fn purge_expired(&self) -> Result<usize> {
        Ok(self.db.lock().execute(
            "DELETE FROM auth_sessions WHERE expires_at_ms <= ?",
            params![self.clock.now_ms()],
        )?)
    }

    fn read_secret(&self, name: &str) -> Result<Option<String>> {
        let guard = self.db.lock();
        let mut statement = guard.prepare("SELECT value FROM auth_secrets WHERE name = ?")?;
        let mut rows = statement.query(params![name])?;
        match rows.next()? {
            None => Ok(None),
            Some(row) => Ok(row.get::<_, String>("value").ok()),
        }
    }
}

/// The name as it is compared, or empty for one too malformed to normalise.
///
/// Empty never matches a stored name, because a stored one has passed the same
/// validation — so a caller that sends nonsense is refused without the store
/// having to fail.
fn normalised_username(raw: &str) -> String {
    let candidate = Username::new(raw);
    if candidate.validate().is_ok() {
        candidate.as_str().to_owned()
    } else {
        String::new()
    }
}

/// String equality that does not stop at the first differing byte.
///
/// The length is compared first and answers early, which is unavoidable. That
/// leaks the length of the username, which is not the secret; its content is,
/// and that is what stays constant-time.
fn constant_time_eq(left: &str, right: &str) -> bool {
    let a = left.as_bytes();
    let b = right.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    bool::from(a.ct_eq(b))
}

/// The bounds a new password must clear, in the one place both callers reach.
///
/// The HTTP body is parsed with a schema that says the same thing — but
/// `--password` and `DARKWIRE_PASSWORD` come in through the composition root and
/// never touch it, and a policy that the command line can walk around is a
/// policy that describes the interface rather than the install.
///
/// The length is counted in UTF-16 units, which is what the schema on the other
/// side of the wire counts, so a password of astral-plane characters is
/// accepted or refused identically by both.
fn assert_password_policy(password: &str) -> Result<()> {
    let length = password.encode_utf16().count();
    if length < PASSWORD_MIN_LENGTH {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            format!(
                "Password must be at least {PASSWORD_MIN_LENGTH} characters. \
                 What is behind it is an agent that can read files and run commands on this host."
            ),
        ));
    }
    if length > PASSWORD_MAX_LENGTH {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            format!("Password must be at most {PASSWORD_MAX_LENGTH} characters"),
        ));
    }
    Ok(())
}
