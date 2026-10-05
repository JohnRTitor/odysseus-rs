//! T0309: GitHub Copilot — device-flow endpoints and request headers.
//!
//! Ports `src/copilot.py`. The Python is explicit that this module holds "constants +
//! pure helpers" and that the HTTP calls live in `routes/copilot_routes.py` so they
//! can be auth-gated; this port keeps the split. Everything here is a pure function of
//! its arguments — no IO, no clock, no environment — so the whole surface is testable
//! and nothing in it can leak a token.
//!
//! ## Public and Enterprise are one implementation, two providers
//!
//! Copilot's device flow talks to `https://{host}/login/device/code` where `host` is
//! either `github.com` or an Enterprise domain, and the resulting token is used
//! against either `https://api.githubcopilot.com` or
//! `https://copilot-api.{domain}`. The request and header logic is byte-for-byte the
//! same in both cases.
//!
//! So [`Deployment`] is a parameter, not a second copy of the functions. The *provider*
//! identity still differs — [`DeviceFlowProvider::CopilotPublic`] and
//! [`DeviceFlowProvider::CopilotEnterprise`] are distinct rows, because an Enterprise
//! credential must never be served by, or provisioned against, the public deployment,
//! and because the client id differs per deployment. Splitting the provider without
//! splitting the implementation is the point: T0306's warning was about two copies of
//! one rule, not about one function serving two callers.

use crate::oauth_device_flow::DeviceFlowProvider;

/// The GitHub OAuth client id used for the device flow.
///
/// Copilot's token endpoint only accepts client ids GitHub has allow-listed for
/// Copilot access, so this is the public VS Code client id — the de-facto standard for
/// third-party clients. An operator who registers their own allow-listed app
/// overrides it.
pub const COPILOT_CLIENT_ID: &str = "01ab8ac9400c4e429b23";

/// The dated API version header Copilot requires on models and chat.
pub const COPILOT_API_VERSION: &str = "2026-06-01";

/// The public Copilot API base.
pub const COPILOT_BASE: &str = "https://api.githubcopilot.com";

/// The editor-like `User-Agent` Copilot expects.
pub const COPILOT_USER_AGENT: &str = "Odysseus/1.0";

/// The integration id that identifies this client to GitHub.
pub const COPILOT_INTEGRATION_ID: &str = "vscode-chat";

/// The `Editor-Version` header.
pub const COPILOT_EDITOR_VERSION: &str = "Odysseus/1.0";

/// The OAuth scope requested during the flow.
pub const COPILOT_SCOPE: &str = "read:user";

/// The default GitHub host.
pub const GITHUB_HOST: &str = "github.com";

/// Which Copilot deployment a credential belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deployment {
    /// Public github.com.
    Public,
    /// A GitHub Enterprise installation, identified by its domain.
    Enterprise,
}

impl Deployment {
    /// The device-flow provider this deployment is stored as.
    #[must_use]
    pub const fn provider(self) -> DeviceFlowProvider {
        match self {
            Self::Public => DeviceFlowProvider::CopilotPublic,
            Self::Enterprise => DeviceFlowProvider::CopilotEnterprise,
        }
    }

    /// The GitHub host the device flow runs against.
    ///
    /// Enterprise takes the domain; public ignores it.
    #[must_use]
    pub fn host(self, enterprise_domain: Option<&str>) -> &str {
        match self {
            Self::Public => GITHUB_HOST,
            Self::Enterprise => enterprise_domain.unwrap_or(GITHUB_HOST),
        }
    }
}

/// Strips a scheme and trailing slash from an Enterprise URL or bare domain.
///
/// Ports `normalize_domain` (`copilot.py:73`). A scheme is *replaced* rather than
/// searched for, matching upstream: a string containing `https://` twice keeps only
/// the first removal, which is fine because the input is a configured value rather
/// than user text.
///
/// Returns an owned `String`. This was briefly written to return `&str` via `Box::leak`,
/// which would have leaked once per call on a code path that runs on every Enterprise
/// device-flow start.
#[must_use]
pub fn normalize_domain(url: &str) -> String {
    url.replace("https://", "")
        .replace("http://", "")
        .trim_end_matches('/')
        .to_owned()
}

