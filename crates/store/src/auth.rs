//! User accounts and browser session tokens.
//!
//! Ports `core/auth.py`. These live in two JSON files beside the database, **not**
//! in it — there is no `users` table anywhere in the Python schema — so this
//! module owns them directly.
//!
//! | File | Contents |
//! |---|---|
//! | `data/auth.json` | users, signup flag, per-user privileges, TOTP state |
//! | `data/sessions.json` | browser session tokens with expiry |
//!
//! Both are written atomically (temp file, `fsync`, rename), mirroring
//! `core/atomic_io.py`. A partially-written `auth.json` would lock every user out,
//! so a torn write is not an acceptable failure mode.
//!
//! # Invariants that must survive the port
//!
//! * Tokens are 32 random bytes as 64 hex characters, with a 7-day TTL.
//! * `validate_token` performs an **orphan check**: a token whose username no
//!   longer exists in `auth.json` is rejected *and* pruned. Otherwise renaming or
//!   deleting a user would leave live sessions behind.
//!
//! TOTP is **not implemented here** (T0068). `User` parses `totp_enabled` and the
//! secrets so the file round-trips, but nothing in this module verifies a code, so a
//! caller reading those fields today must not treat them as enforced.
//! * Admins get the full privilege set wholesale, and the last admin cannot be
//!   demoted or deleted.
//! * Promotion stashes `privileges_before_admin`; demotion restores it, so a
//!   temporary admin stint cannot leave `can_use_bash` behind.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use {
    serde::{Deserialize, Serialize},
    serde_json::json,
    thiserror::Error,
};

use crate::atomic::atomic_write_json;

/// Minimum password length, from `PASSWORD_MIN_LENGTH`.
pub const PASSWORD_MIN_LENGTH: usize = 8;

/// Session lifetime in seconds, from `TOKEN_TTL` (7 days).
pub const TOKEN_TTL_SECONDS: u64 = 60 * 60 * 24 * 7;

/// Bytes of entropy in a session token, matching `secrets.token_hex(32)`.
pub const TOKEN_BYTES: usize = 32;

/// Usernames that may never be registered, from `RESERVED_AUTH_USERNAMES`.
pub const RESERVED_USERNAMES: &[&str] = &[
    "internal-tool",
    "api",
    "demo",
    "system",
    "__odysseus_local__",
];

/// Failures from the auth store.
#[derive(Debug, Error)]
pub enum AuthError {
    /// A filesystem operation failed.
    #[error("auth io error at {path}: {source}")]
    Io {
        /// The path involved.
        path: String,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// The JSON was unreadable or structurally wrong.
    ///
    /// Never silently replaced: a corrupt `auth.json` is something an operator
    /// must look at, and regenerating it would silently revoke everyone's access.
    #[error("auth file at {path} is unreadable: {message}")]
    Malformed {
        /// The path involved.
        path: String,
        /// What went wrong.
        message: String,
    },

    /// Password hashing or verification failed.
    #[error("password error: {0}")]
    Password(String),

    /// The operation is not permitted.
    #[error("{0}")]
    Denied(String),

    /// The requested user does not exist.
    #[error("no such user: {0}")]
    NoSuchUser(String),

    /// The password did not meet the minimum length.
    #[error("password must be at least {PASSWORD_MIN_LENGTH} characters")]
    PasswordTooShort,

    /// A mutation lock was poisoned by a panic in another thread.
    #[error("auth lock for {0} is poisoned; refusing to continue")]
    Lock(String),
}

/// A user's stored record.
///
/// `Debug` is hand-written to redact `password_hash` and the TOTP secrets. A
/// derived `Debug` would put a bcrypt hash and a base32 TOTP seed into any
/// `tracing::debug!(?user)`, any `#[instrument]` that captures its argument, and
/// any `expect("…{user:?}")` — and a leaked TOTP seed regenerates valid codes
/// forever, which no session revocation can undo. See AGENTS.md §4 and §10.
#[derive(Clone, Serialize, Deserialize)]
pub struct User {
    /// bcrypt hash, never the plaintext.
    #[serde(deserialize_with = "null_as_default")]
    pub password_hash: String,
    /// Unix creation time.
    #[serde(default, deserialize_with = "null_as_default")]
    pub created: f64,
    /// Whether this user administers the instance.
    #[serde(default, deserialize_with = "null_as_default")]
    pub is_admin: bool,
    /// Privilege overrides. Admins ignore this entirely.
    #[serde(
        default,
        deserialize_with = "null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub privileges: BTreeMap<String, serde_json::Value>,
    /// The privilege map this user held *before* being promoted to admin.
    ///
    /// Inert while `is_admin` is set: `privileges_for` short-circuits to
    /// [`Privileges::admin`], and `set_privileges` refuses admins, so only
    /// `set_admin` ever touches this field. Without it a temporary admin stint
    /// would leak the admin map — including `can_use_bash` — into the user's
    /// permanent privileges on demotion.
    #[serde(default, deserialize_with = "null_as_default")]
    pub privileges_before_admin: Option<BTreeMap<String, serde_json::Value>>,
    /// Base32 TOTP secret.
    #[serde(default, deserialize_with = "null_as_default")]
    pub totp_secret: Option<String>,
    /// Whether TOTP is active.
    #[serde(default, deserialize_with = "null_as_default")]
    pub totp_enabled: bool,
    /// A secret awaiting confirmation, discarded once confirmed.
    #[serde(default, deserialize_with = "null_as_default")]
    pub totp_secret_pending: Option<String>,
    /// Single-use recovery codes.
    #[serde(default, deserialize_with = "null_as_default")]
    pub totp_backup_codes: Option<Vec<String>>,
    /// Any key this build does not know about.
    ///
    /// Without this, serde silently discards undeclared per-user fields on decode
    /// and the next `save_auth` writes back the truncated record — so any single
    /// mutation, for any user, would erase every unknown field for *every* user.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl std::fmt::Debug for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("User")
            .field("password_hash", &"<redacted>")
            .field("created", &self.created)
            .field("is_admin", &self.is_admin)
            .field("privileges", &self.privileges)
            .field("privileges_before_admin", &self.privileges_before_admin)
            .field("extra_keys", &self.extra.keys().collect::<Vec<_>>())
            .field(
                "totp_secret",
                &self.totp_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("totp_enabled", &self.totp_enabled)
            .field(
                "totp_secret_pending",
                &self.totp_secret_pending.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "totp_backup_codes",
                &self
                    .totp_backup_codes
                    .as_ref()
                    .map_or(0usize, Vec::len)
                    .to_string(),
            )
            .finish()
    }
}

impl User {
    /// A new user with the given bcrypt hash.
    #[must_use]
    pub fn new(password_hash: String, is_admin: bool) -> Self {
        Self {
            password_hash,
            created: now_secs_f64(),
            is_admin,
            privileges: if is_admin {
                admin_privileges_map()
            } else {
                default_privileges_map()
            },
            privileges_before_admin: None,
            totp_secret: None,
            totp_enabled: false,
            totp_secret_pending: None,
            totp_backup_codes: None,
            extra: BTreeMap::new(),
        }
    }
}

/// The `auth.json` document.
///
/// `Debug` is hand-written so that logging an `AuthFile` cannot spill every user's
/// bcrypt hash at once. See [`User`].
#[derive(Clone, Default, Serialize, Deserialize)]
struct AuthFile {
    /// Whether anyone may self-register.
    #[serde(default, deserialize_with = "null_as_default")]
    signup_enabled: bool,
    /// Keyed by lowercased username.
    #[serde(default, deserialize_with = "null_as_default")]
    users: BTreeMap<String, User>,
    /// Any other keys, preserved so an older build's settings survive a round trip.
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl std::fmt::Debug for AuthFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthFile")
            .field("signup_enabled", &self.signup_enabled)
            .field("users", &self.users)
            .field("extra_keys", &self.extra.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// The `sessions.json` document.
///
/// `Debug` is hand-written because the map **key is the credential**: a derived
/// impl would print every live 7-day session token in cleartext.
#[derive(Clone, Default, Serialize, Deserialize)]
struct SessionFile {
    #[serde(flatten, deserialize_with = "drop_unparseable_entries")]
    tokens: BTreeMap<String, SessionEntry>,
}

impl std::fmt::Debug for SessionFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionFile")
            .field("token_count", &self.tokens.len())
            .finish()
    }
}

/// Decodes the token map, discarding entries that are not objects.
///
/// Without this, one element of the wrong type — `{"abc…": null}` from a
/// hand-edit or a partial write — fails the parse of the whole document, and since
/// `validate_token` reads this file on every authenticated request, a single bad
/// byte bricks auth for every user with no way to recover but an operator edit.
/// An entry we cannot read is also an entry we cannot authenticate, so dropping it
/// is the same outcome as rejecting it.
fn drop_unparseable_entries<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, SessionEntry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(
            |(token, value)| match serde_json::from_value::<SessionEntry>(value) {
                Ok(entry) => Some((token, entry)),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping unparseable session entry");
                    None
                }
            },
        )
        .collect())
}

/// One live browser session.
#[derive(Clone, Serialize, Deserialize)]
struct SessionEntry {
    /// Defaulted so a hand-edited or truncated entry is skipped rather than
    /// failing the parse of every other token in the file.
    #[serde(default, deserialize_with = "null_as_default")]
    username: String,
    /// Unix expiry, as Python's `time.time()`.
    ///
    /// Defaults to `0` rather than erroring, matching `_load_sessions`'s
    /// `v.get("expiry", 0) > now`, so a missing or null expiry simply expires.
    /// Without this a single bad entry would fail the whole file's parse and turn
    /// every authenticated request into a 500.
    #[serde(default, deserialize_with = "null_as_default")]
    expiry: f64,
}

/// Treats an explicit JSON `null` as an absent field.
///
/// `#[serde(default)]` only covers a *missing* key, so `{"expiry": null}` would
/// still be a hard parse error. Both failure modes must expire the entry.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

impl std::fmt::Debug for SessionEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionEntry")
            .field("username", &self.username)
            .field("expiry", &self.expiry)
            .finish()
    }
}

