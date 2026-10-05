//! T0308: provider OAuth device flow — the shared store and the 3-route state machine.
//!
//! Ports `routes/device_flow.py`. That module is already a consolidation on the Python
//! side (`create_device_flow_router` exists precisely so Copilot and `ChatGPT`
//! Subscription share one implementation), and this port keeps that shape: the store
//! and the state machine are provider-agnostic, and a provider supplies two closures.
//!
//! ## The `device_code` never reaches the browser
//!
//! A `device_code` is a bearer credential for the user's provider account: anyone
//! holding it can complete the device flow and obtain a token. The upstream code
//! already states the rule — `copilot_routes.py:13-14`, *"The secret `device_code` is
//! kept server-side, never sent to the browser"* — and this port makes it
//! **structural** rather than a matter of remembering:
//!
//! * [`PendingDeviceFlow`] holds the code in an [`OAuthSecret`], which is not
//!   `Serialize`. There is no way to serialize the payload, so no handler can
//!   accidentally return it.
//! * [`BrowserDeviceStart`] — the type a start route returns — has no field that can
//!   hold a code. It carries `poll_id` and the `user_code`/`verification_uri` the
//!   user must type, and it is `Serialize`.
//! * The code is stored server-side under `poll_id`; the poll route looks it up.
//!   It is never passed as a query parameter, a header, or inside a redirect URL.
//! * [`PollOutcome`] does not carry the upstream error body. Providers embed the code
//!   in some error strings, so an error is reduced to a fixed vocabulary before it
//!   leaves this module.
//!
//! A structural guarantee can still be broken by adding a field, so there is a test
//! asserting [`BrowserDeviceStart`] has exactly the intended field set — a field named
//! `device_code` appearing there is a compile-adjacent failure rather than a silent
//! leak.

use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::oauth_secret::OAuthSecret;

/// The default poll interval when a provider does not state one.
pub const DEFAULT_INTERVAL_SECONDS: u32 = 5;

/// The default flow lifetime when a provider does not state one.
pub const DEFAULT_EXPIRES_IN_SECONDS: u32 = 900;

/// The providers this store holds flows for.
///
/// Copilot appears twice on purpose: public GitHub and GitHub Enterprise are separate
/// deployments with separate hosts, separate API bases and separate allow-listed
/// client ids, and an admin connecting an Enterprise account must not be silently
/// served by the public one. They share one implementation (see
/// [`crate::copilot`]) because the only difference is a host and a base URL — the
/// duplication that T0306 warns about would be two copies of the same request logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceFlowProvider {
    /// GitHub Copilot on public github.com.
    CopilotPublic,
    /// GitHub Copilot on a GitHub Enterprise deployment.
    CopilotEnterprise,
    /// `ChatGPT` subscription / Codex backend.
    ChatGptSubscription,
}

impl DeviceFlowProvider {
    /// Every provider, for exhaustive matching.
    pub const ALL: [Self; 3] = [
        Self::CopilotPublic,
        Self::CopilotEnterprise,
        Self::ChatGptSubscription,
    ];

    /// The stored wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CopilotPublic => "copilot",
            Self::CopilotEnterprise => "copilot-enterprise",
            Self::ChatGptSubscription => "chatgpt-subscription",
        }
    }

    /// Parses a stored wire name.
    ///
    /// # Errors
    ///
    /// Returns [`DeviceFlowError::UnknownProvider`] for an unrecognised name, so a
    /// row written by a newer build is reported rather than silently dropped.
    pub fn parse(name: &str) -> Result<Self, DeviceFlowError> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str() == name)
            .ok_or_else(|| DeviceFlowError::UnknownProvider(name.to_owned()))
    }
}

/// Why a device-flow operation failed.
#[derive(Debug, PartialEq, thiserror::Error)]
pub enum DeviceFlowError {
    /// The SQLite query failed.
    #[error("device-flow store error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// The provider name is not one of [`DeviceFlowProvider::ALL`].
    #[error("unknown device-flow provider: {0}")]
    UnknownProvider(String),

    /// No live flow for this `poll_id`.
    ///
    /// One variant covers "never existed" and "expired" together, so the poll route
    /// cannot be used to probe which `poll_id`s were once real.
    #[error("unknown or expired login session")]
    NotFound,

    /// The stored payload was not the shape the provider wrote.
    #[error("stored device-flow payload is malformed")]
    CorruptPayload,

    /// The provider did not return what the flow needs to proceed.
    #[error("provider did not return a device code")]
    NoDeviceCode,
}