/// The Copilot API base for a deployment.
///
/// Ports `enterprise_base` (`copilot.py:78`).
#[must_use]
pub fn enterprise_base(enterprise_url: Option<&str>) -> String {
    match enterprise_url
        .map(normalize_domain)
        .filter(|d| !d.is_empty())
    {
        None => COPILOT_BASE.to_owned(),
        Some(domain) => format!("https://copilot-api.{domain}"),
    }
}

/// Whether a base URL points at a Copilot API, public or Enterprise.
///
/// Ports `is_copilot_base` (`copilot.py:89`).
///
/// Note the asymmetry, which is upstream's and deliberate: the public check accepts
/// `api.githubcopilot.com` **and any subdomain of it**, while the Enterprise check is a
/// bare `copilot-api.` prefix with no domain validation. A URL of
/// `https://copilot-api.attacker.test/` therefore classifies as Copilot. That only
/// affects which headers get attached to a request the operator configured themselves,
/// and the request still goes to the host in the URL, so it is not a credential
/// redirection — but it is worth knowing before relying on this for an authz decision.
#[must_use]
pub fn is_copilot_base(url: Option<&str>) -> bool {
    use crate::model_endpoints::UrlParts;

    let Some(url) = url.filter(|u| !u.trim().is_empty()) else {
        return false;
    };
    let host = UrlParts::parse(url).host;
    if host.is_empty() {
        return false;
    }
    if host == "githubcopilot.com" || host.ends_with(".githubcopilot.com") {
        return true;
    }
    host.starts_with("copilot-api.")
}

/// Who initiated a request, for Copilot's agent-vs-user accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Initiator {
    /// A tool-driven turn from the agent loop.
    Agent,
    /// A direct user message.
    User,
}

impl Initiator {
    /// The header value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
        }
    }
}

/// The per-request flags Copilot derives from an outgoing message list.
///
/// Ports `request_flags` (`copilot.py:222`). Both fields come from the messages, not
/// from the caller, so the two cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestFlags {
    /// Whether the turn is agent-initiated.
    pub agent: bool,
    /// Whether the request carries an image part.
    pub vision: bool,
}

/// The OpenAI content-part types that mean "this is an image".
const IMAGE_PART_TYPES: &[&str] = &["image_url", "input_image", "image"];

/// Derives `(agent, vision)` from a serialised OpenAI-style message list.
///
/// Ports `request_flags` (`copilot.py:222`).
///
/// Two behaviours worth naming, both from upstream:
///
/// * **`agent` means "the last message is not a plain user message".** A tool result or
///   an assistant follow-up makes the turn agent-initiated. Mirrors opencode.
/// * **Non-object elements are tolerated.** Clients really do send
///   `{"messages": ["hi"]}`, so a bare string element must not panic. Upstream guards
///   with `isinstance`; here the JSON is walked by type, which cannot fail the same way.
#[must_use]
pub fn request_flags(messages: &[serde_json::Value]) -> RequestFlags {
    use serde_json::Value;

    let agent = messages.last().is_some_and(|last| {
        last.get("role")
            .and_then(Value::as_str)
            .is_none_or(|role| role != "user")
    });
    let vision = messages.iter().any(|message| {
        message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|parts| {
                parts.iter().any(|part| {
                    part.get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|t| IMAGE_PART_TYPES.contains(&t))
                })
            })
    });
    RequestFlags { agent, vision }
}