/// The privilege set, from `DEFAULT_PRIVILEGES`.
///
/// Boolean switches are `true` by default except `can_use_bash`, which is off
/// because shell access is the highest-risk capability in the app.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "these mirror the eleven independent DEFAULT_PRIVILEGES keys that `auth_helpers.py` reads by name; folding them into a bitflags field would hide the exact JSON keys, which are part of the wire format of auth.json"
)]
pub struct Privileges {
    /// May use the agent loop.
    pub can_use_agent: bool,
    /// May use the browser tool.
    pub can_use_browser: bool,
    /// May use the document editor.
    pub can_use_documents: bool,
    /// May run deep research.
    pub can_use_research: bool,
    /// May generate images.
    pub can_generate_images: bool,
    /// May manage memories.
    pub can_manage_memory: bool,
    /// May run shell commands.
    pub can_use_bash: bool,
    /// Daily message cap; `0` is unlimited.
    pub max_messages_per_day: u64,
    /// Model ids this user may use. Empty means no restriction.
    pub allowed_models: Vec<String>,
    /// Whether `allowed_models` is enforced.
    pub allowed_models_restricted: bool,
    /// Whether every model is blocked.
    pub block_all_models: bool,
}

impl Default for Privileges {
    /// The defaults an unconfigured user gets.
    fn default() -> Self {
        Self {
            can_use_agent: true,
            can_use_browser: true,
            can_use_documents: true,
            can_use_research: true,
            can_generate_images: true,
            can_manage_memory: true,
            // Off by default: shell is remote code execution.
            can_use_bash: false,
            max_messages_per_day: 0,
            allowed_models: Vec::new(),
            allowed_models_restricted: false,
            block_all_models: false,
        }
    }
}

impl Privileges {
    /// What an admin gets: every switch on, no restrictions.
    #[must_use]
    pub fn admin() -> Self {
        Self {
            can_use_agent: true,
            can_use_browser: true,
            can_use_documents: true,
            can_use_research: true,
            can_generate_images: true,
            can_manage_memory: true,
            can_use_bash: true,
            max_messages_per_day: 0,
            allowed_models: Vec::new(),
            allowed_models_restricted: false,
            block_all_models: false,
        }
    }

    /// Looks up a privilege by its key, matching `require_privilege`.
    ///
    /// An unknown key returns `true`, which is the Python behaviour
    /// (`auth_helpers.py:169`). That is deliberately permissive so a newer
    /// privilege name cannot lock every user out of an older build, and it is
    /// documented here because it reads like a mistake.
    #[must_use]
    pub fn get(&self, key: &str) -> bool {
        match key {
            "can_use_agent" => self.can_use_agent,
            "can_use_browser" => self.can_use_browser,
            "can_use_documents" => self.can_use_documents,
            "can_use_research" => self.can_use_research,
            "can_generate_images" => self.can_generate_images,
            "can_manage_memory" => self.can_manage_memory,
            "can_use_bash" => self.can_use_bash,
            // Not booleans; a boolean probe of these keys is vacuously permitted.
            // Unknown keys also fall through to `true`, per the doc comment.
            _ => true,
        }
    }
}

/// Where the auth files live.
///
/// # Sharing
///
/// Create **one** instance per data directory and share it (usually inside
/// `Arc`); every mutation is a read-modify-write of a whole JSON document, so
/// two stores built separately over the same directory would not share a lock and
/// could lose an update. Clones of a single instance do share the locks.
#[derive(Debug, Clone)]
pub struct AuthStore {
    auth_path: PathBuf,
    sessions_path: PathBuf,
    /// Guards the `auth.json` read-modify-write cycle, mirroring Python's
    /// `_config_lock`. Without it two concurrent `create_user` calls can each read
    /// the same file and the second write silently drops the first user.
    config_lock: Arc<Mutex<()>>,
    /// Guards the `sessions.json` read-modify-write cycle, mirroring Python's
    /// `_sessions_lock`. Held separately from `config_lock` so validating a token
    /// does not serialise against user administration.
    sessions_lock: Arc<Mutex<()>>,
}

impl AuthStore {
    /// Points the store at `data_dir`.
    #[must_use]
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        let dir = data_dir.as_ref();
        Self {
            auth_path: dir.join("auth.json"),
            sessions_path: dir.join("sessions.json"),
            config_lock: Arc::new(Mutex::new(())),
            sessions_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Locks `auth.json` for a read-modify-write cycle.
    ///
    /// A poisoned lock means some other thread panicked mid-mutation. The on-disk
    /// file is still atomically written, so the data is intact; refusing is the
    /// fail-closed choice for an auth store.
    fn lock_config(&self) -> Result<MutexGuard<'_, ()>, AuthError> {
        self.config_lock
            .lock()
            .map_err(|_| AuthError::Lock("auth.json".into()))
    }

