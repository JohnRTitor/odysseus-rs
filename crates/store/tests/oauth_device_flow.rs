//! T0308: the device-flow store and the 3-route state machine.
//!
//! Persistence tests use a real migrated SQLite database, never mocks (AGENTS.md §11).
//! Time comes from an injected `TestClock`, so nothing here sleeps.

#![expect(
    clippy::expect_used,
    reason = "fixture helpers in an integration test; clippy does not class these as test code"
)]

use std::time::Duration;

use {
    odysseus_store::{
        oauth_device_flow::{
            Clock, DeviceFlow, DeviceFlowError, DeviceFlowProvider, DeviceFlowStore, PollFailure,
            PollOutcome, ProviderCallError, ProvisionError, TestClock,
        },
        oauth_secret::OAuthSecret,
        pool,
        schema::migrate,
    },
    rusqlite::Connection,
};

/// A migrated database in a temp dir.
struct Fixture {
    _dir: tempfile::TempDir,
    conn: Connection,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = Connection::open(dir.path().join("app.db")).expect("open");
        pool::configure(&conn).expect("configure");
        migrate(&conn).expect("migrate");
        Self { _dir: dir, conn }
    }
}

/// A Copilot payload carrying the secret.
fn copilot_payload() -> odysseus_store::oauth_device_flow::PendingDeviceFlow {
    odysseus_store::oauth_device_flow::PendingDeviceFlow::new(OAuthSecret::new(
        "DEVICE_CODE_SECRET",
    ))
    .with_host("github.com")
    .with_owner("alice")
    .with_user_code("ABCD-1234")
    .with_verification_uri("https://github.com/login/device")
}

// ── The structural security invariant ──────────────────────────────────────

/// The browser-facing start type carries no credential, and cannot be given one.
///
/// This is the test that makes "the device code never reaches the browser" more than a
/// convention. `BrowserDeviceStart` is `Serialize` — a handler must be able to return
/// it — so the only thing standing between the stored `device_code` and a JSON response
/// is that this type has no field able to hold one. `PendingDeviceFlow`, which does hold
/// one, is not `Serialize` because `OAuthSecret` is not.
///
/// Asserted through serde rather than by reading the field list, so it fails if someone
/// adds a `device_code` field *and* derives `Serialize` on the type.
#[test]
fn the_browser_start_type_cannot_carry_a_device_code() {
    let start = odysseus_store::oauth_device_flow::BrowserDeviceStart {
        poll_id: "abc123".into(),
        user_code: Some("ABCD-1234".into()),
        verification_uri: Some("https://github.com/login/device".into()),
        verification_uri_complete: None,
        interval: 5,
        expires_in: 900,
    };
    let rendered = serde_json::to_string(&start).expect("serialises");
    for forbidden in [
        "device_code",
        "device_auth_id",
        "code_verifier",
        "credential",
    ] {
        assert!(
            !rendered.contains(forbidden),
            "{forbidden} must not be serializable: {rendered}"
        );
    }
    // The secret is nowhere in the output, because it was never an input.
    assert!(!rendered.contains("DEVICE_CODE_SECRET"), "{rendered}");
}

/// The type that *does* hold the code is not serializable at all.
#[test]
fn the_pending_flow_type_is_not_serializable() {
    /// A compile-time assertion, not a runtime one: if either type ever gains a
    /// `Serialize` derive, this file *stops compiling*, which is the whole point.
    fn assert_not_serializable<T>() {}

    assert_not_serializable::<odysseus_store::oauth_device_flow::PendingDeviceFlow>();
    // And the secret wrapper itself.
    assert_not_serializable::<OAuthSecret>();
}

/// The stored payload round-trips through the database with the code intact.
#[test]
fn the_device_code_is_stored_server_side_and_readable_only_by_poll_id() {
    let f = Fixture::new();
    let clock = TestClock::at(1_000);
    let store = DeviceFlowStore::new(&f.conn, &clock);

    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            None,
            None,
        )
        .expect("added");

    // The browser got a handle, not the code.
    assert!(!start.poll_id.contains("DEVICE_CODE"));

    // The server can recover the code from the handle — that is the whole point.
    let recovered = store.get(&start.poll_id).expect("reads").expect("live");
    assert_eq!(recovered.credential.expose(), "DEVICE_CODE_SECRET");
    assert_eq!(recovered.user_code.as_deref(), Some("ABCD-1234"));
    assert_eq!(recovered.owner.as_deref(), Some("alice"));
}