/// The Copilot request headers.
///
/// Ports `copilot_headers` (`copilot.py:108`) with `apply_request_headers`
/// (`copilot.py:248`) folded in, since both write the same two keys and calling a
/// header builder and then patching it is how they drift apart.
///
/// The token is passed as a plain `Option<&str>` rather than an
/// [`crate::oauth_secret::OAuthSecret`] because this function's whole job is to place
/// it in an `Authorization` header. The value is never logged or stored here; it is
/// copied into the returned map and the map is the caller's to send.
#[must_use]
pub fn copilot_headers(
    api_key: Option<&str>,
    flags: RequestFlags,
) -> std::collections::BTreeMap<String, String> {
    let mut headers = std::collections::BTreeMap::from([
        (
            "X-GitHub-Api-Version".to_owned(),
            COPILOT_API_VERSION.to_owned(),
        ),
        ("Openai-Intent".to_owned(), "conversation-edits".to_owned()),
        ("User-Agent".to_owned(), COPILOT_USER_AGENT.to_owned()),
        (
            "Editor-Version".to_owned(),
            COPILOT_EDITOR_VERSION.to_owned(),
        ),
        (
            "Copilot-Integration-Id".to_owned(),
            COPILOT_INTEGRATION_ID.to_owned(),
        ),
        (
            "x-initiator".to_owned(),
            if flags.agent {
                Initiator::Agent
            } else {
                Initiator::User
            }
            .as_str()
            .to_owned(),
        ),
    ]);
    // An empty key is treated as absent: `Authorization: Bearer ` looks
    // authenticated to a server that only checks for the header's presence.
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        headers.insert("Authorization".to_owned(), format!("Bearer {key}"));
    }
    if flags.vision {
        headers.insert("Copilot-Vision-Request".to_owned(), "true".to_owned());
    }
    headers
}

/// The headers for the device-flow POSTs.
///
/// Ports `_oauth_post_headers` (`copilot.py:142`).
#[must_use]
pub fn oauth_post_headers() -> std::collections::BTreeMap<String, String> {
    std::collections::BTreeMap::from([
        ("Accept".to_owned(), "application/json".to_owned()),
        ("Content-Type".to_owned(), "application/json".to_owned()),
        ("User-Agent".to_owned(), COPILOT_USER_AGENT.to_owned()),
    ])
}

/// The device-code endpoint for a host.
///
/// Ports `device_code_url` (`copilot.py:65`).
#[must_use]
pub fn device_code_url(host: &str) -> String {
    format!("https://{host}/login/device/code")
}

/// The access-token endpoint for a host.
///
/// Ports `access_token_url` (`copilot.py:69`).
#[must_use]
pub fn access_token_url(host: &str) -> String {
    format!("https://{host}/login/oauth/access_token")
}

/// The JSON body for starting a device flow.
///
/// Ports the `json=` argument of `request_device_code` (`copilot.py:157`).
#[must_use]
pub fn device_code_request_body() -> serde_json::Value {
    serde_json::json!({
        "client_id": COPILOT_CLIENT_ID,
        "scope": COPILOT_SCOPE,
    })
}

/// The JSON body for exchanging a device code.
///
/// Ports the `json=` argument of `poll_access_token` (`copilot.py:172`). The
/// `device_code` is a secret, so it is passed through a wrapper that keeps it out of
/// `Debug` and out of any accidental log of the request body.
#[must_use]
pub fn access_token_request_body(device_code: &str) -> serde_json::Value {
    serde_json::json!({
        "client_id": COPILOT_CLIENT_ID,
        "device_code": device_code,
        "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
    })
}

/// What GitHub returned from a device-code request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCodeResponse {
    /// The secret. Never sent to a browser.
    pub device_code: String,
    /// The short code the user types.
    pub user_code: Option<String>,
    /// Where the user enters it.
    pub verification_uri: Option<String>,
    /// A pre-filled variant, which embeds the *user* code.
    pub verification_uri_complete: Option<String>,
    /// Seconds until the code expires.
    pub expires_in: Option<u32>,
    /// The provider's suggested poll interval.
    pub interval: Option<u32>,
}