    /// Locks `sessions.json` for a read-modify-write cycle.
    fn lock_sessions(&self) -> Result<MutexGuard<'_, ()>, AuthError> {
        self.sessions_lock
            .lock()
            .map_err(|_| AuthError::Lock("sessions.json".into()))
    }

    /// The `auth.json` path.
    #[must_use]
    pub fn auth_path(&self) -> &Path {
        &self.auth_path
    }

    /// The `sessions.json` path.
    #[must_use]
    pub fn sessions_path(&self) -> &Path {
        &self.sessions_path
    }

    /// Reads `auth.json`, applying legacy migrations and normalisation.
    ///
    /// Migrations, all from `core/auth.py`:
    /// * the single-user `{username, password_hash}` shape becomes a `users` map;
    /// * a legacy `role: "admin"` becomes `is_admin: true`;
    /// * usernames are lowercased, so `Alice` and `alice` are one account;
    /// * reserved names are dropped, since they are used as internal identities.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Malformed`] if the file exists but cannot be parsed.
    /// The caller gets a loud failure rather than a silently empty user table,
    /// which would look like "everyone was locked out".
    fn load_auth(&self) -> Result<AuthFile, AuthError> {
        let Some(raw) = self.read_auth_raw()? else {
            return Ok(AuthFile::default());
        };
        let mut document = parse_auth(&raw, &self.auth_path)?;
        Self::migrate_document(&mut document);
        decode_auth(document, &self.auth_path)
    }

    /// Reads `auth.json` verbatim, or `None` when it does not exist.
    fn read_auth_raw(&self) -> Result<Option<String>, AuthError> {
        if !self.auth_path.exists() {
            return Ok(None);
        }
        std::fs::read_to_string(&self.auth_path)
            .map(Some)
            .map_err(|source| AuthError::Io {
                path: self.auth_path.display().to_string(),
                source,
            })
    }

    /// Reads `auth.json` and writes the migrated form back if it changed.
    ///
    /// Python persists each migration inside the migration itself (`self._save()`).
    /// That is done here, as one explicit step, rather than as a side effect of
    /// reading: a read that writes needs `config_lock`, and `load_auth` is reached
    /// from `validate_token` and `issue_token` while they hold `sessions_lock`.
    /// Taking `config_lock` there would invert the lock order against
    /// `delete_user`/`rename_user`, which hold config and then want sessions.
    ///
    /// Unsynchronised *reads* are safe — `rename` is atomic, so a reader sees
    /// either the whole old file or the whole new one. Only read-modify-**write**
    /// needs the lock, which is why only this method and the mutators take it.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Io`] if the migrated document cannot be written.
    pub fn migrate_and_persist(&self) -> Result<(), AuthError> {
        let _guard = self.lock_config()?;
        let Some(raw) = self.read_auth_raw()? else {
            return Ok(());
        };
        let mut document = parse_auth(&raw, &self.auth_path)?;
        if Self::migrate_document(&mut document) {
            let file = decode_auth(document, &self.auth_path)?;
            self.save_auth(&file)?;
        }
        Ok(())
    }

    /// Applies every `auth.json` migration, in the order `AuthManager.__init__` does.
    ///
    /// Returns whether anything changed, so the caller knows to write the file back.
    fn migrate_document(document: &mut serde_json::Value) -> bool {
        let mut changed = false;

        // 1. Lowercase and strip every key. Mixed-case keys in the file must match
        //    the `.strip().lower()` applied at login, or an existing user suddenly
        //    cannot sign in.
        if let Some(users) = document
            .get_mut("users")
            .and_then(serde_json::Value::as_object_mut)
        {
            let existing = std::mem::take(users);
            let mut normalised = serde_json::Map::with_capacity(existing.len());
            for (name, user) in existing {
                let key = name.trim().to_lowercase();
                if key.is_empty() {
                    changed = true;
                    continue;
                }
                // A collision means two spellings of one account; last write wins,
                // matching the dict comprehension in `_load`.
                if normalised.insert(key.clone(), user).is_some() {
                    changed = true;
                }
                if key != name {
                    changed = true;
                }
            }
            *users = normalised;
        }

        // 2. Legacy single-user shape. Guarded on `users` being absent: a file that
        //    already has a user table is not the legacy shape, and merging them
        //    would resurrect a stale top-level `password_hash`.
        let has_legacy_hash = document.get("password_hash").is_some();
        let has_users = document.get("users").is_some();
        if has_legacy_hash && !has_users {
            let Some(hash) = document.get("password_hash").cloned() else {
                return changed;
            };
            let raw_name = document
                .get("username")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("admin");
            let mut key = normalise_username(raw_name);
            // An empty or reserved name cannot become a login account; Python
            // renames both to "admin".
            if key.is_empty() || RESERVED_USERNAMES.contains(&key.as_str()) {
                tracing::warn!(legacy_user = %key, "renaming unusable legacy username to admin");
                key.clone_from(&String::from("admin"));
            }

            let admin = serde_json::json!({
                "password_hash": hash,
                "created": now_secs_f64(),
                // The single legacy account *is* the administrator.
                "is_admin": true,
            });
            let migrated_doc = serde_json::json!({ "users": { key: admin } });
            // `_migrate_single_user` replaces the whole config, dropping
            // `signup_enabled` and any unknown keys.
            *document = migrated_doc;
            changed = true;
        }

        // 3. Drop reserved and empty usernames left in the user table.
        if let Some(users) = document
            .get_mut("users")
            .and_then(serde_json::Value::as_object_mut)
        {
            let before = users.len();
            users.retain(|name, _| {
                let keep = !name.is_empty() && !RESERVED_USERNAMES.contains(&name.as_str());
                if !keep {
                    tracing::warn!(user = %name, "dropping reserved username from auth file");
                }
                keep
            });
            changed |= users.len() != before;
        }

        // 4. `setup.py`'s `role: "admin"` marker, per user. Only when `is_admin` is
        //    absent — an explicit `is_admin: false` outranks the old marker.
        if let Some(users) = document
            .get_mut("users")
            .and_then(serde_json::Value::as_object_mut)
        {
            for user in users.values_mut() {
                let Some(user) = user.as_object_mut() else {
                    continue;
                };
                let legacy_admin =
                    user.get("role").and_then(serde_json::Value::as_str) == Some("admin");
                if legacy_admin && !user.contains_key("is_admin") {
                    user.insert("is_admin".to_owned(), serde_json::Value::Bool(true));
                    changed = true;
                }
            }
        }

        changed
    }

    /// Writes `auth.json` atomically.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Io`] if the write fails.
    fn save_auth(&self, file: &AuthFile) -> Result<(), AuthError> {
        atomic_write_json(&self.auth_path, file).map_err(|source| AuthError::Io {
            path: self.auth_path.display().to_string(),
            source,
        })
    }

    /// True when at least one user exists, i.e. first-run setup is complete.
    ///
    /// # A caller must map `Err` to deny, never to `false`
    ///
    /// `Err` here means the file exists but cannot be read — corrupt, or
    /// permission-denied. Python swallows that and resets to `{}`
    /// (`core/auth.py:130-134`), which makes `is_configured` false and **reopens the
    /// first-run setup endpoint** — and on an unconfigured instance
    /// `auth_helpers.py:155-159` lets any loopback caller through `require_user` as
    /// `""`, which is the empty user that skips every privilege gate and every owner
    /// filter. Failing loudly removes that path. It is only safe because the caller
    /// treats the error as fatal; turning it into `false` here would reintroduce it.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError`] if the file cannot be read.
    pub fn is_configured(&self) -> Result<bool, AuthError> {
        Ok(!self.load_auth()?.users.is_empty())
    }

    /// Hashes a password for storage.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::PasswordTooShort`] below the minimum length and
    /// [`AuthError::Password`] if bcrypt fails.
    pub fn hash_password(password: &str) -> Result<String, AuthError> {
        if password.chars().count() < PASSWORD_MIN_LENGTH {
            return Err(AuthError::PasswordTooShort);
        }
        bcrypt::hash(password, bcrypt::DEFAULT_COST).map_err(|e| AuthError::Password(e.to_string()))
    }

    /// Verifies a password against a stored hash.
    ///
    /// Fails closed: a malformed hash is an error, never a `true`.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Password`] if the hash cannot be parsed.
    pub fn verify_password(password: &str, hash: &str) -> Result<bool, AuthError> {
        bcrypt::verify(password, hash).map_err(|e| AuthError::Password(e.to_string()))
    }

    /// Hashes a password for storage, **on the blocking pool**.
    ///
    /// Cost 12 is ~250 ms of CPU. Calling [`AuthStore::hash_password`] directly from
    /// an async handler would park a runtime worker for a quarter of a second per
    /// login, stalling SSE streams and token validation for the whole process
    /// (AGENTS.md D11). This wrapper is the intended entry point from `odysseus-web`;
    /// the sync form exists for startup work and tests.
    ///
    /// # Errors
    ///
    /// Propagates [`AuthError::PasswordTooShort`], [`AuthError::Password`], and a
    /// [`tokio::task::JoinError`] if the task is cancelled or panics.
    pub async fn hash_password_blocking(
        password: String,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        crate::blocking::spawn_blocking(move || Self::hash_password(&password))
            .await?
            .map_err(Into::into)
    }

    /// Verifies a password against a stored hash, **on the blocking pool**.
    ///
    /// # Errors
    ///
    /// As [`AuthStore::hash_password_blocking`].
    pub async fn verify_password_blocking(
        password: String,
        hash: String,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        crate::blocking::spawn_blocking(move || Self::verify_password(&password, &hash))
            .await?
            .map_err(Into::into)
    }

    /// Resolves a username to its stored record.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::NoSuchUser`] if the account does not exist.
    pub fn user(&self, username: &str) -> Result<User, AuthError> {
        let file = self.load_auth()?;
        file.users
            .get(&normalise_username(username))
            .cloned()
            .ok_or_else(|| AuthError::NoSuchUser(username.to_owned()))
    }

    /// Whether a user exists.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError`] if the auth file cannot be read.
    pub fn user_exists(&self, username: &str) -> Result<bool, AuthError> {
        Ok(self
            .load_auth()?
            .users
            .contains_key(&normalise_username(username)))
    }

    /// Creates a user, rejecting reserved names and duplicates.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Denied`] for a reserved name or an existing account,
    /// and propagates password and IO failures.
    pub fn create_user(
        &self,
        username: &str,
        password: &str,
        is_admin: bool,
    ) -> Result<(), AuthError> {
        let key = normalise_username(username);
        if RESERVED_USERNAMES.contains(&key.as_str()) {
            return Err(AuthError::Denied(format!("{key} is reserved")));
        }
        if key.is_empty() {
            return Err(AuthError::Denied("username must not be empty".into()));
        }
        let hash = Self::hash_password(password)?;

        let _guard = self.lock_config()?;

        let mut file = self.load_auth()?;
        if file.users.contains_key(&key) {
            return Err(AuthError::Denied(format!("{key} already exists")));
        }
        file.users.insert(key, User::new(hash, is_admin));
        self.save_auth(&file)
    }

    /// Sets a user's password.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::NoSuchUser`] if the account does not exist, and
    /// propagates password and IO failures.
    pub fn set_password(&self, username: &str, password: &str) -> Result<(), AuthError> {
        let hash = Self::hash_password(password)?;
        // bcrypt is deliberately run before taking the lock: it is slow enough
        // that holding the config lock across it would serialise every other
        // mutation behind one password hash.
        let _guard = self.lock_config()?;
        let mut file = self.load_auth()?;
        let key = normalise_username(username);
        let user = file
            .users
            .get_mut(&key)
            .ok_or_else(|| AuthError::NoSuchUser(username.to_owned()))?;
        user.password_hash = hash;
        self.save_auth(&file)
    }

    /// Grants or revokes admin.
    ///
    /// Refuses to demote the last admin, which would leave the instance with no
    /// way to manage users.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Denied`] when demoting the last admin, and
    /// [`AuthError::NoSuchUser`] for an unknown account.
    pub fn set_admin(&self, username: &str, is_admin: bool) -> Result<(), AuthError> {
        let _guard = self.lock_config()?;
        let mut file = self.load_auth()?;
        let key = normalise_username(username);
        let Some(currently_admin) = file.users.get(&key).map(|u| u.is_admin) else {
            return Err(AuthError::NoSuchUser(username.to_owned()));
        };

        // Promotion and demotion are no-ops when the flag already matches. Leaving
        // the privilege map alone matters: a redundant `set_admin(x, true)` on an
        // existing admin would otherwise re-stash `ADMIN_PRIVILEGES` over the
        // user's real pre-admin map and permanently lose it.
        if currently_admin == is_admin {
            return Ok(());
        }

        // Only the last *admin* is protected. Demoting an ordinary user is
        // harmless and must not be blocked just because one admin exists.
        if currently_admin && !is_admin {
            let admin_count = file.users.values().filter(|u| u.is_admin).count();
            if admin_count <= 1 {
                return Err(AuthError::Denied(
                    "cannot demote the last administrator".into(),
                ));
            }
        }

        let Some(user) = file.users.get_mut(&key) else {
            return Err(AuthError::NoSuchUser(username.to_owned()));
        };

        if is_admin {
            // Stash the pre-admin map so demotion can restore it. While
            // `is_admin` is set the stored map is inert: `privileges_for`
            // short-circuits to `Privileges::admin()`.
            let previous = if user.privileges.is_empty() {
                default_privileges_map()
            } else {
                user.privileges.clone()
            };
            user.privileges_before_admin = Some(previous);
            user.privileges = admin_privileges_map();
            user.is_admin = true;
        } else {
            // Restore the stashed map. Users created as admins have none, so their
            // stored map is `ADMIN_PRIVILEGES`; falling back to defaults is what
            // stops `can_use_bash` from leaking past the demotion.
            user.privileges = user
                .privileges_before_admin
                .take()
                .filter(|p| !p.is_empty())
                .unwrap_or_else(default_privileges_map);
            user.is_admin = false;
        }

        self.save_auth(&file)
    }

    /// Resolves a username's effective privileges.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::NoSuchUser`] for an unknown account.
    pub fn privileges(&self, username: &str) -> Result<Privileges, AuthError> {
        let user = self.user(username)?;
        Ok(Self::privileges_for(&user))
    }

    /// Resolves privileges from an already-loaded record.
    #[must_use]
    pub fn privileges_for(user: &User) -> Privileges {
        if user.is_admin {
            return Privileges::admin();
        }
        let mut p = Privileges::default();
        for (key, value) in &user.privileges {
            Self::apply_privilege(&mut p, key, value);
        }
        p
    }

    /// Applies one stored privilege override.
    ///
    /// A value of the wrong JSON type is **denied**, not silently replaced by the
    /// default. The permissive fallback would be the wrong direction: for the six
    /// capabilities that default to `true`, a corrupt `"can_use_agent": 0` would
    /// decode as *permitted*, and a corrupt `max_messages_per_day` would decode as
    /// `0` — an unlimited quota. `auth_helpers.py:186` denies in the same cases
    /// (`not privs.get(key, True)`).
    ///
    /// An unrecognised *key* is still ignored, so a newer build's privilege does
    /// not stop an older one from starting.
    fn apply_privilege(target: &mut Privileges, key: &str, value: &serde_json::Value) {
        match key {
            "can_use_agent" => target.can_use_agent = denied_unless_bool(value),
            "can_use_browser" => target.can_use_browser = denied_unless_bool(value),
            "can_use_documents" => target.can_use_documents = denied_unless_bool(value),
            "can_use_research" => target.can_use_research = denied_unless_bool(value),
            "can_generate_images" => {
                target.can_generate_images = denied_unless_bool(value);
            }
            "can_manage_memory" => target.can_manage_memory = denied_unless_bool(value),
            "can_use_bash" => target.can_use_bash = denied_unless_bool(value),
            // `routes/chat_helpers.py:220` reads
            // `cap = int(privs.get("max_messages_per_day") or 0); if cap <= 0: return`,
            // so `0` *and* any negative value are unlimited, and a non-numeric value
            // raises (a 500, i.e. no access). `as_u64` succeeds only for a
            // non-negative integer, for which Python's `int()` agrees exactly.
            //
            // The `unwrap_or(1)` therefore only fires for `null`, a string, a float or
            // a bool. Those are unlimited-or-raise in Python, and denying is the safe
            // direction: the one thing it must never do is turn a corrupted quota into
            // no quota at all.
            "max_messages_per_day" => {
                target.max_messages_per_day = value.as_u64().unwrap_or(1);
            }
            // `_allowed_models_from_privileges` (`routes/chat_helpers.py:163-169`)
            // computes `restricted = bool(privs.get("allowed_models_restricted")) or
            // bool(allowed)`. Python truthiness, not JSON booleanness, is the rule.
            "allowed_models" => match value {
                serde_json::Value::Array(_) => {
                    target.allowed_models =
                        serde_json::from_value::<Vec<String>>(value.clone()).unwrap_or_default();
                }
                // A truthy non-list is a restriction under `bool(...)`, so honour it as
                // one. Decoding it as "no restriction" would grant more than Python.
                other if !is_falsy(other) => {
                    target.allowed_models = Vec::new();
                    target.allowed_models_restricted = true;
                }
                // Empty list or empty container: Python's `bool([])` is False, so this
                // is *unrestricted*. Forcing `restricted` here would lock a legacy user
                // out of every model while Python permits all of them.
                _ => {}
            },
            // These two are restrictions, so the direction of a wrong-typed value
            // matters: `"block_all_models": "yes"` is truthy in Python and blocks every
            // model, whereas decoding it as `false` would grant *more*.
            // `set_privileges` stores whatever JSON the admin request body contained,
            // with no type validation, so a `"false"` string from the UI is enough to
            // reach this.
            "allowed_models_restricted" => {
                target.allowed_models_restricted = truthy(value);
            }
            "block_all_models" => target.block_all_models = truthy(value),
            _ => {}
        }
    }

    /// Issues a new session token for a user, persisting it.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::NoSuchUser`] for an unknown account and
    /// [`AuthError::Io`] if the session file cannot be written.
    pub fn issue_token(&self, username: &str) -> Result<String, AuthError> {
        let _guard = self.lock_sessions()?;
        if !self.user_exists(username)? {
            return Err(AuthError::NoSuchUser(username.to_owned()));
        }
        let token = random_hex(TOKEN_BYTES)?;
        let mut sessions = self.load_sessions()?;
        sessions.tokens.insert(
            token.clone(),
            SessionEntry {
                username: normalise_username(username),
                expiry: now_secs_f64() + ttl_secs_f64(),
            },
        );
        self.save_sessions(&sessions)?;
        Ok(token)
    }

    /// Validates a session token.
    ///
    /// Expired tokens are pruned. Tokens whose username has vanished are pruned
    /// too — the orphan check — so a deleted user's session does not linger.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError`] if the session file cannot be read.
    pub fn validate_token(&self, token: &str) -> Result<bool, AuthError> {
        let _guard = self.lock_sessions()?;
        if token.is_empty() {
            return Ok(false);
        }
        let mut sessions = self.read_sessions()?;

        // Expiry is checked here rather than by pre-filtering the file, so that a
        // write happens only when the *presented* token is bad -- not whenever some
        // unrelated token has aged out.
        let Some(entry) = sessions.tokens.get(token).cloned() else {
            return Ok(false);
        };
        if entry.expiry <= now_secs_f64() {
            sessions.tokens.remove(token);
            self.save_sessions(&sessions)?;
            return Ok(false);
        }

        // Orphan check: the account must still exist. `user_exists` reads
        // `auth.json` without taking `config_lock`, which is safe because writes
        // land by atomic rename -- a reader sees the whole old file or the whole new
        // one. Taking the config lock here would invert the order against
        // `delete_user`/`rename_user`, which hold config and then want sessions.
        //
        // An empty username lands here too (a `null` field decodes to `""`) and is
        // rejected, because no account is keyed on the empty string.
        if !self.user_exists(&entry.username)? {
            sessions.tokens.remove(token);
            self.save_sessions(&sessions)?;
            return Ok(false);
        }

        Ok(true)
    }

    /// Resolves a session token to its username, applying the same checks as
    /// [`AuthStore::validate_token`].
    ///
    /// Both live checks and the lookup happen under one `sessions_lock`, as
    /// `core/auth.py:629-646` does. Validating and then re-reading would be a TOCTOU:
    /// a concurrent `revoke_token` in between would yield a username for a token that
    /// was revoked after it was accepted.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError`] if the session file cannot be read.
    pub fn username_for_token(&self, token: &str) -> Result<Option<String>, AuthError> {
        let _guard = self.lock_sessions()?;
        if token.is_empty() {
            return Ok(None);
        }
        let sessions = self.load_sessions()?;
        let Some(entry) = sessions.tokens.get(token) else {
            return Ok(None);
        };
        if entry.expiry <= now_secs_f64() || !self.user_exists(&entry.username)? {
            return Ok(None);
        }
        Ok(Some(entry.username.clone()))
    }

    /// Revokes one session token.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Io`] if the session file cannot be written.
    pub fn revoke_token(&self, token: &str) -> Result<(), AuthError> {
        let _guard = self.lock_sessions()?;
        let mut sessions = self.load_sessions()?;
        if sessions.tokens.remove(token).is_some() {
            self.save_sessions(&sessions)?;
        }
        Ok(())
    }

    /// Revokes every session for a user, optionally keeping one.
    ///
    /// Returns how many tokens were dropped.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Io`] if the session file cannot be written.
    pub fn revoke_user_sessions(
        &self,
        username: &str,
        except: Option<&str>,
    ) -> Result<usize, AuthError> {
        let guard = self.lock_sessions()?;
        self.revoke_user_sessions_locked(&guard, username, except)
    }

    /// The body of [`AuthStore::revoke_user_sessions`], for callers that already hold
    /// `sessions_lock`.
    ///
    /// The lock is a *parameter*, not a naming convention. `sessions_lock` is a plain
    /// `Mutex`, so re-locking inside `delete_user`/`rename_user` would self-deadlock,
    /// and a private `_locked` suffix would stop nothing: the next `pub fn` that forgot
    // the guard would compile and silently do an unsynchronised read-modify-write of
    /// the live session file, which is the bug this shape exists to prevent.
    /// Requiring the guard makes that unrepresentable (AGENTS.md §4).
    fn revoke_user_sessions_locked(
        &self,
        _held: &MutexGuard<'_, ()>,
        username: &str,
        except: Option<&str>,
    ) -> Result<usize, AuthError> {
        let key = normalise_username(username);
        let mut sessions = self.load_sessions()?;
        let doomed: Vec<String> = sessions
            .tokens
            .iter()
            .filter(|(token, entry)| {
                // Normalised on both sides: `user_exists` normalises internally, so a
                // hand-edited `"Alice"` authenticates and must also be revocable.
                entry.username.trim().to_lowercase() == key && Some(token.as_str()) != except
            })
            .map(|(token, _)| token.clone())
            .collect();
        for token in &doomed {
            sessions.tokens.remove(token);
        }
        if !doomed.is_empty() {
            self.save_sessions(&sessions)?;
        }
        Ok(doomed.len())
    }

    /// Deletes a user and every **browser session** belonging to them.
    ///
    /// # Not yet complete — T0070
    ///
    /// API tokens are **not** revoked here, because they are not implemented (T0065).
    /// `core/auth.py:301-318` deletes the `ApiToken` rows *first* and returns without
    /// touching the user if the token store is unavailable, precisely so a token
    /// cannot outlive its owner. Until that exists, a deleted user's bearer tokens
    /// would keep working — so a caller must not treat this as full revocation.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Denied`] for the last administrator,
    /// [`AuthError::NoSuchUser`] for an unknown account, and propagates IO failures.
    pub fn delete_user(&self, username: &str) -> Result<(), AuthError> {
        let _config = self.lock_config()?;
        // Held for the whole operation, so the account edit and the session revocation
        // cannot interleave with another rename or delete.
        let sessions = self.lock_sessions()?;
        let key = normalise_username(username);
        let mut file = self.load_auth()?;
        if !file.users.contains_key(&key) {
            return Err(AuthError::NoSuchUser(username.to_owned()));
        }

        // The last admin is not deletable, for the same reason they are not
        // demotable. `set_admin` enforces one half of that invariant; this is the
        // other half.
        if file.users.get(&key).is_some_and(|u| u.is_admin)
            && file.users.values().filter(|u| u.is_admin).count() <= 1
        {
            return Err(AuthError::Denied(
                "cannot delete the last administrator".into(),
            ));
        }

        // Nor may the table become *empty*. Deleting the only non-admin user would
        // otherwise empty it just as effectively: `is_configured` flips false and the
        // first-run setup path reopens, handing admin to whoever reaches it next.
        if file.users.len() <= 1 {
            return Err(AuthError::Denied(
                "cannot delete the only user; an instance always has at least one".into(),
            ));
        }

        file.users.remove(&key);
        self.save_auth(&file)?;
        self.revoke_user_sessions_locked(&sessions, &key, None)?;
        Ok(())
    }

    /// Renames a user, carrying their sessions across.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Denied`] when the target name is empty, taken, or
    /// reserved, and propagates IO failures.
    pub fn rename_user(&self, from: &str, to: &str) -> Result<(), AuthError> {
        let _config = self.lock_config()?;
        let _sessions = self.lock_sessions()?;
        let from_key = normalise_username(from);
        let to_key = normalise_username(to);

        // `create_user` refuses an empty username; rename must too. An empty key is
        // worse than a collision: `auth_helpers.require_privilege` and
        // `owner_filter` both treat a falsy user as "no filter applied", so an ""
        // account would pass every privilege gate and see every user's data — and
        // `migrate_document` drops empty keys on the next load, deleting it anyway.
        if to_key.is_empty() {
            return Err(AuthError::Denied("username must not be empty".into()));
        }
        if RESERVED_USERNAMES.contains(&to_key.as_str()) {
            return Err(AuthError::Denied(format!("{to_key} is reserved")));
        }

        let mut file = self.load_auth()?;
        let Some(user) = file.users.remove(&from_key) else {
            return Err(AuthError::NoSuchUser(from.to_owned()));
        };
        if from_key != to_key && file.users.contains_key(&to_key) {
            file.users.insert(from_key, user);
            return Err(AuthError::Denied(format!("{to_key} already exists")));
        }
        file.users.insert(to_key.clone(), user);

        // Rewrite live sessions, or the rename silently logs the user out.
        let mut sessions = self.load_sessions()?;
        let mut touched = false;
        for entry in sessions.tokens.values_mut() {
            if entry.username.trim().to_lowercase() == from_key {
                entry.username.clone_from(&to_key);
                touched = true;
            }
        }
        self.save_auth(&file)?;
        if touched {
            self.save_sessions(&sessions)?;
        }
        Ok(())
    }

    /// Reads `sessions.json` verbatim.
    ///
    /// Expired entries are **not** dropped here. They are filtered on the way out
    /// by [`AuthStore::validate_token`] and in bulk by [`AuthStore::prune_sessions`].
    /// Pruning during the read looked harmless but meant the hot path rewrote and
    /// `fsync`ed the whole file whenever *any* token in it had expired — which, with
    /// a 7-day TTL and continuous logins, is every request, serialised behind
    /// `sessions_lock`. It also made a successful authentication depend on a
    /// successful disk write, which Python does not (`_save_sessions` swallows it).
    fn read_sessions(&self) -> Result<SessionFile, AuthError> {
        if !self.sessions_path.exists() {
            return Ok(SessionFile::default());
        }
        let raw = std::fs::read_to_string(&self.sessions_path).map_err(|source| AuthError::Io {
            path: self.sessions_path.display().to_string(),
            source,
        })?;
        serde_json::from_str(&raw).map_err(|e| AuthError::Malformed {
            path: self.sessions_path.display().to_string(),
            message: e.to_string(),
        })
    }

    fn load_sessions(&self) -> Result<SessionFile, AuthError> {
        self.read_sessions()
    }

    /// Drops expired sessions from `sessions.json` and returns how many remain.
    ///
    /// Call once at startup, as `_load_sessions:143-146` effectively does. It is
    /// what keeps the file bounded: pruning only when a token is presented would
    /// leave every abandoned login in it forever, so each authenticated request
    /// would re-parse a document that only ever grows.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Io`] if the file cannot be written.
    pub fn prune_sessions(&self) -> Result<usize, AuthError> {
        let _guard = self.lock_sessions()?;
        let mut sessions = self.read_sessions()?;
        let before = sessions.tokens.len();
        sessions
            .tokens
            .retain(|_, entry| entry.expiry > now_secs_f64());
        let remaining = sessions.tokens.len();
        if remaining != before {
            self.save_sessions(&sessions)?;
        }
        Ok(remaining)
    }

    /// Tightens the permissions of an existing `auth.json` / `sessions.json`.
    ///
    /// Writes already land at 0600, but a file created before that was true stays
    /// world-readable until something rewrites it — and the read paths never do. On
    /// an upgraded install that would leave bcrypt hashes and TOTP seeds readable by
    /// any local account indefinitely.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Io`] if `chmod` fails.
    pub fn harden_permissions(&self) -> Result<(), AuthError> {
        use std::os::unix::fs::PermissionsExt as _;

        for path in [&self.auth_path, &self.sessions_path] {
            if !path.exists() {
                continue;
            }
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
                |source| AuthError::Io {
                    path: path.display().to_string(),
                    source,
                },
            )?;
        }
        Ok(())
    }

    fn save_sessions(&self, file: &SessionFile) -> Result<(), AuthError> {
        atomic_write_json(&self.sessions_path, file).map_err(|source| AuthError::Io {
            path: self.sessions_path.display().to_string(),
            source,
        })
    }
}