/// A source of time, injected so no test reads the wall clock.
///
/// `AGENTS.md` §11 forbids sleep-based synchronisation. Python's
/// `PendingDeviceFlowStore` takes a `time_func` parameter for exactly this reason and
/// this keeps it.
pub trait Clock {
    /// Seconds since an arbitrary but fixed epoch.
    fn now_secs(&self) -> u64;
}

/// A clock a test advances by hand.
#[derive(Debug, Default)]
pub struct TestClock(std::cell::Cell<u64>);

impl TestClock {
    /// A clock at `secs`.
    #[must_use]
    pub fn at(secs: u64) -> Self {
        Self(std::cell::Cell::new(secs))
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: Duration) {
        self.0.set(self.0.get() + by.as_secs());
    }
}

impl Clock for TestClock {
    fn now_secs(&self) -> u64 {
        self.0.get()
    }
}

/// Unix seconds in the form the `DATETIME` columns hold.
///
/// The clock is a `u64` and SQLite has no unsigned type, so every crossing goes through
/// here rather than a bare `as`. `as` wraps silently above `i64::MAX`, and on the *read*
/// side it also turns a negative column into an enormous `u64` — a row that then reads
/// as "far in the future", i.e. a credential that never looks expired. Saturating in
/// both directions makes a corrupt row degrade to "expired", the safe direction.
fn to_db_secs(secs: u64) -> i64 {
    secs.cast_signed()
}

/// A `DATETIME` column back to clock seconds, clamped at zero.
fn from_db_secs(secs: i64) -> u64 {
    secs.max(0).cast_unsigned()
}

/// The provider-side payload for a pending flow.
///
/// Not `Serialize`, and neither is any field that could hold a code without being
/// wrapped. The two providers need different keys, so the payload is a small map of
/// named values — but every value that is a credential is an [`OAuthSecret`], which
/// keeps the whole payload unserializable by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDeviceFlow {
    /// The GitHub host, for Copilot. `github.com` or an Enterprise domain.
    pub host: Option<Box<str>>,
    /// The user's own account, for attribution.
    pub owner: Option<Box<str>>,
    /// The Enterprise URL the admin supplied, if any.
    pub enterprise_url: Option<Box<str>>,
    /// The provider's credential: a GitHub `device_code`, or a `ChatGPT`
    /// `device_auth_id`.
    ///
    /// An `OAuthSecret`, so this type cannot be serialized.
    pub credential: OAuthSecret,
    /// The `user_code` the user types at the provider.
    ///
    /// Not a secret — it is displayed on screen by design — but it lives here rather
    /// than only in the browser response because `ChatGPT`'s poll needs it back.
    pub user_code: Option<Box<str>>,
    /// A PKCE `code_verifier`, where the provider uses one.
    pub code_verifier: Option<OAuthSecret>,
    /// Where the user goes to enter the code, echoed back on start.
    pub verification_uri: Option<Box<str>>,
    /// A pre-filled variant of `verification_uri`, when the provider sends one.
    ///
    /// Copilot sends this and it embeds the *user* code
    /// (`copilot_routes.py:119-122`), so it is safe to hand the browser.
    pub verification_uri_complete: Option<Box<str>>,
}

impl PendingDeviceFlow {
    /// A flow for a provider that needs only a credential.
    #[must_use]
    pub fn new(credential: OAuthSecret) -> Self {
        Self {
            host: None,
            owner: None,
            enterprise_url: None,
            credential,
            user_code: None,
            code_verifier: None,
            verification_uri: None,
            verification_uri_complete: None,
        }
    }