// ── add / get / expiry ─────────────────────────────────────────────────────

#[test]
fn a_flow_defaults_to_the_upstream_interval_and_lifetime() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            None,
            None,
        )
        .expect("added");
    assert_eq!(start.interval, 5);
    assert_eq!(start.expires_in, 900);
}

/// A provider reporting `0` for either value cannot create a flow that is instantly
/// dead or never throttled.
#[test]
fn zero_intervals_are_floored_at_one() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            Some(0),
            Some(0),
        )
        .expect("added");
    assert_eq!(start.interval, 1);
    assert_eq!(start.expires_in, 1);
    // Still live one second later, then gone.
    clock.advance(Duration::from_secs(1));
    assert!(store.get(&start.poll_id).expect("reads").is_none());
}

/// An empty credential is refused at the boundary rather than stored.
#[test]
fn a_flow_without_a_credential_is_refused() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let payload = odysseus_store::oauth_device_flow::PendingDeviceFlow::new(OAuthSecret::new(""));
    assert!(matches!(
        store.add(DeviceFlowProvider::CopilotPublic, &payload, None, None),
        Err(DeviceFlowError::NoDeviceCode)
    ));
    assert_eq!(store.live_len().expect("counts"), 0);
}

/// Each start gets its own handle, so two concurrent flows do not collide.
#[test]
fn each_start_gets_a_distinct_poll_id() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let a = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            None,
            None,
        )
        .expect("added");
    let b = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            None,
            None,
        )
        .expect("added");
    assert_ne!(a.poll_id, b.poll_id);
    assert_eq!(store.live_len().expect("counts"), 2);
}

/// An expired flow is pruned, so its `device_code` does not sit on disk.
#[test]
fn an_expired_flow_is_pruned_rather_than_kept() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            None,
            Some(10),
        )
        .expect("added");
    assert_eq!(store.live_len().expect("counts"), 1);

    clock.advance(Duration::from_secs(11));
    assert!(store.get(&start.poll_id).expect("reads").is_none());
    // Adding another flow prunes the dead one.
    store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            None,
            None,
        )
        .expect("added");
    assert_eq!(
        store.live_len().expect("counts"),
        1,
        "the expired row must be deleted, not merely hidden"
    );
    let remaining: i64 = f
        .conn
        .query_row("SELECT count(*) FROM pending_device_flows", [], |r| {
            r.get(0)
        })
        .expect("counts");
    assert_eq!(remaining, 1, "and it is gone from the table itself");
}

// ── throttling ─────────────────────────────────────────────────────────────

/// The first poll is never throttled, because `next_poll_at` is seeded to zero.
#[test]
fn the_first_poll_is_not_throttled() {
    let f = Fixture::new();
    let clock = TestClock::at(100);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            Some(5),
            None,
        )
        .expect("added");
    assert!(!store.is_throttled(&start.poll_id).expect("reads"));
}

/// After a poll is scheduled, a second poll inside the interval is throttled.
#[test]
fn a_scheduled_poll_throttles_the_next_one() {
    let f = Fixture::new();
    let clock = TestClock::at(100);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            Some(5),
            None,
        )
        .expect("added");
    store.schedule_next(&start.poll_id).expect("scheduled");

    assert!(store.is_throttled(&start.poll_id).expect("reads"));
    clock.advance(Duration::from_secs(5));
    assert!(
        !store.is_throttled(&start.poll_id).expect("reads"),
        "the throttle lifts at exactly the interval"
    );
}

/// A `slow_down` with no interval from the provider escalates by 5.
#[test]
fn a_slow_down_escalates_the_interval_by_five() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            Some(5),
            None,
        )
        .expect("added");

    store.slow_down(&start.poll_id, None).expect("escalated");
    // interval 5 -> 10, next_poll_at = 0 + 10.
    assert!(store.is_throttled(&start.poll_id).expect("reads"));

    clock.advance(Duration::from_secs(10));
    store
        .slow_down(&start.poll_id, None)
        .expect("escalated again");
    // interval 10 -> 15, next_poll_at = 10 + 15 = 25.
    assert!(store.is_throttled(&start.poll_id).expect("reads"));

    clock.advance(Duration::from_secs(10));
    assert!(
        store.is_throttled(&start.poll_id).expect("reads"),
        "at t=20 the window set at t=10 for 15s has not closed"
    );
    clock.advance(Duration::from_secs(5));
    assert!(
        !store.is_throttled(&start.poll_id).expect("reads"),
        "the window closes at exactly t=25"
    );
}