/// A stored boolean *capability*, denied unless it is literally `true` or `false`.
///
/// `auth_helpers.py:186` reads `not privs.get(key, True)`, so a present-but-wrong-typed
/// value denies. An *absent* key still takes the default, which `privileges_for`
/// handles by starting from `Privileges::default()` and only visiting stored keys.
fn denied_unless_bool(value: &serde_json::Value) -> bool {
    value.as_bool().unwrap_or(false)
}

/// Python's `bool(value)`.
///
/// Needed for the two model *restriction* flags, where coercing a wrong-typed value
/// to `false` would grant more access than Python does.
fn truthy(value: &serde_json::Value) -> bool {
    !is_falsy(value)
}

/// The inverse of [`truthy`]: whether Python would treat `value` as false.
fn is_falsy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Bool(b) => !*b,
        serde_json::Value::Number(n) => n.as_f64() == Some(0.0),
        serde_json::Value::String(s) => s.is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        serde_json::Value::Object(o) => o.is_empty(),
    }
}

/// `DEFAULT_PRIVILEGES` as the JSON map stored in `auth.json`.
///
/// These are the eleven keys `auth_helpers.py` reads by name, so the map is the
/// wire format, not an implementation detail.
#[must_use]
pub fn default_privileges_map() -> BTreeMap<String, serde_json::Value> {
    BTreeMap::from([
        ("can_use_agent".into(), json!(true)),
        ("can_use_browser".into(), json!(true)),
        ("can_use_documents".into(), json!(true)),
        ("can_use_research".into(), json!(true)),
        ("can_generate_images".into(), json!(true)),
        ("can_manage_memory".into(), json!(true)),
        // Off by default: shell is remote code execution.
        ("can_use_bash".into(), json!(false)),
        ("max_messages_per_day".into(), json!(0)),
        ("allowed_models".into(), json!([])),
        ("allowed_models_restricted".into(), json!(false)),
        // An empty `allowed_models` is ambiguous — it is also what the admin gets
        // when clicking "[All]" — so "may use no model at all" needs its own flag.
        ("block_all_models".into(), json!(false)),
    ])
}

