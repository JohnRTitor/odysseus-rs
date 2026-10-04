//! Request identity resolution.
//!
//! Ports `src/auth_helpers.py`. The distinction this module exists to protect:
//! a bearer `ody_` token arrives as the sandboxed pseudo-user `"api"`, but the
//! token was minted by a real person, and routes that attribute data must
//! resolve back to that person. [`AuthContext::effective_user`] does that.
//! [`AuthContext::is_delegated`] exists because resolving to the owner is
//! *correct for data and wrong for authority* — only admins may mint tokens, so
//! a token resolves to an admin, and an authority gate that asks "is the owner an
//! admin?" would hand the shell to any credential the owner gave away.
//!
//! Every gate here **fails closed**: an ambiguous identity is denied rather than
//! defaulted.

use odysseus_store::{api_tokens::Scope, auth::AuthStore};

use crate::error::AppError;

/// The pseudo-user a bearer API token authenticates as.
///
/// Present so a token cannot wander into a cookie/user route by default. It is
/// **never** the value used for ownership — see [`AuthContext::effective_user`].
pub const API_PSEUDO_USER: &str = "api";

/// The user stamped on an in-process internal-tool loopback call.
pub const INTERNAL_TOOL_USER: &str = "internal-tool";

/// Which of the documented bypasses are active.
///
/// Split out so [`AuthContext::require_user`] stays a pure function of its
/// inputs and can be tested without environment mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BypassConfig {
    /// `AUTH_ENABLED=false` — the operator explicitly turned auth off.
    pub auth_disabled: bool,
    /// `LOCALHOST_BYPASS=true` — the documented dev-only skip.
    pub localhost_bypass: bool,
    /// Whether `auth.json` has at least one user.
    pub is_configured: bool,
}

impl BypassConfig {
    /// Reads both switches from the environment, mirroring `app.py`.
    #[must_use]
    pub fn from_env(is_configured: bool) -> Self {
        let flag = |name: &str| std::env::var(name).is_ok_and(|v| v.eq_ignore_ascii_case("true"));
        Self {
            auth_disabled: !flag("AUTH_ENABLED"),
            localhost_bypass: flag("LOCALHOST_BYPASS"),
            is_configured,
        }
    }
}

/// Who the request is, as decided by the auth gate.
///
/// Inserted into request extensions by [`crate::middleware::auth`]. Layout
/// follows `AGENTS.md` §12.1: the scope list is a `Box<[Scope]>` because it is
/// immutable once the request is in flight, and the three `Option<String>`
/// fields are each genuinely optional — "same as parent" is `None`, never an
/// empty string.
#[derive(Debug, Clone, Default)]
pub struct AuthContext {
    current_user: Option<String>,
    api_token: bool,
    api_token_id: Option<String>,
    api_token_owner: Option<String>,
    api_token_scopes: Box<[Scope]>,
}

impl AuthContext {
    /// A browser session authenticated against `auth.json`.
    #[must_use]
    pub fn cookie_session(user: impl Into<String>) -> Self {
        Self {
            current_user: Some(user.into()),
            ..Self::default()
        }
    }

    /// A bearer API token. `owner` is the account the token belongs to.
    #[must_use]
    pub fn bearer(
        token_id: impl Into<String>,
        owner: impl Into<String>,
        scopes: Box<[Scope]>,
    ) -> Self {
        Self {
            current_user: Some(String::from(API_PSEUDO_USER)),
            api_token: true,
            api_token_id: Some(token_id.into()),
            api_token_owner: Some(owner.into()),
            api_token_scopes: scopes,
        }
    }

    /// An in-process internal-tool call, optionally impersonating `owner`.
    ///
    /// Mirrors `app.py:381-390`: impersonation applies only when the named
    /// account exists, and authorization is checked separately — this is owner
    /// *attribution* for notes and calendar, not authority.
    #[must_use]
    pub fn internal_tool(impersonated: Option<String>) -> Self {
        Self {
            current_user: Some(impersonated.unwrap_or_else(|| String::from(INTERNAL_TOOL_USER))),
            ..Self::default()
        }
    }

    /// The raw authenticated identity, i.e. Python's `get_current_user`.
    ///
    /// For a bearer token this is the `"api"` pseudo-user. Use it only where
    /// sandboxing is the intent; use [`Self::effective_user`] for ownership.
    #[must_use]
    pub fn current_user(&self) -> Option<&str> {
        self.current_user.as_deref()
    }

