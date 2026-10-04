//! Proof that the T0005 test dependencies resolve, compile and work.
//!
//! Every entry exists because it was added to `[workspace.dependencies]` in T0005,
//! and a dependency nobody exercises is one the next tidy-up deletes. This file is
//! the smallest honest exercise of each of them, deliberately kept in the store
//! crate because the properties it checks are store properties.
//!
//! It is NOT the place for the real property tests. AGENTS.md §11 wants proptest
//! on the sanitizers and parsers and insta on the SSE sequences; those belong in
//! the suites that own them (`crates/store/src/**` and the web crate's SSE
//! tests), which this change does not touch. What lives here is the wiring.

use odysseus_store::{
    api_tokens::{Scope, TokenProfile, join_scopes, split_scopes},
    atomic::atomic_write_bytes,
};

// ---------------------------------------------------------------- proptest

use proptest::prelude::*;

// One case is deliberately NOT the identity: an empty scope set round-trips to the
// default, because `split_scopes` maps `None` and the blank string to `DEFAULT_SCOPE`
// (`api_tokens.rs:420-424`). Asserting that separately keeps the round-trip property
// honest instead of quietly excluding the empty case.
//
// This is the shape the real parser tests in `api_tokens.rs` should take. Here it
// only has to hold often enough to prove proptest is wired.
proptest! {
    #[test]
    fn scope_sets_round_trip_through_their_stored_form(
        picks in prop::collection::vec(0usize..Scope::ALL.len(), 1..6)
    ) {
        let parsed: Vec<Scope> =
            picks.iter().filter_map(|i| Scope::parse(Scope::ALL[*i]).ok()).collect();
        let stored = join_scopes(&parsed);
        prop_assert_eq!(split_scopes(Some(stored.as_str())), parsed);
    }
}

proptest! {
    // Arbitrary text in the column must never panic and never yield an empty scope.
    // The port of the Python token-by-token fuzz test (AGENTS.md §11), at fixture scale.
    #[test]
    fn splitting_arbitrary_stored_text_never_panics(raw in ".{0,64}") {
        let scopes = split_scopes(Some(&raw));
        prop_assert!(!scopes.is_empty(), "split_scopes must never return empty");
        prop_assert!(scopes.iter().all(|s| !s.as_str().is_empty()));
    }
}

proptest! {
    #[test]
    fn absent_scopes_resolve_to_the_default(_ in any::<u8>()) {
        prop_assert_eq!(split_scopes(None).len(), 1);
        let blank = split_scopes(Some("   "));
        prop_assert_eq!(blank[0].as_str(), "chat");
    }
}

// ---------------------------------------------------------------- pretty_assertions

#[test]
fn a_scope_error_reads_usefully() {
    use pretty_assertions::assert_eq;
    // A rejected scope must name the offending input, because this text reaches a
    // route handler. If it ever degrades to "invalid", this assertion notices.
    let err = Scope::parse("not a scope!").expect_err("whitespace and punctuation are not scopes");
    assert_eq!(
        err.to_string(),
        "unknown token scope: not a scope!",
        "the error text is part of the contract"
    );
}

// ---------------------------------------------------------------- insta

/// A committed snapshot of the events a streaming response emits, in order.
///
/// AGENTS.md §11 asks for insta over SSE sequences. The real sequences belong to
/// the web crate's SSE tests; what is asserted here is the *shape* the eventual
/// snapshots will have, so the dependency and its unreviewed-change-is-a-diff
/// workflow are proven before anything depends on them.
#[test]
fn streaming_event_sequence_snapshot() {
    let events = [
        r#"{"type":"start","session":"s1"}"#,
        r#"{"type":"delta","text":"Hello"}"#,
        r#"{"type":"delta","text":", world"}"#,
        r#"{"type":"done","tokens":7}"#,
    ];
    let joined = events.join("\n");
    let snapshot = format!("{joined}\n-- events: {}", events.len());
    insta::assert_snapshot!(snapshot);
}

// ---------------------------------------------------------------- mockall

/// The trait exists to be mocked at the edges that cannot be driven for real.
/// `mockall::mock!` is the macro; the generated type below is the proof it expands
/// and that the expectations it installs are actually enforced.
pub mod edges {
    pub trait EdgeClock {
        fn now_millis(&self) -> u64;
    }
}

use mockall::mock;
// `mock!` prefixes the generated struct, so `EdgeClock` below yields `MockEdgeClock`.
mock! {
    EdgeClock {}
    impl edges::EdgeClock for EdgeClock {
        fn now_millis(&self) -> u64;
    }
}

#[test]
fn a_mock_clock_returns_what_it_was_told_to() {
    use edges::EdgeClock as _;
    let mut clock = MockEdgeClock::new();
    clock
        .expect_now_millis()
        .times(1)
        .returning(|| 1_700_000_000_000);
    assert_eq!(clock.now_millis(), 1_700_000_000_000);
}

// ---------------------------------------------------------------- tempfile

/// `tempfile` was already in use, but the `getrandom` feature makes generated
/// names unguessable — which matters because these tests write files that are
/// chmod 0600 and hold secret-shaped values.
#[test]
fn a_temp_dir_is_private_and_unique() {
    let a = tempfile::tempdir().expect("tempdir");
    let b = tempfile::tempdir().expect("tempdir");
    assert_ne!(a.path(), b.path(), "two temp dirs must not collide");

    let secret = b"enc:0123456789abcdef";
    atomic_write_bytes(&a.path().join("app_key"), secret).expect("write");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(a.path().join("app_key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "a secret-shaped file must not be world-readable"
        );
    }
    assert_eq!(std::fs::read(a.path().join("app_key")).unwrap(), secret);
}

// ---------------------------------------------------------------- criterion

/// criterion is a bench-only dependency (`harness = false`), so it is exercised by
/// `cargo bench`, not here. This asserts only that the benches that use it are
/// declared, which is what stops the `[[bench]]` entries being deleted alongside
/// the dependency.
#[test]
fn the_bench_targets_that_use_criterion_are_declared() {
    let manifest = include_str!("../Cargo.toml");
    for name in ["auth_hotpath", "layout"] {
        assert!(
            manifest.contains(&format!("name = \"{name}\"")),
            "benches/{name}.rs exists but has no [[bench]] entry, so cargo will not build it"
        );
    }
    assert!(
        manifest.contains("harness = false"),
        "criterion needs harness = false"
    );
}

// ---------------------------------------------------------------- a real property

/// A last sanity check that the fixture above is exercising the code and not a
/// stand-in: every profile, in both directions. A profile that parses but
/// stringifies differently would mislabel a token in the UI, so `as_str` is the
/// half that is easy to get wrong.
#[test]
fn the_token_profile_parser_round_trips() {
    for profile in TokenProfile::ALL {
        let parsed = TokenProfile::parse(profile.as_str()).expect("round trip");
        assert_eq!(parsed.as_str(), profile.as_str());
    }
    assert!(TokenProfile::parse("not-a-profile").is_err());
}