/// `ADMIN_PRIVILEGES` as the JSON map stored in `auth.json`.
///
/// Python builds this with a dict comprehension that flips every boolean default
/// to `true`, then explicitly resets the two model sentinels — an admin must
/// never be blocked from using models.
#[must_use]
pub fn admin_privileges_map() -> BTreeMap<String, serde_json::Value> {
    let mut map = default_privileges_map();
    for value in map.values_mut() {
        if value.is_boolean() {
            *value = json!(true);
        }
    }
    map.insert("allowed_models_restricted".into(), json!(false));
    map.insert("block_all_models".into(), json!(false));
    map
}

/// Parses an `auth.json` document.
///
/// Migrations run on this raw `Value`, not on `AuthFile`. Two of them depend on
/// *key absence* — `"is_admin" not in user`, `"users" not in config` — which a
/// typed struct with `#[serde(default)]` cannot represent, because a defaulted
/// `false` is indistinguishable from an explicit `false`.
fn parse_auth(raw: &str, path: &Path) -> Result<serde_json::Value, AuthError> {
    serde_json::from_str(raw).map_err(|e| AuthError::Malformed {
        path: path.display().to_string(),
        message: e.to_string(),
    })
}

/// Decodes a migrated document into the typed form.
fn decode_auth(document: serde_json::Value, path: &Path) -> Result<AuthFile, AuthError> {
    serde_json::from_value(document).map_err(|e| AuthError::Malformed {
        path: path.display().to_string(),
        message: e.to_string(),
    })
}

/// Lowercases and trims a username, which is how it is keyed.
fn normalise_username(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Seconds since the Unix epoch.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Seconds since the Unix epoch, as the `f64` that Python's `time.time()`
/// produces. Both files store timestamps as JSON floats, and integers
/// serialise as `1.7e9` rather than `1758000000.0`, so reading them as `f64`
/// keeps the value identical.
fn now_secs_f64() -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "f64 holds integers exactly up to 2^53; Unix seconds exceed that in the year 287396, so the rounding is unreachable for any timestamp this program will ever produce"
    )]
    let secs = now_secs() as f64;
    secs
}

/// Expands the TTL into the same `f64` seconds used for `expiry`.
fn ttl_secs_f64() -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "604800 is well inside the 2^53 range f64 represents exactly, so this cast is lossless"
    )]
    let ttl = TOKEN_TTL_SECONDS as f64;
    ttl
}

/// `n` random bytes as lowercase hex, matching `secrets.token_hex(n)`.
///
/// # Errors
///
/// Returns [`AuthError::Password`] if the OS entropy source fails. Failing closed
/// matters here: falling back to a predictable token would hand out a session
/// anyone could guess.
fn random_hex(n: usize) -> Result<String, AuthError> {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes)
        .map_err(|e| AuthError::Password(format!("OS randomness unavailable: {e}")))?;
    Ok(bytes
        .iter()
        .fold(String::with_capacity(n * 2), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        }))
}

#[cfg(test)]
mod tests {
    use {
        super::{
            AuthError, AuthStore, PASSWORD_MIN_LENGTH, Privileges, RESERVED_USERNAMES,
            SessionEntry, SessionFile, TOKEN_BYTES, TOKEN_TTL_SECONDS, User, normalise_username,
            ttl_secs_f64,
        },
        serde_json::json,
        std::collections::BTreeMap,
    };

    fn store() -> (tempfile::TempDir, AuthStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = AuthStore::new(dir.path());
        (dir, store)
    }

    /// The live session map, for tests that need to age a token out.
    fn read_sessions(store: &AuthStore) -> SessionFile {
        store.load_sessions().expect("load")
    }

    /// A store over a temp dir seeded with one raw `auth.json`.
    fn new_store() -> (tempfile::TempDir, AuthStore) {
        store()
    }

    /// A store over a temp dir seeded with one raw `auth.json`.
    fn store_with_raw(contents: &str) -> (tempfile::TempDir, AuthStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = AuthStore::new(dir.path());
        std::fs::write(store.auth_path(), contents).expect("seed");
        (dir, store)
    }

    #[test]
    fn hash_and_verify_round_trip() {
        let hash = AuthStore::hash_password("correct horse").expect("hash");
        assert!(AuthStore::verify_password("correct horse", &hash).expect("verify"));
        assert!(!AuthStore::verify_password("wrong horse", &hash).expect("verify"));
    }

    #[test]
    fn short_passwords_are_refused() {
        let short = "a".repeat(PASSWORD_MIN_LENGTH - 1);
        assert!(matches!(
            AuthStore::hash_password(&short),
            Err(AuthError::PasswordTooShort)
        ));
    }

    #[test]
    fn a_malformed_hash_fails_closed() {
        // Never returns true: an unparseable hash must deny, not wave through.
        assert!(AuthStore::verify_password("anything", "not-a-bcrypt-hash").is_err());
    }

    #[test]
    fn create_and_read_back_a_user() {
        let (_dir, store) = store();
        assert!(!store.is_configured().expect("configured"));
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        assert!(store.is_configured().expect("configured"));

        let user = store.user("alice").expect("user");
        assert!(!user.is_admin);
        assert!(AuthStore::verify_password("hunter2hunter2", &user.password_hash).expect("v"));
    }

    #[test]
    fn usernames_are_case_insensitive() {
        let (_dir, store) = store();
        store
            .create_user("Alice", "hunter2hunter2", false)
            .expect("create");
        assert!(store.user_exists("alice").expect("exists"));
        assert!(store.user_exists("ALICE").expect("exists"));
        assert_eq!(normalise_username("  MiXeD  "), "mixed");
    }

    #[test]
    fn reserved_usernames_are_refused_on_create() {
        let (_dir, store) = store();
        for name in RESERVED_USERNAMES {
            let err = store
                .create_user(name, "hunter2hunter2", false)
                .expect_err("must be refused");
            assert!(
                matches!(err, AuthError::Denied(_)),
                "{name} was not refused"
            );
        }
    }