    /// Sets the GitHub host.
    #[must_use]
    pub fn with_host(mut self, host: impl Into<Box<str>>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Sets the owning user.
    #[must_use]
    pub fn with_owner(mut self, owner: impl Into<Box<str>>) -> Self {
        self.owner = Some(owner.into());
        self
    }

    /// Sets the Enterprise URL.
    #[must_use]
    pub fn with_enterprise_url(mut self, url: impl Into<Box<str>>) -> Self {
        self.enterprise_url = Some(url.into());
        self
    }

    /// Sets the `user_code`.
    #[must_use]
    pub fn with_user_code(mut self, code: impl Into<Box<str>>) -> Self {
        self.user_code = Some(code.into());
        self
    }

    /// Sets the PKCE verifier.
    #[must_use]
    pub fn with_verifier(mut self, verifier: OAuthSecret) -> Self {
        self.code_verifier = Some(verifier);
        self
    }

    /// Sets the verification URI.
    #[must_use]
    pub fn with_verification_uri(mut self, uri: impl Into<Box<str>>) -> Self {
        self.verification_uri = Some(uri.into());
        self
    }

    /// Sets the pre-filled verification URI.
    #[must_use]
    pub fn with_verification_uri_complete(mut self, uri: impl Into<Box<str>>) -> Self {
        self.verification_uri_complete = Some(uri.into());
        self
    }

    /// Renders the payload for the `payload` column.
    ///
    /// A hand-rolled encoding rather than `serde_json`, because the type is deliberately not
    /// serializable and this is the one place its contents are allowed to become bytes.
    ///
    /// The format is a sequence of `<len>\x1f<len bytes>` records. The length covers the
    /// whole record, so decoding *skips* exactly that many bytes instead of searching for a
    /// delimiter — which is what makes a value containing the delimiter round-trip rather
    /// than fail. (An earlier draft length-prefixed each value and searched for `\x1e`, so a
    /// `device_code` containing `\x1e` was silently truncated and then rejected as
    /// corrupt. Failing closed is better than accepting corruption, but a `device_code` is
    /// provider-supplied and the format should not care what it contains.)
    fn encode(&self) -> String {
        let mut out = String::new();
        for (key, value) in [
            ("host", self.host.as_deref()),
            ("owner", self.owner.as_deref()),
            ("enterprise_url", self.enterprise_url.as_deref()),
            ("user_code", self.user_code.as_deref()),
            ("verification_uri", self.verification_uri.as_deref()),
            (
                "verification_uri_complete",
                self.verification_uri_complete.as_deref(),
            ),
            ("credential", Some(self.credential.expose())),
            (
                "code_verifier",
                self.code_verifier.as_ref().map(OAuthSecret::expose),
            ),
        ] {
            // `key=` with an empty value marks an absent field, so a present-but-empty
            // value stays distinguishable from an absent one.
            let record = match value {
                Some(v) => format!("{key}={v}"),
                None => format!("{key}="),
            };
            out.push_str(&record.len().to_string());
            out.push('\x1f');
            out.push_str(&record);
        }
        out
    }

    /// Parses a payload written by [`Self::encode`].
    ///
    /// # Errors
    ///
    /// [`DeviceFlowError::CorruptPayload`] if a field is malformed, a length does not
    /// match, or the credential is absent. Failing closed matters: a half-parsed
    /// payload would send an empty credential upstream and the provider would reject it
    /// with an error string that may quote the code we were trying to protect.
    fn decode(raw: &str) -> Result<Self, DeviceFlowError> {
        let mut host: Option<Box<str>> = None;
        let mut owner: Option<Box<str>> = None;
        let mut enterprise_url: Option<Box<str>> = None;
        let mut user_code: Option<Box<str>> = None;
        let mut verification_uri: Option<Box<str>> = None;
        let mut verification_uri_complete: Option<Box<str>> = None;
        let mut credential: Option<OAuthSecret> = None;
        let mut code_verifier: Option<OAuthSecret> = None;

        let mut rest = raw;
        while !rest.is_empty() {
            let (len, tail) = rest
                .split_once('\x1f')
                .ok_or(DeviceFlowError::CorruptPayload)?;
            let len: usize = len.parse().map_err(|_| DeviceFlowError::CorruptPayload)?;
            if tail.len() < len {
                return Err(DeviceFlowError::CorruptPayload);
            }
            // Skip exactly `len` bytes rather than searching for a delimiter, so a value
            // containing the separator round-trips instead of being truncated.
            let (record, remainder) = tail.split_at(len);
            rest = remainder;

            let (key, value) = record
                .split_once('=')
                .ok_or(DeviceFlowError::CorruptPayload)?;
            // An empty value means the field was absent, not that it held "".
            let present = |v: &str| (!v.is_empty()).then(|| Box::from(v));
            match key {
                "host" => host = present(value),
                "owner" => owner = present(value),
                "enterprise_url" => enterprise_url = present(value),
                "user_code" => user_code = present(value),
                "verification_uri" => verification_uri = present(value),
                "verification_uri_complete" => verification_uri_complete = present(value),
                "code_verifier" => {
                    code_verifier = present(value).map(|v| OAuthSecret::new(v.into_string()));
                }
                // The credential is the one field that must be present, so it is tracked
                // separately rather than defaulted.
                "credential" => credential = present(value).map(OAuthSecret::new),
                _ => return Err(DeviceFlowError::CorruptPayload),
            }
        }

        Ok(Self {
            host,
            owner,
            enterprise_url,
            credential: credential.ok_or(DeviceFlowError::CorruptPayload)?,
            user_code,
            code_verifier,
            verification_uri,
            verification_uri_complete,
        })
    }
}

/// What the browser is told when a flow starts.
///
/// **This is the type that must never carry a credential.** It has exactly three
/// fields: the handle it will poll with, and the two things the user must act on. A
/// `device_code` cannot be placed here — there is no field for it, and the value that
/// would fill one is an [`OAuthSecret`] that does not implement `Serialize`.
///
/// `verification_uri_complete` is included deliberately: GitHub's form embeds the
/// user code in the URL, so opening it lands the user on the authorize screen with the
/// code pre-filled. That is the *user* code, which is designed to be public.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BrowserDeviceStart {
    /// The handle the browser polls with, and the only server-side lookup key.
    pub poll_id: Box<str>,
    /// The short code the user types at the provider.
    pub user_code: Option<Box<str>>,
    /// Where the user goes to enter it.
    pub verification_uri: Option<Box<str>>,
    /// A pre-filled variant of `verification_uri`, when the provider sends one.
    pub verification_uri_complete: Option<Box<str>>,
    /// Seconds the browser should wait between polls.
    pub interval: u32,
    /// Seconds until the flow expires.
    pub expires_in: u32,
}