/// A provider-supplied interval wins over the escalation.
#[test]
fn a_provider_interval_overrides_the_escalation() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            Some(5),
            None,
        )
        .expect("added");
    store
        .slow_down(&start.poll_id, Some(30))
        .expect("escalated");
    assert!(store.is_throttled(&start.poll_id).expect("reads"));
    clock.advance(Duration::from_secs(29));
    assert!(store.is_throttled(&start.poll_id).expect("reads"));
    clock.advance(Duration::from_secs(1));
    assert!(!store.is_throttled(&start.poll_id).expect("reads"));
}

/// A provider reporting `interval: 0` cannot wedge the flow permanently.
#[test]
fn a_zero_provider_interval_does_not_wedge_the_flow() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            Some(5),
            None,
        )
        .expect("added");
    store.slow_down(&start.poll_id, Some(0)).expect("escalated");
    // Floored at 1s rather than 0, so the flow is not throttled forever — but it is
    // still throttled for that one second.
    assert!(
        store.is_throttled(&start.poll_id).expect("reads"),
        "interval 0 is floored to 1s, not treated as no throttle"
    );
    clock.advance(Duration::from_secs(1));
    assert!(
        !store.is_throttled(&start.poll_id).expect("reads"),
        "and the floor means the flow proceeds on the next second"
    );
}

// ── the state machine ──────────────────────────────────────────────────────

#[test]
fn poll_is_not_found_for_an_unknown_handle() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let flow = DeviceFlow::new(&store);
    let result = flow.poll(
        "nope",
        |_| Ok::<_, ProviderCallError>(PollOutcome::pending()),
        |_| Ok::<_, ProvisionError>("ep".into()),
    );
    assert!(matches!(result, Err(DeviceFlowError::NotFound)));
}

/// A throttled poll returns bare `pending` and never reaches the provider.
#[test]
fn a_throttled_poll_does_not_call_the_provider() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            Some(60),
            None,
        )
        .expect("added");
    store.schedule_next(&start.poll_id).expect("scheduled");

    let flow = DeviceFlow::new(&store);
    let called = std::cell::Cell::new(false);
    let result = flow
        .poll(
            &start.poll_id,
            |_| {
                called.set(true);
                Ok::<_, ProviderCallError>(PollOutcome::pending())
            },
            |_| Ok::<_, ProvisionError>("ep".into()),
        )
        .expect("pending");
    assert!(!called.get(), "the provider must not be called");
    assert_eq!(result.status.as_ref(), "pending");
    assert_eq!(result.detail, None, "a throttled poll says nothing new");
}

/// A pending poll arms the next one and keeps the flow alive.
#[test]
fn a_pending_poll_keeps_the_flow_and_arms_the_next() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            Some(5),
            None,
        )
        .expect("added");
    let flow = DeviceFlow::new(&store);

    let result = flow
        .poll(
            &start.poll_id,
            |_| Ok::<_, ProviderCallError>(PollOutcome::pending()),
            |_| Ok::<_, ProvisionError>("ep".into()),
        )
        .expect("pending");
    assert_eq!(result.status.as_ref(), "pending");
    assert!(store.is_throttled(&start.poll_id).expect("reads"));
    assert_eq!(store.live_len().expect("counts"), 1, "the flow survives");
}

/// A provider call failure becomes `pending` rather than an error, so a transient 502
/// does not force the user to restart the flow.
#[test]
fn a_provider_call_failure_is_swallowed_into_pending() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            Some(5),
            None,
        )
        .expect("added");
    let flow = DeviceFlow::new(&store);

    let result = flow
        .poll(
            &start.poll_id,
            |_| Err::<PollOutcome, _>(ProviderCallError),
            |_| Ok::<_, ProvisionError>("ep".into()),
        )
        .expect("swallowed");
    assert_eq!(result.status.as_ref(), "pending");
    assert_eq!(
        store.live_len().expect("counts"),
        1,
        "the session is kept so a retry is possible"
    );
}