    #[test]
    fn a_reserved_name_in_the_file_is_dropped_on_load() {
        let (_dir, store) = store();
        // Simulate a hand-edited or bad-restore file.
        store
            .save_auth(
                &serde_json::from_value(serde_json::json!({
                    "users": {
                        "alice": { "password_hash": "x", "created": 1.0, "is_admin": true },
                        "api": { "password_hash": "x", "created": 1.0, "is_admin": false }
                    }
                }))
                .expect("json"),
            )
            .expect("save");
        assert!(store.user_exists("alice").expect("exists"));
        assert!(!store.user_exists("api").expect("exists"));
    }

    #[test]
    fn the_legacy_single_user_shape_is_migrated() {
        let (_dir, store) = store();
        std::fs::write(
            store.auth_path(),
            r#"{"username": "legacy", "password_hash": "$2b$12$abcdefghijklmnopqrstuv"}"#,
        )
        .expect("write");
        let user = store.user("legacy").expect("legacy user must load");
        // `core/auth.py:_migrate_single_user` makes the sole legacy account an
        // *administrator*. Treating it as an ordinary user would leave a migrated
        // instance with no admin at all.
        assert!(user.is_admin, "legacy single user must become admin");
        assert_eq!(user.password_hash, "$2b$12$abcdefghijklmnopqrstuv");

        // Python persists each migration inside the migration itself
        // (`self._save()`). Here that is the explicit `migrate_and_persist` step,
        // because a read must not write: `load_auth` is reached while `sessions_lock`
        // is held, and writing there would need `config_lock` in the wrong order.
        store.migrate_and_persist().expect("persist migration");
        let on_disk = std::fs::read_to_string(store.auth_path()).expect("read");
        let doc: serde_json::Value = serde_json::from_str(&on_disk).expect("valid json");
        assert!(doc.get("username").is_none(), "top-level username removed");
        assert!(
            doc.get("password_hash").is_none(),
            "top-level password_hash removed"
        );
        assert_eq!(
            doc["users"]["legacy"]["password_hash"],
            "$2b$12$abcdefghijklmnopqrstuv"
        );
        assert_eq!(doc["users"]["legacy"]["is_admin"], true);
    }

    #[test]
    fn a_legacy_reserved_username_is_renamed_to_admin() {
        // `core/auth.py:_migrate_single_user` maps a reserved legacy name to
        // "admin" rather than creating an account that shadows an internal
        // identity such as "api" or "internal-tool".
        let (_dir, store) = store();
        std::fs::write(
            store.auth_path(),
            r#"{"username": "api", "password_hash": "$2b$12$abcdefghijklmnopqrstuv"}"#,
        )
        .expect("write");

        assert!(!store.user_exists("api").expect("api must not exist"));
        let admin = store.user("admin").expect("migrated to admin");
        assert!(admin.is_admin);
        assert_eq!(admin.password_hash, "$2b$12$abcdefghijklmnopqrstuv");
    }

    #[test]
    fn a_top_level_password_hash_alongside_users_is_not_treated_as_legacy() {
        // The legacy migration is guarded on `users` being absent. A file that has
        // both a real user table and a stale top-level `password_hash` must not
        // sprout an extra account.
        let (_dir, store) = store();
        std::fs::write(
            store.auth_path(),
            r#"{"username": "ghost", "password_hash": "$2b$12$stale", "users": {"real": {"password_hash": "$2b$12$real"}}}"#,
        )
        .expect("write");

        assert!(store.user_exists("real").expect("real exists"));
        assert!(
            !store
                .user_exists("ghost")
                .expect("ghost must not be created")
        );
    }

    #[test]
    fn the_legacy_role_field_becomes_is_admin() {
        let (_dir, store) = store();
        std::fs::write(
            store.auth_path(),
            r#"{"users": {"old": {"password_hash": "x", "role": "admin"}}}"#,
        )
        .expect("write");
        // `role` is a *per-user* field. Only consulted when `is_admin` is absent,
        // because an explicit `is_admin: false` outranks the old marker.
        let file = store.load_auth().expect("load");
        assert!(file.users["old"].is_admin, "role=admin must promote");
    }

    #[test]
    fn an_explicit_is_admin_false_outranks_the_legacy_role_marker() {
        let (_dir, store) = store();
        std::fs::write(
            store.auth_path(),
            r#"{"users": {"old": {"password_hash": "x", "role": "admin", "is_admin": false}}}"#,
        )
        .expect("write");
        let file = store.load_auth().expect("load");
        assert!(
            !file.users["old"].is_admin,
            "explicit false must win over role=admin"
        );
    }

    #[test]
    fn tokens_are_64_hex_chars_and_validate() {
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let token = store.issue_token("alice").expect("issue");

        assert_eq!(token.len(), TOKEN_BYTES * 2);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(store.validate_token(&token).expect("validate"));
        assert_eq!(
            store.username_for_token(&token).expect("who"),
            Some("alice".to_owned())
        );
    }

    #[test]
    fn an_unknown_token_is_rejected() {
        let (_dir, store) = store();
        assert!(!store.validate_token(&"0".repeat(64)).expect("validate"));
        assert!(!store.validate_token("").expect("empty"));
    }

    /// The orphan check: a token whose user has been deleted must not survive.
    #[test]
    fn deleting_a_user_invalidates_their_sessions() {
        let (_dir, store) = store();
        // A second account, so the delete is not refused by the "table must not be
        // emptied" guard -- that refusal is covered separately.
        store
            .create_user("root", "hunter2hunter2", true)
            .expect("create admin");
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let token = store.issue_token("alice").expect("issue");
        assert!(store.validate_token(&token).expect("valid before"));

        store.delete_user("alice").expect("delete");

        assert!(
            !store.validate_token(&token).expect("valid after"),
            "a token for a deleted user must not authenticate"
        );
    }

    #[test]
    fn an_expired_token_is_rejected() {
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let token = store.issue_token("alice").expect("issue");

        // Rewrite the expiry into the past.
        let raw = std::fs::read_to_string(store.sessions_path()).expect("read");
        let mut doc: serde_json::Value = serde_json::from_str(&raw).expect("json");
        doc[&token]["expiry"] = serde_json::json!(1.0);
        std::fs::write(
            store.sessions_path(),
            serde_json::to_string(&doc).expect("serialise"),
        )
        .expect("write");

        assert!(!store.validate_token(&token).expect("expired"));
        // And it must have been pruned, not just refused.
        let after = std::fs::read_to_string(store.sessions_path()).expect("read");
        assert!(!after.contains(&token), "expired token must be pruned");
    }

    #[test]
    fn tokens_last_seven_days() {
        assert_eq!(TOKEN_TTL_SECONDS, 7 * 24 * 60 * 60);
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let token = store.issue_token("alice").expect("issue");
        let raw = std::fs::read_to_string(store.sessions_path()).expect("read");
        let doc: serde_json::Value = serde_json::from_str(&raw).expect("json");
        let expiry = doc[&token]["expiry"].as_f64().expect("expiry");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs_f64();
        assert!(
            (expiry - now - ttl_secs_f64()).abs() < 5.0,
            "expiry should be 7 days out, got {expiry} - {now}"
        );
    }

    #[test]
    fn revoke_clears_one_token_and_leaves_others() {
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let a = store.issue_token("alice").expect("a");
        let b = store.issue_token("alice").expect("b");

        store.revoke_token(&a).expect("revoke");
        assert!(!store.validate_token(&a).expect("revoked"));
        assert!(store.validate_token(&b).expect("other survives"));
    }

    #[test]
    fn revoke_user_sessions_can_keep_one() {
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let keep = store.issue_token("alice").expect("keep");
        store.issue_token("alice").expect("drop1");
        store.issue_token("alice").expect("drop2");

        let removed = store
            .revoke_user_sessions("alice", Some(&keep))
            .expect("revoke");
        assert_eq!(removed, 2);
        assert!(store.validate_token(&keep).expect("kept token survives"));
    }

    #[test]
    fn rename_carries_sessions_across() {
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let token = store.issue_token("alice").expect("issue");

        store.rename_user("alice", "bob").expect("rename");

        assert!(store.validate_token(&token).expect("token survives rename"));
        assert_eq!(
            store.username_for_token(&token).expect("who"),
            Some("bob".to_owned()),
            "the session must follow the renamed account"
        );
    }

    #[test]
    fn rename_onto_an_existing_name_is_refused_and_reversible() {
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        store
            .create_user("bob", "hunter2hunter2", false)
            .expect("create");

        assert!(store.rename_user("alice", "bob").is_err());
        assert!(
            store.user_exists("alice").expect("alice must still exist"),
            "a refused rename must not lose the account"
        );
    }

    #[test]
    fn the_last_admin_cannot_be_demoted() {
        let (_dir, store) = store();
        store
            .create_user("root", "hunter2hunter2", true)
            .expect("create");
        let err = store.set_admin("root", false).expect_err("must refuse");
        assert!(matches!(err, AuthError::Denied(_)));
        assert!(store.user("root").expect("root").is_admin, "still admin");

        // With a second admin, demotion is allowed.
        store
            .create_user("second", "hunter2hunter2", true)
            .expect("create");
        store.set_admin("second", false).expect("demote");
        assert!(!store.user("second").expect("second").is_admin);
    }

    #[test]
    fn demoting_an_ordinary_user_is_not_blocked_by_the_last_admin_rule() {
        // Regression: the last-admin guard must key off whether the *target* is an
        // admin, not merely on the admin count. Counting alone refused to demote a
        // user who was never an admin, locking ordinary accounts forever.
        let (_dir, store) = store();
        store
            .create_user("root", "hunter2hunter2", true)
            .expect("create admin");
        store
            .create_user("bob", "hunter2hunter2", false)
            .expect("create ordinary");

        // `bob` is not an admin, so this must succeed even though `root` is the
        // only admin.
        store.set_admin("bob", true).expect("promote");
        assert!(store.user("bob").expect("bob").is_admin);

        store.set_admin("bob", false).expect("demote back");
        assert!(!store.user("bob").expect("bob").is_admin);
        assert!(store.user("root").expect("root").is_admin, "root untouched");
    }

    #[test]
    fn demoting_an_admin_restores_their_pre_admin_privileges() {
        // `core/auth.py:set_admin` stashes `privileges_before_admin` on promotion
        // and restores it on demotion. Without the stash, a temporary admin stint
        // silently promotes the user to the *permanent* admin privilege map — which
        // includes `can_use_bash`. That is remote code execution handed to a user
        // who was demoted specifically to take it away.
        let (_dir, store) = store();
        store
            .create_user("root", "hunter2hunter2", true)
            .expect("create admin");
        store
            .create_user("bob", "hunter2hunter2", false)
            .expect("create ordinary");

        assert!(!store.privileges("bob").expect("privs").can_use_bash);

        store.set_admin("bob", true).expect("promote");
        assert!(
            store.privileges("bob").expect("privs").can_use_bash,
            "an admin does get bash while promoted"
        );

        store.set_admin("bob", false).expect("demote");
        let after = store.privileges("bob").expect("privs");
        assert!(!after.can_use_bash, "bash must not survive demotion");
        assert!(after.can_use_agent, "ordinary privileges must come back");
        assert_eq!(after, Privileges::default());
    }