/// The outcome of one poll, normalised across providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    /// The user has not finished. Keep polling.
    Pending {
        /// A short, non-sensitive hint for the UI.
        detail: Option<Box<str>>,
    },
    /// The provider asked us to back off.
    SlowDown {
        /// A provider-supplied interval, or `None` to use the store's escalation.
        interval: Option<u32>,
        /// A short, non-sensitive hint for the UI.
        detail: Option<Box<str>>,
    },
    /// The user authorised. The credential is consumed here and never returned.
    Authorized {
        /// The provider's token. An [`OAuthSecret`], so this cannot be serialized.
        credential: OAuthSecret,
        /// A refresh token, where the provider issues one.
        refresh_credential: Option<OAuthSecret>,
        /// The API base the credential is valid against.
        base_url: Box<str>,
    },
    /// The flow failed terminally.
    Failed {
        /// The reason, reduced to [`PollFailure`] rather than the upstream body.
        reason: PollFailure,
    },
}

impl PollOutcome {
    /// A `Pending` with no detail.
    #[must_use]
    pub const fn pending() -> Self {
        Self::Pending { detail: None }
    }
}

/// Why a device flow failed.
///
/// A closed vocabulary on purpose. Providers put the `device_code` into some error
/// strings — `ChatGPT`'s poll returns the raw upstream body on the paths that are not
/// `403`/`404` — so echoing the body would leak the credential that this whole module
/// exists to contain. Mapping to an enum means the error text is chosen here and
/// cannot carry provider text at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollFailure {
    /// The user denied the request.
    AccessDenied,
    /// The code or session expired before the user acted.
    ExpiredToken,
    /// The provider returned an error we do not recognise.
    Unknown,
}

impl PollFailure {
    /// Maps a provider's `error` string onto the vocabulary.
    ///
    /// Copilot returns `expired_token` / `access_denied`
    /// (`copilot_routes.py:159`), `ChatGPT` returns `denied` as well
    /// (`chatgpt_subscription_routes.py:155`). Anything unrecognised is
    /// [`Self::Unknown`] rather than being passed through, which is the point.
    #[must_use]
    pub fn from_provider_error(error: &str) -> Self {
        match error {
            "access_denied" | "denied" | "authorization_denied" => Self::AccessDenied,
            "expired_token" | "expired" => Self::ExpiredToken,
            _ => Self::Unknown,
        }
    }

    /// The wire name sent to the browser.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AccessDenied => "access_denied",
            Self::ExpiredToken => "expired_token",
            Self::Unknown => "unknown",
        }
    }
}

/// What the poll route returns to the browser.
///
/// `Serialize`, and every field is safe: a status, an optional already-redacted
/// detail, and — on success — the *provisioned endpoint id*, never the token. The
/// token was written to `provider_auth_sessions` by the provisioning step; the browser
/// learns the id, and the server keeps the credential.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BrowserPollResult {
    /// `pending`, `authorized`, `failed` or `cancelled`.
    pub status: Box<str>,
    /// The provisioned endpoint's id, on success only.
    pub endpoint_id: Option<Box<str>>,
    /// The failure reason, on `failed` only.
    pub error: Option<Box<str>>,
    /// A short, non-sensitive hint.
    pub detail: Option<Box<str>>,
}