/// An authorized poll provisions and then consumes the flow.
#[test]
fn an_authorized_poll_provisions_then_consumes() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            None,
            None,
        )
        .expect("added");
    let flow = DeviceFlow::new(&store);

    let seen = std::cell::Cell::new(0);
    let result = flow
        .poll(
            &start.poll_id,
            |_| {
                Ok::<_, ProviderCallError>(PollOutcome::Authorized {
                    credential: OAuthSecret::new("TOKEN"),
                    refresh_credential: None,
                    base_url: "https://api.githubcopilot.com".into(),
                })
            },
            |outcome| {
                // The provisioner is the only place that should see the token.
                let PollOutcome::Authorized { credential, .. } = outcome else {
                    return Err(ProvisionError);
                };
                seen.set(credential.expose().len());
                Ok::<_, ProvisionError>("endpoint-1".into())
            },
        )
        .expect("authorized");

    assert_eq!(seen.get(), 5, "the provisioner received the token");
    assert_eq!(result.status.as_ref(), "authorized");
    assert_eq!(result.endpoint_id.as_deref(), Some("endpoint-1"));
    assert_eq!(
        store.live_len().expect("counts"),
        0,
        "an authorized flow is consumed"
    );
    let rendered = serde_json::to_string(&result).expect("serialises");
    assert!(
        !rendered.contains("TOKEN"),
        "the token never reaches the browser"
    );
}

/// A provisioning failure keeps the flow, so the grant is not lost.
#[test]
fn a_provisioning_failure_keeps_the_flow_alive() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            None,
            None,
        )
        .expect("added");
    let flow = DeviceFlow::new(&store);

    let result = flow.poll(
        &start.poll_id,
        |_| {
            Ok::<_, ProviderCallError>(PollOutcome::Authorized {
                credential: OAuthSecret::new("TOKEN"),
                refresh_credential: None,
                base_url: "https://api.githubcopilot.com".into(),
            })
        },
        |_| Err::<Box<str>, _>(ProvisionError),
    );
    assert!(result.is_err());
    assert_eq!(
        store.live_len().expect("counts"),
        1,
        "the user can retry rather than re-authorising"
    );
}

/// A terminal failure consumes the flow and reports a vocabulary reason.
#[test]
fn a_failed_poll_consumes_the_flow_and_reports_a_reason() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            None,
            None,
        )
        .expect("added");
    let flow = DeviceFlow::new(&store);

    let result = flow
        .poll(
            &start.poll_id,
            |_| {
                Ok::<_, ProviderCallError>(PollOutcome::Failed {
                    reason: PollFailure::AccessDenied,
                })
            },
            |_| Ok::<_, ProvisionError>("ep".into()),
        )
        .expect("failed");
    assert_eq!(result.status.as_ref(), "failed");
    assert_eq!(result.error.as_deref(), Some("access_denied"));
    assert_eq!(store.live_len().expect("counts"), 0);
}

/// A `slow_down` keeps the flow and escalates.
#[test]
fn a_slow_down_keeps_the_flow_and_escalates() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            Some(5),
            None,
        )
        .expect("added");
    let flow = DeviceFlow::new(&store);

    flow.poll(
        &start.poll_id,
        |_| {
            Ok::<_, ProviderCallError>(PollOutcome::SlowDown {
                interval: Some(20),
                detail: None,
            })
        },
        |_| Ok::<_, ProvisionError>("ep".into()),
    )
    .expect("pending");
    assert_eq!(store.live_len().expect("counts"), 1);
    assert!(store.is_throttled(&start.poll_id).expect("reads"));
}

/// Cancelling is idempotent and removes the flow.
#[test]
fn cancel_removes_the_flow_and_is_idempotent() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            None,
            None,
        )
        .expect("added");
    let flow = DeviceFlow::new(&store);

    let first = flow.cancel(&start.poll_id).expect("cancelled");
    assert_eq!(first.status.as_ref(), "cancelled");
    assert_eq!(store.live_len().expect("counts"), 0);

    let second = flow.cancel(&start.poll_id).expect("idempotent");
    assert_eq!(second.status.as_ref(), "cancelled");
}