    #[test]
    fn demoting_a_user_created_as_admin_falls_back_to_defaults_not_admin_privileges() {
        // A user created *as* an admin has no stash: their stored map is already
        // ADMIN_PRIVILEGES. Restoring that would leave `can_use_bash` on forever.
        let (_dir, store) = store();
        store
            .create_user("root", "hunter2hunter2", true)
            .expect("create");
        store
            .create_user("second", "hunter2hunter2", true)
            .expect("create second admin");

        store.set_admin("second", false).expect("demote");
        assert_eq!(
            store.privileges("second").expect("privs"),
            Privileges::default(),
            "must fall back to DEFAULT_PRIVILEGES, not ADMIN_PRIVILEGES"
        );
    }

    #[test]
    fn a_redundant_promotion_does_not_destroy_the_stashed_privileges() {
        // Python returns early when `currently_admin == is_admin`. If a redundant
        // `set_admin(bob, true)` re-stashed, the real pre-admin map would be
        // overwritten with ADMIN_PRIVILEGES and lost forever.
        let (_dir, store) = store();
        store
            .create_user("root", "hunter2hunter2", true)
            .expect("create");
        store
            .create_user("bob", "hunter2hunter2", false)
            .expect("create");

        // Narrow bob's privileges first, so the restore is observable.
        let mut file = store.load_auth().expect("load");
        file.users.get_mut("bob").expect("bob").privileges = BTreeMap::from([
            ("can_use_agent".into(), json!(false)),
            ("can_use_bash".into(), json!(false)),
            ("max_messages_per_day".into(), json!(7)),
        ]);
        store.save_auth(&file).expect("save");

        store.set_admin("bob", true).expect("promote");
        store
            .set_admin("bob", true)
            .expect("redundant promote is a no-op");
        store.set_admin("bob", false).expect("demote");

        let after = store.privileges("bob").expect("privs");
        assert!(!after.can_use_agent, "narrowed agent access must survive");
        assert_eq!(after.max_messages_per_day, 7, "custom cap must survive");
    }