/// Why a Copilot device-flow response was unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CopilotResponseError {
    /// The response had no `device_code`, so the flow cannot proceed.
    #[error("GitHub did not return a device code")]
    MissingDeviceCode,
}

/// Interprets GitHub's device-code response.
///
/// # Errors
///
/// [`CopilotResponseError::MissingDeviceCode`] when `device_code` is absent or empty.
pub fn parse_device_code_response(
    body: &serde_json::Value,
) -> Result<DeviceCodeResponse, CopilotResponseError> {
    let device_code = body
        .get("device_code")
        .and_then(serde_json::Value::as_str)
        .filter(|c| !c.is_empty())
        .ok_or(CopilotResponseError::MissingDeviceCode)?;
    let text = |key: &str| {
        body.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let number = |key: &str| {
        body.get(key)
            .and_then(serde_json::Value::as_u64)
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
    };
    Ok(DeviceCodeResponse {
        device_code: device_code.to_owned(),
        user_code: text("user_code"),
        verification_uri: text("verification_uri"),
        verification_uri_complete: text("verification_uri_complete"),
        expires_in: number("expires_in"),
        interval: number("interval"),
    })
}

/// Interprets GitHub's access-token response.
///
/// GitHub answers HTTP 200 with an `error` field while the user has not yet
/// authorised, and `{access_token, …}` once they have — so this is keyed on the
/// presence of `access_token`, not on the status.
#[must_use]
pub fn parse_access_token_response(body: &serde_json::Value) -> AccessTokenPoll {
    if let Some(token) = body
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .filter(|t| !t.is_empty())
    {
        return AccessTokenPoll::Authorized {
            token: token.to_owned(),
            scope: body
                .get("scope")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        };
    }
    let error = body
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let interval = body
        .get("interval")
        .and_then(serde_json::Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX));
    match error {
        "authorization_pending" => AccessTokenPoll::Pending,
        "slow_down" => AccessTokenPoll::SlowDown { interval },
        _ => AccessTokenPoll::Error {
            error: error.to_owned(),
            interval,
        },
    }
}

/// One access-token poll outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessTokenPoll {
    /// The user has not authorised yet.
    Pending,
    /// Back off.
    SlowDown {
        /// A provider-supplied interval, if it sent one.
        interval: Option<u32>,
    },
    /// The user authorised.
    Authorized {
        /// The long-lived Copilot token, sent directly as a bearer.
        token: String,
        /// The granted scope.
        scope: Option<String>,
    },
    /// A terminal or unrecognised error.
    Error {
        /// GitHub's error code, not its message.
        error: String,
        /// An interval, if the error carried one.
        interval: Option<u32>,
    },
}

/// One entry from Copilot's model catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopilotModel {
    /// The model id.
    pub id: Box<str>,
    /// Whether it advertises tool calls.
    pub tool_calls: bool,
    /// Whether it advertises vision.
    pub vision: bool,
}