    /// True when a bearer API token authenticated this request.
    #[must_use]
    pub fn is_api_token(&self) -> bool {
        self.api_token
    }

    /// The matched token's row id, when [`Self::is_api_token`].
    #[must_use]
    pub fn api_token_id(&self) -> Option<&str> {
        self.api_token_id.as_deref()
    }

    /// The token's granted scopes.
    #[must_use]
    pub fn api_token_scopes(&self) -> &[Scope] {
        &self.api_token_scopes
    }

    /// The real human behind the request, for ownership and attribution.
    ///
    /// A cookie session resolves to the logged-in username. A bearer caller
    /// resolves back to the token's owner, so a paired client sees the *same*
    /// data as the owner's desktop UI rather than a separate `"api"`-owned silo.
    ///
    /// A token with no owner falls back to the raw identity, so it degrades to
    /// the sandboxed pseudo-user and never escalates.
    #[must_use]
    pub fn effective_user(&self) -> Option<&str> {
        if self.api_token && self.api_token_owner.is_some() {
            return self.api_token_owner.as_deref();
        }
        self.current_user.as_deref()
    }

    /// Whether this request arrived on a credential acting *for* a human.
    ///
    /// Security decisions about what the agent may do must ask this rather than
    /// "is the owner an admin?", so a token cannot inherit the shell merely
    /// because its owner could use one (`AGENTS.md` §7).
    #[must_use]
    pub fn is_delegated(&self) -> bool {
        self.api_token
    }

    /// Authenticate the caller, returning the username.
    ///
    /// Rejects a bearer token outright: owner-scoped routes must go through
    /// [`Self::require_api_token_scope`] so that resolving a token back to its
    /// owner never also grants the owner's interactive-session authority.
    ///
    /// The empty-string cases are the three documented in `auth_helpers.py`:
    /// auth disabled, `LOCALHOST_BYPASS` on loopback, and unconfigured
    /// first-run on loopback.
    ///
    /// # Errors
    ///
    /// [`AppError::Forbidden`] for a bearer token, [`AppError::Unauthenticated`]
    /// when no documented bypass applies.
    pub fn require_user(&self, bypass: BypassConfig, is_loopback: bool) -> Result<&str, AppError> {
        if self.is_api_token() {
            return Err(AppError::Forbidden(String::from(
                "API tokens must use a scope-aware API route",
            )));
        }
        if let Some(user) = self.current_user() {
            return Ok(user);
        }
        if bypass.auth_disabled {
            return Ok("");
        }
        if is_loopback && bypass.localhost_bypass {
            return Ok("");
        }
        if bypass.is_configured {
            return Err(AppError::Unauthenticated);
        }
        if is_loopback {
            return Ok("");
        }
        Err(AppError::Unauthenticated)
    }

    /// Require `scope` when the request is authenticated by an API token.
    ///
    /// Browser sessions pass through untouched.
    ///
    /// # Errors
    ///
    /// [`AppError::Forbidden`] when the token lacks `scope`, or when it has no
    /// owner to attribute the action to.
    pub fn require_api_token_scope(&self, scope: &Scope) -> Result<&str, AppError> {
        if !self.is_api_token() {
            return Ok(self.current_user().unwrap_or(""));
        }
        if !self.api_token_scopes.contains(scope) {
            return Err(AppError::Forbidden(format!(
                "API token missing required scope: {scope}"
            )));
        }
        self.api_token_owner
            .as_deref()
            .ok_or_else(|| AppError::Forbidden(String::from("API token has no owner")))
    }