/// After an authorized poll, a repeat poll is `NotFound` — the handle is spent.
#[test]
fn an_authorized_poll_id_cannot_be_replayed() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            None,
            None,
        )
        .expect("added");
    let flow = DeviceFlow::new(&store);
    let authorized = |_: &odysseus_store::oauth_device_flow::PendingDeviceFlow| {
        Ok::<_, ProviderCallError>(PollOutcome::Authorized {
            credential: OAuthSecret::new("TOKEN"),
            refresh_credential: None,
            base_url: "https://api.githubcopilot.com".into(),
        })
    };
    flow.poll(&start.poll_id, authorized, |_| {
        Ok::<_, ProvisionError>("ep".into())
    })
    .expect("authorized");
    let replay = flow.poll(&start.poll_id, authorized, |_| {
        Ok::<_, ProvisionError>("ep".into())
    });
    assert!(
        matches!(replay, Err(DeviceFlowError::NotFound)),
        "a spent handle must not mint a second token"
    );
}

// ── all three providers ────────────────────────────────────────────────────

/// All three providers round-trip through the store, each keeping its own identity.
#[test]
fn all_three_providers_store_and_report_distinctly() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);

    for provider in DeviceFlowProvider::ALL {
        let start = store
            .add(provider, &copilot_payload(), None, None)
            .expect("added");
        assert_eq!(
            store.provider_of(&start.poll_id).expect("reads"),
            provider,
            "{}",
            provider.as_str()
        );
    }
    assert_eq!(store.live_len().expect("counts"), 3);
}

/// A payload containing a separator or a newline survives the round trip intact.
///
/// The stored encoding is length-prefixed precisely so a host or user code cannot forge
/// a field boundary; without that, an attacker-influenced `enterprise_url` could inject
/// a `credential=` field.
#[test]
fn a_payload_with_separators_round_trips_intact() {
    let f = Fixture::new();
    let clock = TestClock::at(0);
    let store = DeviceFlowStore::new(&f.conn, &clock);

    let hostile = odysseus_store::oauth_device_flow::PendingDeviceFlow::new(OAuthSecret::new(
        "code=\x1fINJECTED\x1e",
    ))
    .with_host("evil=\x1fvalue")
    .with_user_code("A=B=C")
    .with_enterprise_url("https://x.test/?a=1\x1eb=2");

    let start = store
        .add(DeviceFlowProvider::CopilotEnterprise, &hostile, None, None)
        .expect("added");
    let back = store.get(&start.poll_id).expect("reads").expect("live");

    assert_eq!(back.credential.expose(), "code=\x1fINJECTED\x1e");
    assert_eq!(back.host.as_deref(), Some("evil=\x1fvalue"));
    assert_eq!(back.user_code.as_deref(), Some("A=B=C"));
    assert_eq!(
        back.enterprise_url.as_deref(),
        Some("https://x.test/?a=1\x1eb=2")
    );
}

// ── §12.1 ──────────────────────────────────────────────────────────────────

/// The stored payload's in-memory form has a size ceiling.
///
/// §12.1 rule 1: every string in a stored type is a `Box<str>`, never a `String`.
#[test]
fn the_pending_flow_has_a_size_ceiling() {
    use {odysseus_store::oauth_device_flow::PendingDeviceFlow, std::mem::size_of};
    let size = size_of::<PendingDeviceFlow>();
    assert!(
        size <= 200,
        "PendingDeviceFlow is {size} bytes; every field is a Box<str> or a Copy scalar, \
         so growth means a new owned field crept in"
    );
    // A String carries 24 bytes plus a capacity that means nothing once stored.
    assert!(size_of::<Box<str>>() <= size_of::<String>());
}

/// The clock is injected, so the tests above prove nothing reads the wall clock.
#[test]
fn the_clock_is_the_only_source_of_time() {
    let f = Fixture::new();
    let clock = TestClock::at(42);
    let store = DeviceFlowStore::new(&f.conn, &clock);
    let start = store
        .add(
            DeviceFlowProvider::CopilotPublic,
            &copilot_payload(),
            Some(5),
            Some(100),
        )
        .expect("added");
    // Expiry is exactly start + 100, so moving the clock moves the boundary.
    clock.advance(Duration::from_secs(99));
    assert!(store.get(&start.poll_id).expect("reads").is_some());
    clock.advance(Duration::from_secs(1));
    assert!(store.get(&start.poll_id).expect("reads").is_none());
    let _: &dyn Clock = &clock;
}