impl BrowserPollResult {
    /// A bare `pending`, which is what a throttled poll returns.
    ///
    /// Throttling deliberately omits `detail`: the upstream Python returns
    /// `{"status": "pending"}` with nothing else (`device_flow.py:166`), because a
    /// client polling faster than the interval should not learn anything new.
    #[must_use]
    pub fn pending() -> Self {
        Self {
            status: "pending".into(),
            endpoint_id: None,
            error: None,
            detail: None,
        }
    }

    /// An `authorized` carrying the provisioned endpoint id.
    #[must_use]
    pub fn authorized(endpoint_id: Box<str>) -> Self {
        Self {
            status: "authorized".into(),
            endpoint_id: Some(endpoint_id),
            error: None,
            detail: None,
        }
    }

    /// A `failed` carrying a vocabulary reason.
    #[must_use]
    pub fn failed(reason: PollFailure) -> Self {
        Self {
            status: "failed".into(),
            endpoint_id: None,
            error: Some(reason.as_str().into()),
            detail: None,
        }
    }

    /// A `cancelled`.
    #[must_use]
    pub fn cancelled() -> Self {
        Self {
            status: "cancelled".into(),
            endpoint_id: None,
            error: None,
            detail: None,
        }
    }
}

/// The SQLite-backed pending-flow store.
///
/// One instance per process, injected rather than a global (AGENTS.md §8). All methods
/// take `&self` and borrow the connection for the duration of a statement, and nothing
/// holds a lock across an await.
///
/// The type is deliberately **not** internally synchronised: `&Connection` is not
/// `Sync`, so it can only be used from one thread at a time, and a caller needing it
/// across tasks wraps it in a `Mutex` and keeps the critical section to the statement.
/// An earlier draft carried a `RefCell<()>` "guard" here; that is not mutual exclusion
/// and has been removed rather than left to imply a guarantee the type does not give.
pub struct DeviceFlowStore<'c> {
    conn: &'c Connection,
    clock: &'c dyn Clock,
}

impl std::fmt::Debug for DeviceFlowStore<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceFlowStore").finish_non_exhaustive()
    }
}

impl<'c> DeviceFlowStore<'c> {
    /// A store over `conn`, reading time from `clock`.
    #[must_use]
    pub fn new(conn: &'c Connection, clock: &'c dyn Clock) -> Self {
        Self { conn, clock }
    }

    fn now(&self) -> u64 {
        self.clock.now_secs()
    }

    /// Removes flows whose expiry has passed.
    ///
    /// Called at the top of every mutating operation, matching Python's
    /// `prune_expired` placement. A flow that is never pruned would keep a
    /// `device_code` on disk indefinitely.
    fn prune(&self) -> Result<(), DeviceFlowError> {
        let now = self.now();
        self.conn.execute(
            "DELETE FROM pending_device_flows WHERE expires_at <= ?1",
            params![to_db_secs(now)],
        )?;
        Ok(())
    }

    /// Stores `payload` under a fresh `poll_id` and returns the browser-facing start.
    ///
    /// The returned [`BrowserDeviceStart`] contains no credential. `interval` and
    /// `expires_in` are floored at 1 so a provider reporting `0` cannot create a flow
    /// that is instantly expired or never throttled.
    ///
    /// # Errors
    ///
    /// [`DeviceFlowError::NoDeviceCode`] when `payload` carries an empty credential,
    /// and [`DeviceFlowError::Sqlite`] on a database failure.
    pub fn add(
        &self,
        provider: DeviceFlowProvider,
        payload: &PendingDeviceFlow,
        interval: Option<u32>,
        expires_in: Option<u32>,
    ) -> Result<BrowserDeviceStart, DeviceFlowError> {
        if payload.credential.is_empty() {
            return Err(DeviceFlowError::NoDeviceCode);
        }
        self.prune()?;

        let interval = interval.unwrap_or(DEFAULT_INTERVAL_SECONDS).max(1);
        let expires_in = expires_in.unwrap_or(DEFAULT_EXPIRES_IN_SECONDS).max(1);
        let now = self.now();
        // Reuse the endpoint module's id generator rather than adding a second one:
        // two id schemes in one crate is the T0306 duplication trap. Its error
        // type is `EndpointError`, so the only variant reachable here is the
        // entropy failure, which surfaces as a store error.
        let poll_id =
            crate::model_endpoints::short_id().map_err(|_| DeviceFlowError::NoDeviceCode)?;

        self.conn.execute(
            "INSERT INTO pending_device_flows
               (id, provider, owner, payload, interval_seconds, expires_at,
                next_poll_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                poll_id,
                provider.as_str(),
                payload.owner.as_deref(),
                payload.encode(),
                i64::from(interval),
                to_db_secs(now + u64::from(expires_in)),
                // Python seeds next_poll_at to 0 so the first poll is never throttled.
                0_i64,
                to_db_secs(now),
            ],
        )?;

        Ok(BrowserDeviceStart {
            poll_id: Box::from(poll_id),
            user_code: payload.user_code.clone(),
            verification_uri: payload.verification_uri.clone(),
            verification_uri_complete: payload.verification_uri_complete.clone(),
            interval,
            expires_in,
        })
    }