    /// Require `privilege` for the resolved user.
    ///
    /// Two cases that Python conflates in one `try` block, kept apart here
    /// because they mean opposite things:
    ///
    /// * **A missing privilege key permits.** `auth_helpers.py:184` reads
    ///   `privs.get(key, True)` — "this user was never granted this flag". The UI
    ///   gates display-side, so a flag nobody has heard of is not a denial. This
    ///   part is faithful parity and stays.
    /// * **A failed lookup denies.** This is a **deliberate deviation** from
    ///   `auth_helpers.py:178-181`, which catches `Exception` and returns the
    ///   user. Python's failure means "we could not determine what this user was
    ///   granted", not "they were granted it", and `AGENTS.md` §5 requires that
    ///   ambiguous input be denied.
    ///
    /// The reachable path is not hypothetical: `AuthStore::privileges` calls
    /// `self.user(username)?`, which returns `Err(NoSuchUser)` for an account
    /// deleted between the session resolving and this lookup. Python hands that
    /// request the privilege it was just revoked, and `require_admin` two
    /// functions below denies the identical failure — so the asymmetry was a
    /// genuine escalation path, not a style mismatch.
    ///
    /// An explicit `false` denies. Admins hold every privilege via
    /// [`AuthStore::privileges`], so this is a no-op for them.
    ///
    /// # Errors
    ///
    /// [`AppError::Forbidden`] when the flag is explicitly false, when the lookup
    /// fails, or on an unauthenticated caller.
    pub fn require_privilege(
        &self,
        auth: &AuthStore,
        key: &str,
        bypass: BypassConfig,
        is_loopback: bool,
    ) -> Result<&str, AppError> {
        let user = self.require_user(bypass, is_loopback)?;
        if user.is_empty() {
            return Ok(user);
        }
        let denied = || {
            AppError::Forbidden(format!(
                "Your account is not allowed to {}.",
                key.replace('_', " ")
            ))
        };
        let privileges = auth.privileges(user).map_err(|e| {
            tracing::warn!(user, error = %e, "privilege lookup failed; denying");
            denied()
        })?;
        if !privileges.get(key) {
            return Err(denied());
        }
        Ok(user)
    }

    /// Require administrator authority.
    ///
    /// A delegated credential is refused **before** the owner is consulted, so
    /// handing a token to a third party never hands them the shell
    /// (`AGENTS.md` §7).
    ///
    /// # Errors
    ///
    /// [`AppError::Forbidden`] for a non-admin, including any bearer token.
    pub fn require_admin(
        &self,
        auth: &AuthStore,
        bypass: BypassConfig,
        is_loopback: bool,
    ) -> Result<&str, AppError> {
        if self.is_delegated() {
            return Err(AppError::Forbidden(String::from(
                "API tokens may not act with administrator authority",
            )));
        }
        let user = self.require_user(bypass, is_loopback)?;
        if user.is_empty() {
            return Ok(user);
        }
        let Ok(is_admin) = auth.is_admin(user) else {
            tracing::warn!(user, "admin lookup failed; denying");
            return Err(AppError::Forbidden(String::from("Not an administrator")));
        };
        if is_admin {
            Ok(user)
        } else {
            Err(AppError::Forbidden(String::from("Not an administrator")))
        }
    }
}