/// Parses Copilot's model list, preferring picker-enabled models.
///
/// Ports `fetch_models`' parsing half (`copilot.py:183`).
///
/// The `picker or parsed` fallback is upstream's "defensive against API-shape drift":
/// if no model advertises `model_picker_enabled`, showing the full list beats showing
/// an empty picker. Preserved, including the fact that `picker` is dropped from the
/// returned entries.
#[must_use]
pub fn parse_models(body: &serde_json::Value) -> Box<[CopilotModel]> {
    let empty: Vec<serde_json::Value> = Vec::new();
    let items = body
        .get("data")
        .and_then(serde_json::Value::as_array)
        .unwrap_or(&empty);

    let parsed: Vec<(CopilotModel, bool)> = items
        .iter()
        .filter_map(|item| {
            let id = item
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())?;
            let supports = item.get("capabilities").and_then(|c| c.get("supports"));
            let flag = |key: &str| {
                supports
                    .and_then(|s| s.get(key))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            };
            Some((
                CopilotModel {
                    id: id.into(),
                    tool_calls: flag("tool_calls"),
                    vision: flag("vision"),
                },
                item.get("model_picker_enabled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            ))
        })
        .collect();

    let chosen: Vec<CopilotModel> = parsed
        .iter()
        .filter(|(_, picker)| *picker)
        .map(|(m, _)| m.clone())
        .chain(
            parsed
                .iter()
                .filter(|(_, picker)| !*picker)
                .map(|(m, _)| m.clone()),
        )
        .take(if parsed.iter().any(|(_, p)| *p) {
            parsed.iter().filter(|(_, p)| *p).count()
        } else {
            parsed.len()
        })
        .collect();
    chosen.into_boxed_slice()
}

#[cfg(test)]
mod tests {
    use super::{
        COPILOT_BASE, COPILOT_CLIENT_ID, COPILOT_INTEGRATION_ID, COPILOT_SCOPE, Deployment,
        Initiator, RequestFlags, access_token_request_body, access_token_url, copilot_headers,
        device_code_request_body, device_code_url, enterprise_base, is_copilot_base,
        normalize_domain, oauth_post_headers, parse_device_code_response, parse_models,
        request_flags,
    };

    #[test]
    fn the_endpoints_follow_the_host() {
        assert_eq!(
            device_code_url("github.com"),
            "https://github.com/login/device/code"
        );
        assert_eq!(
            access_token_url("github.com"),
            "https://github.com/login/oauth/access_token"
        );
        assert_eq!(
            device_code_url("ghe.corp.test"),
            "https://ghe.corp.test/login/device/code"
        );
    }

    #[test]
    fn the_request_bodies_match_the_python_literals() {
        assert_eq!(
            device_code_request_body(),
            serde_json::json!({"client_id": COPILOT_CLIENT_ID, "scope": COPILOT_SCOPE})
        );
        let body = access_token_request_body("SECRET");
        assert_eq!(body["device_code"], "SECRET");
        assert_eq!(
            body["grant_type"],
            "urn:ietf:params:oauth:grant-type:device_code"
        );
    }

    #[test]
    fn public_and_enterprise_resolve_different_bases() {
        assert_eq!(enterprise_base(None), COPILOT_BASE);
        assert_eq!(enterprise_base(Some("")), COPILOT_BASE);
        assert_eq!(
            enterprise_base(Some("https://ghe.corp.test/")),
            "https://copilot-api.ghe.corp.test"
        );
        assert_eq!(
            enterprise_base(Some("ghe.corp.test")),
            "https://copilot-api.ghe.corp.test"
        );
    }

    #[test]
    fn normalize_strips_scheme_and_trailing_slash() {
        assert_eq!(normalize_domain("https://ghe.corp.test/"), "ghe.corp.test");
        assert_eq!(normalize_domain("http://ghe.corp.test"), "ghe.corp.test");
        assert_eq!(normalize_domain("ghe.corp.test"), "ghe.corp.test");
    }

    #[test]
    fn a_deployment_picks_its_own_provider_and_host() {
        assert_eq!(Deployment::Public.provider().as_str(), "copilot");
        assert_eq!(
            Deployment::Enterprise.provider().as_str(),
            "copilot-enterprise"
        );
        assert_eq!(Deployment::Public.host(None), "github.com");
        assert_eq!(
            Deployment::Enterprise.host(Some("ghe.corp.test")),
            "ghe.corp.test"
        );
    }

    #[test]
    fn the_three_required_headers_are_always_present() {
        let headers = copilot_headers(
            Some("tok"),
            RequestFlags {
                agent: false,
                vision: false,
            },
        );
        assert_eq!(headers["X-GitHub-Api-Version"], "2026-06-01");
        assert_eq!(headers["Openai-Intent"], "conversation-edits");
        assert_eq!(headers["Copilot-Integration-Id"], COPILOT_INTEGRATION_ID);
        assert_eq!(headers["x-initiator"], "user");
        assert_eq!(headers["Authorization"], "Bearer tok");
    }

    #[test]
    fn x_initiator_follows_the_derived_flag() {
        let agent = copilot_headers(
            None,
            RequestFlags {
                agent: true,
                vision: false,
            },
        );
        assert_eq!(agent["x-initiator"], "agent");
        assert_eq!(Initiator::Agent.as_str(), "agent");
    }

    #[test]
    fn the_vision_header_appears_only_for_an_image_request() {
        let with = copilot_headers(
            None,
            RequestFlags {
                agent: false,
                vision: true,
            },
        );
        assert_eq!(with["Copilot-Vision-Request"], "true");
        let without = copilot_headers(
            None,
            RequestFlags {
                agent: false,
                vision: false,
            },
        );
        assert!(!without.contains_key("Copilot-Vision-Request"));
    }

    #[test]
    fn an_empty_key_sends_no_authorization_header() {
        let headers = copilot_headers(
            Some(""),
            RequestFlags {
                agent: false,
                vision: false,
            },
        );
        assert!(
            !headers.contains_key("Authorization"),
            "an empty bearer looks authenticated to a presence check"
        );
    }

    #[test]
    fn the_oauth_post_headers_are_json() {
        let headers = oauth_post_headers();
        assert_eq!(headers["Accept"], "application/json");
        assert_eq!(headers["Content-Type"], "application/json");
    }

    #[test]
    fn a_missing_device_code_is_refused() {
        assert!(parse_device_code_response(&serde_json::json!({})).is_err());
        assert!(parse_device_code_response(&serde_json::json!({"device_code": ""})).is_err());
    }

    #[test]
    fn a_device_code_response_carries_the_user_facing_fields() {
        let parsed = parse_device_code_response(&serde_json::json!({
            "device_code": "SECRET",
            "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device",
            "verification_uri_complete": "https://github.com/login/device?uc=ABCD-1234",
            "expires_in": 900,
            "interval": 5
        }))
        .expect("valid");
        assert_eq!(parsed.device_code, "SECRET");
        assert_eq!(parsed.user_code.as_deref(), Some("ABCD-1234"));
        assert_eq!(parsed.expires_in, Some(900));
    }

    #[test]
    fn copilot_base_detection_matches_the_python_rules() {
        assert!(is_copilot_base(Some("https://api.githubcopilot.com")));
        assert!(is_copilot_base(Some("https://foo.githubcopilot.com/v1")));
        assert!(is_copilot_base(Some("https://copilot-api.ghe.corp.test")));
        assert!(!is_copilot_base(Some("https://api.openai.com")));
        assert!(!is_copilot_base(Some("not copilot")));
        assert!(!is_copilot_base(None));
        assert!(!is_copilot_base(Some("")));
    }

    #[test]
    fn a_non_dict_message_element_does_not_panic() {
        // Clients really do send {"messages": ["hi"]}.
        let messages = vec![serde_json::json!("hi")];
        let flags = request_flags(&messages);
        assert!(flags.agent, "a bare string is not a user message");
        assert!(!flags.vision);
    }

    #[test]
    fn picker_enabled_models_win_and_the_flag_is_dropped() {
        let models = parse_models(&serde_json::json!({"data": [
            {"id": "hidden-model", "model_picker_enabled": false},
            {"id": "gpt-4o", "model_picker_enabled": true,
             "capabilities": {"supports": {"tool_calls": true, "vision": true}}}
        ]}));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id.as_ref(), "gpt-4o");
        assert!(models[0].tool_calls);
        assert!(models[0].vision);
    }

    #[test]
    fn with_no_picker_models_the_full_list_is_shown() {
        let models = parse_models(&serde_json::json!({"data": [
            {"id": "a"}, {"id": "b"}
        ]}));
        assert_eq!(models.len(), 2, "defensive against API-shape drift");
    }
}