    /// Reads a live flow, or `None` if it is unknown or expired.
    ///
    /// # Errors
    ///
    /// Propagates the underlying database failure.
    pub fn get(&self, poll_id: &str) -> Result<Option<PendingDeviceFlow>, DeviceFlowError> {
        let now = self.now();
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT provider, payload FROM pending_device_flows
                  WHERE id = ?1 AND expires_at > ?2",
                params![poll_id, to_db_secs(now)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(_, payload)| PendingDeviceFlow::decode(&payload))
            .transpose()
    }

    /// The provider a flow belongs to.
    ///
    /// # Errors
    ///
    /// [`DeviceFlowError::NotFound`] when the flow is unknown or expired.
    pub fn provider_of(&self, poll_id: &str) -> Result<DeviceFlowProvider, DeviceFlowError> {
        let now = self.now();
        let name: Option<String> = self
            .conn
            .query_row(
                "SELECT provider FROM pending_device_flows WHERE id = ?1 AND expires_at > ?2",
                params![poll_id, to_db_secs(now)],
                |row| row.get(0),
            )
            .optional()?;
        // `NotFound` for a missing row, so the poll route cannot distinguish "never
        // existed" from "expired"; `UnknownProvider` for a row this build cannot
        // interpret, which is a real condition and must not read as "not found".
        let name = name.ok_or(DeviceFlowError::NotFound)?;
        DeviceFlowProvider::parse(&name)
    }