/// Owner-scoped row filter, mirroring `auth_helpers.py:owner_filter`.
///
/// `None` means "every row", which is what single-user mode needs; it is not the
/// same as `Some("")`.
#[must_use]
pub fn owner_matches(row_owner: Option<&str>, owner: &str) -> bool {
    owner.is_empty() || row_owner.is_none_or(|o| o == owner)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        odysseus_store::api_tokens::{Scope, TokenProfile},
    };

    fn scope(name: &str) -> Scope {
        Scope::parse(name).unwrap()
    }

    /// Bypasses all off, auth configured — the normal deployed state.
    const STRICT: BypassConfig = BypassConfig {
        auth_disabled: false,
        localhost_bypass: false,
        is_configured: true,
    };

    #[test]
    fn a_cookie_session_is_its_own_effective_user() {
        let ctx = AuthContext::cookie_session("alice");
        assert_eq!(ctx.current_user(), Some("alice"));
        assert_eq!(ctx.effective_user(), Some("alice"));
        assert!(!ctx.is_delegated());
    }

    #[test]
    fn a_bearer_token_resolves_to_its_owner_not_the_pseudo_user() {
        let ctx = AuthContext::bearer("tid", "alice", Box::new([]));
        assert_eq!(ctx.current_user(), Some(API_PSEUDO_USER));
        assert_eq!(ctx.effective_user(), Some("alice"));
        assert!(ctx.is_delegated());
    }

    #[test]
    fn a_token_without_an_owner_degrades_to_the_pseudo_user_and_never_escalates() {
        let mut ctx = AuthContext::bearer("tid", "alice", Box::new([]));
        ctx.api_token_owner = None;
        assert_eq!(ctx.effective_user(), Some(API_PSEUDO_USER));
        assert_eq!(ctx.api_token_owner, None);
    }

    #[test]
    fn internal_tool_impersonation_only_when_a_user_is_named() {
        assert_eq!(
            AuthContext::internal_tool(Some(String::from("alice"))).current_user(),
            Some("alice")
        );
        assert_eq!(
            AuthContext::internal_tool(None).current_user(),
            Some(INTERNAL_TOOL_USER)
        );
    }

    #[test]
    fn require_user_refuses_a_bearer_token_before_resolving_its_owner() {
        let ctx = AuthContext::bearer("tid", "alice", Box::new([]));
        let err = ctx
            .require_user(STRICT, true)
            .expect_err("a token must not pass a user gate");
        assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    }

    #[test]
    fn require_user_denies_an_anonymous_caller_when_configured() {
        let ctx = AuthContext::default();
        assert!(matches!(
            ctx.require_user(STRICT, false),
            Err(AppError::Unauthenticated)
        ));
    }

    #[test]
    fn require_user_allows_the_three_documented_bypasses() {
        let ctx = AuthContext::default();
        let disabled = BypassConfig {
            auth_disabled: true,
            ..STRICT
        };
        assert_eq!(ctx.require_user(disabled, false).unwrap(), "");

        let bypass = BypassConfig {
            localhost_bypass: true,
            ..STRICT
        };
        assert_eq!(ctx.require_user(bypass, true).unwrap(), "");

        let unconfigured = BypassConfig {
            is_configured: false,
            ..STRICT
        };
        assert_eq!(ctx.require_user(unconfigured, true).unwrap(), "");
        // ...but an unconfigured install still denies a remote caller.
        assert!(matches!(
            ctx.require_user(unconfigured, false),
            Err(AppError::Unauthenticated)
        ));
    }

    #[test]
    fn a_scope_gate_admits_a_browser_session_untouched() {
        let ctx = AuthContext::cookie_session("alice");
        assert_eq!(
            ctx.require_api_token_scope(&scope("chat")).unwrap(),
            "alice"
        );
    }

    #[test]
    fn a_scope_gate_refuses_a_token_without_the_scope() {
        let ctx = AuthContext::bearer(
            "tid",
            "alice",
            Box::new([scope("calendar:read"), scope("chat")]),
        );
        let err = ctx
            .require_api_token_scope(&scope("memory:write"))
            .expect_err("a token without the scope must be refused");
        assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    }

    #[test]
    fn a_scope_gate_refuses_a_token_that_has_no_owner() {
        let mut ctx = AuthContext::bearer("tid", "alice", Box::new([scope("chat")]));
        ctx.api_token_owner = None;
        let err = ctx
            .require_api_token_scope(&scope("chat"))
            .expect_err("a token with no owner cannot be attributed");
        assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    }

    #[test]
    fn a_chat_profile_grants_chat_but_not_the_write_scopes() {
        let scopes: Box<[Scope]> = TokenProfile::Chat.scopes().into_boxed_slice();
        let ctx = AuthContext::bearer("tid", "alice", scopes);
        assert!(
            ctx.require_api_token_scope(&Scope::parse("chat").unwrap())
                .is_ok()
        );
        // A chat token must not reach a write surface. There is deliberately no
        // `admin` scope: authority over tokens comes from the *profile*, and
        // `require_admin` refuses every bearer credential outright.
        let err = ctx
            .require_api_token_scope(&Scope::parse("memory:write").unwrap())
            .expect_err("a chat token must not hold memory:write");
        assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    }

    #[test]
    fn there_is_no_admin_scope_to_grant() {
        // Pinning the absence: if a future `admin` scope appeared, the profile
        // tests above would silently stop proving anything.
        assert!(Scope::parse("admin").is_err());
        assert!(Scope::parse("shell").is_err());
        assert_eq!(Scope::ALL.len(), 14);
    }

    #[test]
    fn a_delegated_credential_is_refused_by_the_admin_gate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep();
        let auth = AuthStore::new(&path);
        auth.prepare().expect("prepare");
        auth.create_user("alice", "correct horse", true)
            .expect("create admin");

        // A token minted by an admin, carrying every scope that exists.
        let all: Box<[Scope]> = Scope::ALL
            .iter()
            .filter_map(|s| Scope::parse(s).ok())
            .collect();
        let ctx = AuthContext::bearer("tid", "alice", all);
        assert!(ctx.is_delegated());

        // The owner IS an admin, and the gate still refuses, because the refusal
        // happens before the owner is consulted.
        let err = ctx
            .require_admin(&auth, STRICT, false)
            .expect_err("a delegated credential must never reach the admin gate");
        assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");

        // The same user through a cookie session passes.
        let session = AuthContext::cookie_session("alice");
        assert_eq!(
            session.require_admin(&auth, STRICT, false).expect("admin"),
            "alice"
        );
    }

    #[test]
    fn owner_filter_is_a_no_op_for_single_user_mode() {
        assert!(owner_matches(Some("alice"), ""));
        assert!(owner_matches(None, ""));
        assert!(owner_matches(Some("alice"), "alice"));
        assert!(!owner_matches(Some("bob"), "alice"));
        // Shared rows are visible to every owner.
        assert!(owner_matches(None, "alice"));
    }
}