    #[test]
    fn concurrent_user_creation_keeps_every_account() {
        // Regression: without `config_lock`, N threads each read the same
        // `auth.json` and write it back, so all but the last writer's user vanish.
        let dir = tempfile::tempdir().expect("tempdir");
        let store = AuthStore::new(dir.path());

        std::thread::scope(|scope| {
            for i in 0..12 {
                let store = store.clone();
                scope.spawn(move || {
                    store
                        .create_user(&format!("user{i}"), "hunter2hunter2", false)
                        .expect("create");
                });
            }
        });

        let file = store.load_auth().expect("load");
        assert_eq!(
            file.users.len(),
            12,
            "lost updates: {:?}",
            file.users.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn rename_cannot_create_an_empty_username() {
        // An empty key is worse than a collision: `auth_helpers.require_privilege`
        // and `owner_filter` both treat a falsy user as "no filter applied", so an
        // "" account would pass every privilege gate and read every user's data.
        // `migrate_document` also drops empty keys on the next load, deleting it.
        let (_dir, store) = store();
        store
            .create_user("root", "hunter2hunter2", true)
            .expect("create");

        for bad in ["", "   ", "\t\n "] {
            let err = store.rename_user("root", bad).expect_err("must refuse");
            assert!(matches!(err, AuthError::Denied(_)), "{bad:?} gave {err:?}");
        }

        // The account must be intact, not silently consumed by the failed rename.
        assert!(store.user("root").expect("root survives").is_admin);
        assert!(!store.user_exists("").expect("no empty account"));
    }

    #[test]
    fn the_last_admin_cannot_be_deleted() {
        // The other half of the invariant `set_admin` protects. Without it the user
        // table can reach zero, `is_configured` flips false, and the first-run setup
        // path reopens — handing admin to whoever reaches it next.
        let (_dir, store) = store();
        store
            .create_user("root", "hunter2hunter2", true)
            .expect("create");

        let err = store.delete_user("root").expect_err("must refuse");
        assert!(matches!(err, AuthError::Denied(_)), "{err:?}");
        assert!(store.user_exists("root").expect("root survives"));

        // With a second admin, deleting an ordinary user still works.
        store
            .create_user("bob", "hunter2hunter2", false)
            .expect("create");
        store.delete_user("bob").expect("delete");
        assert!(!store.user_exists("bob").expect("gone"));
    }

    #[test]
    fn a_malformed_privilege_value_denies_rather_than_defaulting() {
        // Fail-open risk: `can_use_agent` defaults to true, so a corrupt non-boolean
        // would decode as *permitted*. Python denies (`not privs.get(key, True)`).
        let (_dir, store) = store();
        let mut file = store.load_auth().expect("load");
        file.users.insert(
            "alice".to_owned(),
            User {
                privileges: BTreeMap::from([
                    ("can_use_agent".into(), json!(0)),
                    ("can_use_bash".into(), json!("yes")),
                    ("max_messages_per_day".into(), json!("lots")),
                    ("allowed_models".into(), json!({"not": "a list"})),
                ]),
                ..User::new("$2b$12$abc".to_owned(), false)
            },
        );
        store.save_auth(&file).expect("save");

        let privs = store.privileges("alice").expect("privs");
        assert!(!privs.can_use_agent, "a corrupt flag must not permit");
        assert!(!privs.can_use_bash, "a corrupt flag must not permit");
        assert_eq!(
            privs.max_messages_per_day, 1,
            "a corrupt quota must deny, not become unlimited"
        );
        assert!(
            privs.allowed_models_restricted,
            "an undecodable model list must not mean 'no restriction'"
        );
    }

    #[test]
    fn a_quota_of_zero_still_means_unlimited() {
        // `DEFAULT_PRIVILEGES["max_messages_per_day"] = 0` means unlimited, so the
        // deny-on-corrupt rule must not swallow the one legitimate non-positive value.
        let (_dir, store) = store();
        let mut file = store.load_auth().expect("load");
        file.users.insert(
            "alice".to_owned(),
            User {
                privileges: BTreeMap::from([("max_messages_per_day".into(), json!(0))]),
                ..User::new("$2b$12$abc".to_owned(), false)
            },
        );
        store.save_auth(&file).expect("save");
        assert_eq!(
            store
                .privileges("alice")
                .expect("privs")
                .max_messages_per_day,
            0,
            "0 is unlimited"
        );
    }

    #[test]
    fn debug_output_redacts_the_hash_and_totp_secrets() {
        // A leaked base32 TOTP seed regenerates valid codes forever, and no session
        // revocation undoes it, so `Debug` must never carry it (AGENTS.md §4, §10).
        let user = User {
            totp_secret: Some("JBSWY3DPEHPK3PXP".to_owned()),
            totp_secret_pending: Some("KRSXG5CTMVRXEZLU".to_owned()),
            totp_backup_codes: Some(vec!["a".into(), "b".into()]),
            ..User::new("$2b$12$abcdefghijklmnopqrstuv".to_owned(), false)
        };
        let rendered = format!("{user:?}");

        assert!(!rendered.contains("JBSWY3DPEHPK3PXP"), "{rendered}");
        assert!(!rendered.contains("KRSXG5CTMVRXEZLU"), "{rendered}");
        assert!(!rendered.contains("$2b$12$"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn expired_sessions_are_pruned_from_disk_without_any_token_being_presented() {
        // Python prunes at load (`_load_sessions:143-146`). Pruning only on
        // presentation would leave every abandoned login in the file forever.
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        store.issue_token("alice").expect("issue");

        // Expire it by rewriting the file directly.
        let mut doc = read_sessions(&store);
        let stale = "e".repeat(64);
        doc.tokens.insert(
            stale.clone(),
            SessionEntry {
                username: "alice".to_owned(),
                expiry: 1.0,
            },
        );
        store.save_sessions(&doc).expect("write");

        let remaining = store.prune_sessions().expect("prune");
        assert_eq!(remaining, 1, "only the live token should remain");
        assert!(
            !store.validate_token(&stale).expect("validate"),
            "the expired token must be rejected after the prune"
        );
    }

    #[test]
    fn a_session_entry_with_null_fields_expires_instead_of_breaking_the_file() {
        // `#[serde(default)]` covers a missing key but not an explicit `null`, and
        // one unparseable entry would otherwise fail the whole document — turning
        // every authenticated request into a 500.
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let live = store.issue_token("alice").expect("issue");

        let contents = format!(
            r#"{{"{live}": {{"username": "alice", "expiry": 4102444800.0}}, "nulled": {{"username": null, "expiry": null}}}}"#
        );
        std::fs::write(store.sessions_path(), contents).expect("write");

        assert!(
            store
                .validate_token(&live)
                .expect("a null entry must not break the file"),
            "the valid token must still work"
        );
        assert!(!store.validate_token("nulled").expect("null entry expires"));
    }

    #[test]
    fn debug_output_never_prints_a_live_session_token() {
        // The token *is* the map key, so a derived `Debug` on `SessionFile` would put
        // every usable 7-day credential in the log verbatim.
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let token = store.issue_token("alice").expect("issue");

        let rendered = format!("{:?}", store.load_sessions().expect("load"));
        assert!(!rendered.contains(&token), "token leaked: {rendered}");
        assert!(rendered.contains("token_count"), "{rendered}");
    }

    #[test]
    fn an_unparseable_session_entry_does_not_break_the_whole_file() {
        // One element of the wrong type — `null`, a number, a string — used to fail
        // the parse of the entire document, and since `validate_token` reads this
        // file on every request, a single bad byte bricked auth for every user.
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let live = store.issue_token("alice").expect("issue");

        let contents = format!(
            r#"{{"{live}": {{"username": "alice", "expiry": 4102444800.0}}, "junk1": null, "junk2": 5, "junk3": "x"}}"#
        );
        std::fs::write(store.sessions_path(), contents).expect("write");

        assert!(
            store
                .validate_token(&live)
                .expect("a junk entry must not break the file"),
            "the valid token must still authenticate"
        );
        assert!(!store.validate_token("junk1").expect("junk"));
        assert!(!store.validate_token("junk2").expect("junk"));
        assert!(!store.validate_token("junk3").expect("junk"));
    }

    #[test]
    fn a_null_field_in_auth_json_does_not_break_the_instance() {
        // `#[serde(default)]` covers a missing key but not an explicit `null`. One
        // null byte used to make `decode_auth` fail, which bricks `is_configured`,
        // every user lookup, and `validate_token`'s orphan check — permanently.
        let (_dir, store) = store_with_raw(
            r#"{"signup_enabled": null, "users": {"alice": {"password_hash": "$2b$12$abc", "is_admin": null, "created": null, "privileges": null, "totp_enabled": null}}}"#,
        );

        assert!(
            store
                .is_configured()
                .expect("null fields must not brick the file")
        );
        let user = store.user("alice").expect("user");
        assert!(!user.is_admin);
        assert!(!user.totp_enabled);
        assert_eq!(
            store.privileges("alice").expect("privs"),
            Privileges::default()
        );
    }

    #[test]
    fn an_unknown_per_user_key_survives_a_write() {
        // `AuthFile` had `#[serde(flatten)] extra` but `User` did not, so any single
        // mutation — for any user — silently erased every undeclared per-user field
        // for *every* user on the next save.
        let (_dir, store) = store_with_raw(
            r#"{"users": {"alice": {"password_hash": "$2b$12$abc", "is_admin": false, "future_field": {"nested": [1, 2]}}}}"#,
        );

        store.set_admin("alice", true).expect("promote");

        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(store.auth_path()).expect("read"))
                .expect("json");
        assert_eq!(
            on_disk["users"]["alice"]["future_field"]["nested"],
            serde_json::json!([1, 2]),
            "unknown per-user key was dropped: {on_disk}"
        );
    }

    #[test]
    fn a_wrong_typed_restriction_flag_blocks_rather_than_grants() {
        // The inverse of the capability case, and the one place a deny-on-corrupt rule
        // is not fail-closed. `"block_all_models": "yes"` is truthy in Python and
        // blocks every model; decoding it as `false` grants *more*. `set_privileges`
        // stores unvalidated request JSON, so a string from the UI reaches this.
        let (_dir, store) = store_with_raw(
            r#"{"users": {"alice": {"password_hash": "$2b$12$abc", "is_admin": false,
                "privileges": {"block_all_models": "yes", "allowed_models_restricted": "true"}}}}"#,
        );

        let privs = store.privileges("alice").expect("privs");
        assert!(privs.block_all_models, "a truthy flag must block");
        assert!(
            privs.allowed_models_restricted,
            "a truthy flag must restrict"
        );
    }

    #[test]
    fn an_empty_allowed_models_list_means_unrestricted_like_python() {
        // `_allowed_models_from_privileges` computes
        // `restricted = bool(privs.get("allowed_models_restricted")) or bool(allowed)`,
        // and `bool([])` is False. An earlier attempt forced `restricted` here, which
        // locked a legacy user out of every model while Python permits all of them.
        let (_dir, store) = store_with_raw(
            r#"{"users": {"alice": {"password_hash": "$2b$12$abc", "is_admin": false,
                "privileges": {"allowed_models": []}}}}"#,
        );

        let privs = store.privileges("alice").expect("privs");
        assert!(
            !privs.allowed_models_restricted,
            "an empty list is unrestricted in Python"
        );
    }

    #[test]
    fn the_user_table_can_never_be_emptied() {
        // Deleting the only *non-admin* emptied the table just as effectively as
        // deleting the last admin: `is_configured` flips false and first-run setup
        // reopens.
        let (_dir, store) = store();
        store
            .create_user("root", "hunter2hunter2", true)
            .expect("create");
        store
            .create_user("bob", "hunter2hunter2", false)
            .expect("create");

        store.delete_user("bob").expect("delete the middle user");
        assert!(store.is_configured().expect("still configured"));

        let err = store.delete_user("root").expect_err("last admin");
        assert!(matches!(err, AuthError::Denied(_)), "{err:?}");

        // A sole non-admin is equally protected.
        let (_d2, single) = new_store();
        single
            .create_user("solo", "hunter2hunter2", false)
            .expect("create");
        let err = single.delete_user("solo").expect_err("only user");
        assert!(matches!(err, AuthError::Denied(_)), "{err:?}");
        assert!(single.is_configured().expect("still configured"));
    }

    #[test]
    fn validate_token_does_not_write_when_the_token_is_good() {
        // A regression guard on the hot path. Pruning during the read looked harmless
        // but meant every request re-serialised and `fsync`ed the whole session file
        // whenever *any* token had expired, serialised behind `sessions_lock` — and
        // made a successful login depend on a successful disk write.
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let live = store.issue_token("alice").expect("issue");

        // An unrelated, already-expired entry sits in the file.
        let mut doc = read_sessions(&store);
        doc.tokens.insert(
            "f".repeat(64),
            SessionEntry {
                username: "alice".to_owned(),
                expiry: 1.0,
            },
        );
        store.save_sessions(&doc).expect("seed");

        let before = std::fs::metadata(store.sessions_path())
            .expect("stat")
            .modified()
            .expect("mtime");
        assert!(store.validate_token(&live).expect("validate"));
        let after = std::fs::metadata(store.sessions_path())
            .expect("stat")
            .modified()
            .expect("mtime");

        assert_eq!(before, after, "a valid token must not rewrite the file");
    }

    #[test]
    fn a_hand_edited_mixed_case_username_is_still_revokable() {
        // `user_exists` normalises internally, so a session written as `"Alice"`
        // authenticates; a "revoke all sessions" that compared raw strings would miss
        // it.
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let token = store.issue_token("alice").expect("issue");

        let mut doc = read_sessions(&store);
        doc.tokens.get_mut(&token).expect("entry").username = "Alice".to_owned();
        store.save_sessions(&doc).expect("write");

        assert!(store.validate_token(&token).expect("still valid"));
        assert_eq!(
            store.revoke_user_sessions("alice", None).expect("revoke"),
            1,
            "a mixed-case session must still be revoked"
        );
        assert!(!store.validate_token(&token).expect("revoked"));
    }

    #[test]
    fn rename_moves_a_hand_edited_mixed_case_session() {
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let token = store.issue_token("alice").expect("issue");
        let mut doc = read_sessions(&store);
        doc.tokens.get_mut(&token).expect("entry").username = "ALICE".to_owned();
        store.save_sessions(&doc).expect("write");

        store.rename_user("alice", "bob").expect("rename");
        assert!(
            store
                .validate_token(&token)
                .expect("session must survive the rename"),
            "a mixed-case session must be carried across, not orphaned"
        );
    }

    #[test]
    fn harden_permissions_tightens_a_loose_existing_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        // sessions.json only exists once a token has been written.
        store.issue_token("alice").expect("issue");
        for path in [store.auth_path(), store.sessions_path()] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).expect("loosen");
        }

        store.harden_permissions().expect("harden");
        for path in [store.auth_path(), store.sessions_path()] {
            let mode = std::fs::metadata(path).expect("stat").permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "{} was {:o}",
                path.display(),
                mode & 0o777
            );
        }
    }

    #[test]
    fn concurrent_token_issuance_keeps_every_session() {
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");

        let tokens: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..12)
                .map(|_| {
                    let store = store.clone();
                    scope.spawn(move || store.issue_token("alice").expect("issue"))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("join"))
                .collect()
        });

        assert_eq!(
            tokens
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            12
        );
        let sessions = store.load_sessions().expect("load");
        assert_eq!(sessions.tokens.len(), 12, "lost sessions");
        for token in &tokens {
            assert!(store.validate_token(token).expect("validate"));
        }
    }

    #[test]
    fn set_admin_on_an_unknown_user_is_an_error_not_a_silent_no_op() {
        let (_dir, store) = store();
        store
            .create_user("root", "hunter2hunter2", true)
            .expect("create");
        let err = store.set_admin("nobody", true).expect_err("must refuse");
        assert!(matches!(err, AuthError::NoSuchUser(_)), "{err:?}");
    }

    #[test]
    fn admins_get_every_privilege_including_shell() {
        let (_dir, store) = store();
        store
            .create_user("root", "hunter2hunter2", true)
            .expect("create");
        let p = store.privileges("root").expect("privileges");
        assert_eq!(p, Privileges::admin());
        assert!(p.can_use_bash, "an admin may use the shell");
        assert!(p.get("can_use_bash"));
    }

    #[test]
    fn shell_is_off_by_default_for_ordinary_users() {
        let p = Privileges::default();
        assert!(
            !p.can_use_bash,
            "shell is remote code execution and must be opt-in"
        );
        assert!(p.can_use_agent);
    }

    #[test]
    fn an_unknown_privilege_key_is_permissive() {
        // Matches auth_helpers.py:169, so a newer privilege name cannot lock
        // every user out of an older build. Documented because it reads like a
        // mistake.
        assert!(Privileges::default().get("can_do_something_new"));
    }

    #[test]
    fn privilege_overrides_are_applied() {
        let mut user = User::new("hash".into(), false);
        user.privileges
            .insert("can_use_bash".into(), serde_json::json!(true));
        user.privileges
            .insert("can_use_research".into(), serde_json::json!(false));
        user.privileges
            .insert("allowed_models".into(), serde_json::json!(["a", "b"]));

        let p = AuthStore::privileges_for(&user);
        assert!(p.can_use_bash);
        assert!(!p.can_use_research);
        assert_eq!(p.allowed_models, vec!["a", "b"]);
        assert!(p.can_use_agent, "unlisted privileges keep their default");
    }

    #[test]
    fn a_corrupt_auth_file_is_reported_not_replaced() {
        let (_dir, store) = store();
        std::fs::write(store.auth_path(), "{not json").expect("write");
        let err = store.load_auth().expect_err("must fail");
        assert!(matches!(err, AuthError::Malformed { .. }));
        // The original bytes must still be there for an operator to inspect.
        assert_eq!(
            std::fs::read_to_string(store.auth_path()).expect("read"),
            "{not json"
        );
    }

    #[test]
    fn a_missing_auth_file_reads_as_not_configured() {
        let (_dir, store) = store();
        assert!(!store.is_configured().expect("configured"));
        assert!(store.load_auth().expect("load").users.is_empty());
    }

    #[test]
    fn duplicate_creation_is_refused() {
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("first");
        assert!(store.create_user("ALICE", "hunter2hunter2", false).is_err());
    }

    #[test]
    fn tokens_are_unique_per_issue() {
        let (_dir, store) = store();
        store
            .create_user("alice", "hunter2hunter2", false)
            .expect("create");
        let mut seen = std::collections::HashSet::new();
        for _ in 0..16 {
            assert!(seen.insert(store.issue_token("alice").expect("issue")));
        }
    }
}