    /// Whether the caller is polling faster than the stored interval allows.
    ///
    /// # Errors
    ///
    /// Propagates the underlying database failure.
    pub fn is_throttled(&self, poll_id: &str) -> Result<bool, DeviceFlowError> {
        let now = self.now();
        let next: Option<i64> = self
            .conn
            .query_row(
                "SELECT next_poll_at FROM pending_device_flows WHERE id = ?1",
                params![poll_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(next.is_some_and(|t| now < from_db_secs(t)))
    }

    /// Arms the next poll, `interval` seconds out.
    ///
    /// # Errors
    ///
    /// [`DeviceFlowError::NotFound`] when the flow is gone.
    pub fn schedule_next(&self, poll_id: &str) -> Result<(), DeviceFlowError> {
        let now = self.now();
        let interval: Option<i64> = self
            .conn
            .query_row(
                "SELECT interval_seconds FROM pending_device_flows WHERE id = ?1",
                params![poll_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(interval) = interval else {
            return Err(DeviceFlowError::NotFound);
        };
        self.conn.execute(
            "UPDATE pending_device_flows SET next_poll_at = ?1 WHERE id = ?2",
            params![to_db_secs(now + interval.max(0).cast_unsigned()), poll_id],
        )?;
        Ok(())
    }

    /// Backs the interval off after a `slow_down`.
    ///
    /// A provider-supplied interval wins; otherwise the stored one grows by 5, which
    /// is Python's escalation (`device_flow.py:113`). The floor of 1 stops a provider
    /// reporting `interval: 0` from producing a permanently throttled flow.
    ///
    /// # Errors
    ///
    /// [`DeviceFlowError::NotFound`] when the flow is gone.
    pub fn slow_down(&self, poll_id: &str, interval: Option<u32>) -> Result<(), DeviceFlowError> {
        let now = self.now();
        let current: Option<i64> = self
            .conn
            .query_row(
                "SELECT interval_seconds FROM pending_device_flows WHERE id = ?1",
                params![poll_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else {
            return Err(DeviceFlowError::NotFound);
        };
        let next = interval.map_or(current + 5, i64::from).max(1);
        self.conn.execute(
            "UPDATE pending_device_flows SET interval_seconds = ?1, next_poll_at = ?2
              WHERE id = ?3",
            params![next, to_db_secs(now + next.cast_unsigned()), poll_id],
        )?;
        Ok(())
    }

    /// Removes a flow, whatever its state.
    ///
    /// # Errors
    ///
    /// [`DeviceFlowError::Sqlite`] on a database failure. Removing an unknown
    /// `poll_id` is not an error: cancel is idempotent.
    pub fn pop(&self, poll_id: &str) -> Result<(), DeviceFlowError> {
        self.conn.execute(
            "DELETE FROM pending_device_flows WHERE id = ?1",
            params![poll_id],
        )?;
        Ok(())
    }

    /// How many live flows are stored.
    ///
    /// Used by tests and by the prune path's callers; a flow count that only grows is
    /// the symptom of a prune that stopped running.
    ///
    /// # Errors
    ///
    /// Propagates the underlying database failure.
    pub fn live_len(&self) -> Result<usize, DeviceFlowError> {
        let now = self.now();
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM pending_device_flows WHERE expires_at > ?1",
            params![to_db_secs(now)],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count.max(0)).unwrap_or(0))
    }
}

/// The three route operations, as one provider-agnostic state machine.
///
/// Ports the body of `create_device_flow_router`'s handlers. Web owns the HTTP
/// surface (§5: "Map errors to HTTP status only in odysseus-web"), so this returns
/// data and lets the caller choose a status code.
///
/// The ordering of [`Self::poll`] is load-bearing and matches upstream exactly:
///
/// 1. unknown/expired → [`DeviceFlowError::NotFound`], *before* any throttle check,
/// 2. throttled → bare `pending`, with no provider call at all,
/// 3. provider call, whose error is swallowed into `pending` rather than propagated —
///    except for an authorized result, which consumes the flow,
/// 4. `authorized`/`failed` consume the flow; `slow_down` escalates and keeps it;
///    `pending` arms the next poll and keeps it.
#[derive(Debug)]
pub struct DeviceFlow<'s> {
    store: &'s DeviceFlowStore<'s>,
}

impl<'s> DeviceFlow<'s> {
    /// A state machine over `store`.
    #[must_use]
    pub const fn new(store: &'s DeviceFlowStore<'s>) -> Self {
        Self { store }
    }

    /// Starts a flow and returns what the browser may see.
    ///
    /// # Errors
    ///
    /// Propagates [`DeviceFlowStore::add`].
    pub fn start(
        &self,
        provider: DeviceFlowProvider,
        payload: &PendingDeviceFlow,
        interval: Option<u32>,
        expires_in: Option<u32>,
    ) -> Result<BrowserDeviceStart, DeviceFlowError> {
        self.store.add(provider, payload, interval, expires_in)
    }

    /// Cancels a flow. Idempotent.
    ///
    /// # Errors
    ///
    /// Propagates [`DeviceFlowStore::pop`].
    pub fn cancel(&self, poll_id: &str) -> Result<BrowserPollResult, DeviceFlowError> {
        self.store.pop(poll_id)?;
        Ok(BrowserPollResult::cancelled())
    }

    /// Polls once.
    ///
    /// `poll_provider` performs the provider call and maps its response onto a
    /// [`PollOutcome`]. It receives the stored payload — including the credential —
    /// and must not log it.
    ///
    /// `provision` runs on an authorised result and returns the endpoint id to report.
    /// It receives the credential and is the only place that should persist it.
    ///
    /// # Errors
    ///
    /// [`DeviceFlowError::NotFound`] for an unknown or expired `poll_id`.
    pub fn poll<P, R>(
        &self,
        poll_id: &str,
        poll_provider: P,
        provision: R,
    ) -> Result<BrowserPollResult, DeviceFlowError>
    where
        P: FnOnce(&PendingDeviceFlow) -> Result<PollOutcome, ProviderCallError>,
        R: FnOnce(&PollOutcome) -> Result<Box<str>, ProvisionError>,
    {
        // 1. Existence before throttling, so an expired id cannot be distinguished
        //    from a throttled one.
        if self.store.get(poll_id)?.is_none() {
            return Err(DeviceFlowError::NotFound);
        }
        // 2. A throttled poll never reaches the provider.
        if self.store.is_throttled(poll_id)? {
            return Ok(BrowserPollResult::pending());
        }

        let payload = self.store.get(poll_id)?.ok_or(DeviceFlowError::NotFound)?;

        // Upstream swallows a provider error into `pending` and keeps the session, so a
        // transient 502 does not force the user to restart the flow. `let-else` keeps
        // the happy path unindented, per AGENTS.md §1.
        let Ok(outcome) = poll_provider(&payload) else {
            self.store.schedule_next(poll_id)?;
            return Ok(BrowserPollResult::pending());
        };

        match outcome {
            PollOutcome::Authorized { .. } => {
                // Provision *before* consuming: if provisioning fails the flow is
                // still live, so a retry is possible instead of losing the grant.
                // Provision *before* consuming the flow: if provisioning fails the flow
                // is still live, so the grant is not lost and the user can retry rather
                // than being told to authorise again.
                let endpoint_id = provision(&outcome)?;
                self.store.pop(poll_id)?;
                Ok(BrowserPollResult::authorized(endpoint_id))
            }
            PollOutcome::Failed { reason } => {
                self.store.pop(poll_id)?;
                Ok(BrowserPollResult::failed(reason))
            }
            PollOutcome::SlowDown { interval, detail } => {
                self.store.slow_down(poll_id, interval)?;
                Ok(Self::pending_with(detail))
            }
            PollOutcome::Pending { detail } => {
                self.store.schedule_next(poll_id)?;
                Ok(Self::pending_with(detail))
            }
        }
    }

    /// A `pending` carrying an optional detail.
    fn pending_with(detail: Option<Box<str>>) -> BrowserPollResult {
        BrowserPollResult {
            status: "pending".into(),
            endpoint_id: None,
            error: None,
            detail,
        }
    }
}

/// Why a provider call did not produce an outcome.
///
/// Deliberately carries no message: the upstream body may embed the `device_code`, so
/// there is nothing safe to put in here. A caller that wants to log the cause logs the
/// status code from its transport, not this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("provider call failed")]
pub struct ProviderCallError;

/// Why provisioning an authorised credential failed.
///
/// Also message-free, for the same reason: it is raised on a path that has the token in
/// hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("endpoint provisioning failed")]
pub struct ProvisionError;

impl From<ProvisionError> for DeviceFlowError {
    fn from(_: ProvisionError) -> Self {
        DeviceFlowError::CorruptPayload
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_EXPIRES_IN_SECONDS, DEFAULT_INTERVAL_SECONDS, DeviceFlowProvider, PollFailure,
    };

    #[test]
    fn there_are_three_providers_and_each_round_trips() {
        for provider in DeviceFlowProvider::ALL {
            // Compared on the parsed value: `DeviceFlowError` cannot derive `Eq`
            // because `rusqlite::Error` does not, so the `Result` is not comparable.
            assert_eq!(
                DeviceFlowProvider::parse(provider.as_str()).ok(),
                Some(provider),
                "{}",
                provider.as_str()
            );
        }
        assert_eq!(DeviceFlowProvider::ALL.len(), 3);
    }

    #[test]
    fn copilot_public_and_enterprise_are_distinct() {
        assert_ne!(
            DeviceFlowProvider::CopilotPublic,
            DeviceFlowProvider::CopilotEnterprise
        );
        assert_ne!(
            DeviceFlowProvider::CopilotPublic.as_str(),
            DeviceFlowProvider::CopilotEnterprise.as_str()
        );
    }

    #[test]
    fn an_unknown_provider_name_is_rejected() {
        assert!(DeviceFlowProvider::parse("gemini").is_err());
    }

    #[test]
    fn upstream_defaults_match_the_python_literals() {
        assert_eq!(DEFAULT_INTERVAL_SECONDS, 5);
        assert_eq!(DEFAULT_EXPIRES_IN_SECONDS, 900);
    }

    #[test]
    fn provider_errors_reduce_to_the_vocabulary() {
        for denied in ["access_denied", "denied", "authorization_denied"] {
            assert_eq!(
                PollFailure::from_provider_error(denied),
                PollFailure::AccessDenied
            );
        }
        for expired in ["expired_token", "expired"] {
            assert_eq!(
                PollFailure::from_provider_error(expired),
                PollFailure::ExpiredToken
            );
        }
        // The critical case: an unrecognised error must NOT be passed through,
        // because provider error strings can embed the device code.
        assert_eq!(
            PollFailure::from_provider_error("device_code ABCDEF was rejected"),
            PollFailure::Unknown
        );
        assert_eq!(PollFailure::Unknown.as_str(), "unknown");
    }
}