#[cfg(test)]
mod privilege_tests {
    //! `require_privilege` had zero callers and zero tests, and its two fail-open
    //! branches were both unasserted — so "fails open" was a claim in a doc
    //! comment rather than a tested property.
    //!
    //! The distinction under test is Python's own conflation: a *missing key*
    //! means "never granted", which is a real default; a *failed lookup* means
    //! "cannot tell", which is not.

    use {
        super::{AppError, AuthContext, BypassConfig},
        odysseus_store::auth::AuthStore,
    };

    const STRICT: BypassConfig = BypassConfig {
        auth_disabled: false,
        localhost_bypass: false,
        is_configured: true,
    };

    fn auth_with(name: &str) -> (tempfile::TempDir, AuthStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let auth = AuthStore::new(dir.path());
        auth.prepare().expect("prepare");
        auth.create_user(name, "correct horse battery", false)
            .expect("create");
        (dir, auth)
    }

    #[test]
    fn a_missing_privilege_key_permits_which_is_python_parity() {
        let (_dir, auth) = auth_with("alice");
        let ctx = AuthContext::cookie_session("alice");
        assert!(
            ctx.require_privilege(&auth, "a_privilege_nobody_has_heard_of", STRICT, false)
                .is_ok(),
            "a key that was never granted is not a denial: the UI gates display-side, \
             which is the whole basis for this default (auth_helpers.py:184)"
        );
    }

    #[test]
    fn an_explicitly_false_privilege_denies() {
        let (_dir, auth) = auth_with("alice");
        // `can_use_bash` defaults to false, so it is a real stored denial rather
        // than an absent key — which is the distinction the previous test guards.
        let ctx = AuthContext::cookie_session("alice");
        let err = ctx
            .require_privilege(&auth, "can_use_bash", STRICT, false)
            .expect_err("can_use_bash defaults off, so it must deny");
        assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    }

    #[test]
    fn an_account_deleted_mid_request_loses_its_privileges() {
        // The regression, and a reachable path rather than a hypothetical one.
        // Python catches the exception and returns the user, so a request whose
        // account vanished between the session resolving and the privilege lookup
        // keeps the privilege it was just revoked.
        let dir = tempfile::tempdir().expect("tempdir");
        let auth = AuthStore::new(dir.path());
        auth.prepare().expect("prepare");
        auth.create_user("alice", "correct horse battery", false)
            .expect("create");
        auth.create_user("root", "correct horse battery", true)
            .expect("create root");

        // The session resolved while alice still existed.
        let session = auth.issue_token("alice").expect("issue");
        assert_eq!(
            auth.username_for_token(&session)
                .expect("resolve")
                .as_deref(),
            Some("alice")
        );

        // Now she is deleted. `root` is the requester, so the self-delete and
        // last-admin rules do not fire. `delete_user` revokes the owner's API
        // tokens first, so the token store needs its table.
        let store = odysseus_store::Store::open(dir.path().join("app.db"));
        store.migrate().expect("migrate");
        let tokens = odysseus_store::api_tokens::ApiTokenStore::new(store);
        auth.delete_user("alice", "root", &tokens).expect("delete");

        let ctx = AuthContext::cookie_session("alice");
        let err = ctx
            .require_privilege(&auth, "can_use_agent", STRICT, false)
            .expect_err("a deleted account must not keep its privileges");
        assert!(
            matches!(err, AppError::Forbidden(_)),
            "expected a denial, got {err:?}"
        );
    }

    #[test]
    fn the_admin_gate_and_the_privilege_gate_now_agree_on_a_lookup_failure() {
        // The asymmetry that made this an escalation path rather than a style
        // mismatch: one denied, one granted, on the same failure.
        let (_dir, auth) = auth_with("alice");
        let ghost = AuthContext::cookie_session("nobody");

        let privilege = ghost
            .require_privilege(&auth, "can_use_agent", STRICT, false)
            .expect_err("privilege gate must deny");
        let admin = ghost
            .require_admin(&auth, STRICT, false)
            .expect_err("admin gate must deny");
        assert!(matches!(privilege, AppError::Forbidden(_)));
        assert!(matches!(admin, AppError::Forbidden(_)));
    }
}
