//! T0210 — transcript search over the FTS5 index, and the `LIKE` fallback.
//!
//! The Python is `src/session_search.py` plus `_migrate_chat_messages_fts`
//! (`core/database.py:2178-2260`). Its own suite is
//! `tests/test_session_search.py`; every test here names the case it pins.
//!
//! Two things this file exists to prove:
//!
//! * FTS5 availability is a **runtime** property (`_has_fts_table`,
//!   `session_search.py:118-126`), not a compile-time one. The bundled SQLite has
//!   FTS5, so the `LIKE` branch would otherwise never run in CI — and an
//!   unexercised fallback is a broken fallback. [`SessionSearch::like_only`]
//!   forces that branch through the same assertions.
//! * The index is maintained by triggers on `chat_messages`, so a message
//!   inserted through the normal path is searchable without an explicit index
//!   write. T0220 covers what those triggers must *not* store.

use std::error::Error;

use odysseus_store::{
    Store,
    session_search::{FtsMode, SearchOptions, SessionSearch},
    sessions::{NewMessage, NewSession, SessionStore},
};

/// A store over a fresh temp database with the baseline applied.
///
/// The `TempDir` is returned so it outlives the `Store`: deleting it under an
/// open connection makes the assertions meaningless rather than failing.
fn store() -> Result<(tempfile::TempDir, Store), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("app.db"));
    store.migrate()?;
    Ok((dir, store))
}

const T0: f64 = 1_800_000_000.0;

async fn add_session(
    sessions: &SessionStore,
    id: &str,
    name: &str,
    owner: &str,
    archived: bool,
) -> Result<(), Box<dyn Error>> {
    sessions
        .create(NewSession {
            id: id.into(),
            name: name.into(),
            endpoint_url: "http://example.test".into(),
            model: "test-model".into(),
            owner: owner.into(),
            rag: false,
            now: T0,
        })
        .await?;
    if archived {
        sessions.set_archived(id, true, T0).await?;
    }
    Ok(())
}

async fn add_message(
    sessions: &SessionStore,
    session_id: &str,
    id: &str,
    role: &str,
    content: &str,
    offset: f64,
) -> Result<(), Box<dyn Error>> {
    sessions
        .add_message(session_id, NewMessage::new(id, role, content, T0 + offset))
        .await?;
    Ok(())
}

fn options() -> SearchOptions {
    SearchOptions {
        limit: 20,
        context_messages: 1,
        ..SearchOptions::default()
    }
}

// ---------------------------------------------------------------------------
// The FTS path
// ---------------------------------------------------------------------------

/// `test_session_search_uses_fts_and_returns_context`, ported.
#[tokio::test]
async fn fts_search_finds_the_message_and_its_context() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "Jazz planning", "alice", false).await?;
    add_message(
        &sessions,
        "s1",
        "m1",
        "user",
        "Before context about music",
        0.0,
    )
    .await?;
    add_message(
        &sessions,
        "s1",
        "m2",
        "assistant",
        "We talked about modal jazz theory",
        60.0,
    )
    .await?;
    add_message(
        &sessions,
        "s1",
        "m3",
        "user",
        "After context about tasks",
        120.0,
    )
    .await?;

    let search = SessionSearch::new(store);
    let hits = search.search("modal jazz", &options()).await?;

    assert_eq!(hits.len(), 1, "only m2 contains both terms");
    let hit = &hits[0];
    assert_eq!(hit.message_id, "m2");
    assert_eq!(hit.session_name, "Jazz planning");
    assert_eq!(hit.context_before.len(), 1);
    assert_eq!(hit.context_before[0].message_id, "m1");
    assert_eq!(hit.context_after.len(), 1);
    assert_eq!(hit.context_after[0].message_id, "m3");
    Ok(())
}

/// The session name must come back, and an empty one must read `Untitled`
/// (`session_search.py:181`).
#[tokio::test]
async fn a_hit_carries_its_session_name() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "Named chat", "alice", false).await?;
    add_message(&sessions, "s1", "m1", "user", "findable needle here", 0.0).await?;

    let hits = SessionSearch::new(store)
        .search("needle", &options())
        .await?;
    assert_eq!(hits[0].session_name, "Named chat");
    Ok(())
}

/// Terms are combined with AND, so a message missing one of the words must not match.
#[tokio::test]
async fn fts_terms_are_combined_with_and() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "s", "alice", false).await?;
    add_message(&sessions, "s1", "m1", "user", "alpha only", 0.0).await?;
    add_message(&sessions, "s1", "m2", "user", "alpha and beta", 60.0).await?;

    let hits = SessionSearch::new(store)
        .search("alpha beta", &options())
        .await?;
    assert_eq!(
        hits.iter()
            .map(|h| h.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["m2"]
    );
    Ok(())
}

/// Only `user` and `assistant` are searchable (`SEARCH_ROLES`).
#[tokio::test]
async fn system_and_tool_messages_are_not_searchable() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "s", "alice", false).await?;
    add_message(&sessions, "s1", "m1", "system", "hidden system prompt", 0.0).await?;
    add_message(&sessions, "s1", "m2", "tool", "hidden tool output", 60.0).await?;
    add_message(&sessions, "s1", "m3", "user", "visible needle", 120.0).await?;

    let hits = SessionSearch::new(store)
        .search("hidden", &options())
        .await?;
    assert!(hits.is_empty(), "system and tool roles must be excluded");
    Ok(())
}

/// Archived sessions are hidden unless asked for (`include_archived`).
#[tokio::test]
async fn archived_sessions_are_excluded_by_default() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "live", "Live", "alice", false).await?;
    add_session(&sessions, "arch", "Archived", "alice", true).await?;
    add_message(&sessions, "live", "m1", "user", "shared needle", 0.0).await?;
    add_message(&sessions, "arch", "m2", "user", "shared needle", 60.0).await?;

    let hidden = SessionSearch::new(store.clone())
        .search("needle", &options())
        .await?;
    assert_eq!(hidden.len(), 1);
    assert_eq!(hidden[0].session_id, "live");

    let shown = SessionSearch::new(store)
        .search(
            "needle",
            &SearchOptions {
                include_archived: true,
                ..options()
            },
        )
        .await?;
    assert_eq!(shown.len(), 2, "include_archived must widen the search");
    Ok(())
}

/// The owner filter is applied on the FTS path too.
#[tokio::test]
async fn the_owner_filter_restricts_results() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "mine", "Mine", "alice", false).await?;
    add_session(&sessions, "theirs", "Theirs", "bob", false).await?;
    add_message(&sessions, "mine", "m1", "user", "shared needle", 0.0).await?;
    add_message(&sessions, "theirs", "m2", "user", "shared needle", 60.0).await?;

    let hits = SessionSearch::new(store)
        .search(
            "needle",
            &SearchOptions {
                owner: Some("alice".to_string()),
                restrict_owner: true,
                ..options()
            },
        )
        .await?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, "mine");
    Ok(())
}

/// SFT trace batches are excluded by name (`EXCLUDED_NAME_PREFIX`).
#[tokio::test]
async fn sft_trace_batches_are_excluded() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "train", "SFT trace batch 42", "alice", false).await?;
    add_session(&sessions, "real", "Real chat", "alice", false).await?;
    add_message(&sessions, "train", "m1", "user", "shared needle", 0.0).await?;
    add_message(&sessions, "real", "m2", "user", "shared needle", 60.0).await?;

    let hits = SessionSearch::new(store)
        .search("needle", &options())
        .await?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, "real");
    Ok(())
}

/// `limit` is honoured.
#[tokio::test]
async fn the_limit_caps_the_result_count() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "s", "alice", false).await?;
    for i in 0..10 {
        add_message(
            &sessions,
            "s1",
            &format!("m{i}"),
            "user",
            "repeated needle",
            f64::from(i) * 60.0,
        )
        .await?;
    }

    let hits = SessionSearch::new(store)
        .search(
            "needle",
            &SearchOptions {
                limit: 3,
                ..options()
            },
        )
        .await?;
    assert_eq!(hits.len(), 3);
    Ok(())
}

/// A query with nothing usable in it must not error; it falls through to the
/// other path or returns empty.
#[tokio::test]
async fn a_query_of_only_punctuation_does_not_error() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "s", "alice", false).await?;
    add_message(&sessions, "s1", "m1", "user", "some content", 0.0).await?;

    for query in ["", "   ", "!!!", "^&*", "()"] {
        let hits = SessionSearch::new(store.clone())
            .search(query, &options())
            .await?;
        assert!(hits.is_empty(), "{query:?} must not match anything");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The LIKE fallback
// ---------------------------------------------------------------------------
//
// The same fixture as the tests above, re-run through `like_only`. A fallback
// that is never exercised is a fallback that is broken, and the bundled SQLite
// *has* FTS5, so without this the branch would only ever run on a machine that
// happens to lack it — that is, in production, at the worst possible moment.

/// The fallback returns the same hit the index does.
#[tokio::test]
async fn the_like_fallback_finds_the_same_message() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "Jazz planning", "alice", false).await?;
    add_message(
        &sessions,
        "s1",
        "m1",
        "user",
        "Before context about music",
        0.0,
    )
    .await?;
    add_message(
        &sessions,
        "s1",
        "m2",
        "assistant",
        "We talked about modal jazz theory",
        60.0,
    )
    .await?;
    add_message(
        &sessions,
        "s1",
        "m3",
        "user",
        "After context about tasks",
        120.0,
    )
    .await?;

    let search = SessionSearch::like_only(store);
    assert_eq!(search.mode(), FtsMode::LikeOnly);
    let hits = search.search("modal jazz", &options()).await?;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message_id, "m2");
    assert_eq!(hits[0].session_name, "Jazz planning");
    assert_eq!(hits[0].context_before[0].message_id, "m1");
    assert_eq!(hits[0].context_after[0].message_id, "m3");
    Ok(())
}

/// Both branches must agree, or which one runs becomes an observable
/// behaviour difference rather than an implementation detail.
#[tokio::test]
async fn both_branches_agree_on_the_result_set() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "Chat", "alice", false).await?;
    add_message(&sessions, "s1", "m1", "user", "first needle here", 0.0).await?;
    add_message(&sessions, "s1", "m2", "user", "second needle there", 60.0).await?;
    add_message(&sessions, "s1", "m3", "user", "unrelated", 120.0).await?;

    let indexed: Vec<String> = SessionSearch::new(store.clone())
        .search("needle", &options())
        .await?
        .into_iter()
        .map(|h| h.message_id)
        .collect();
    let fallback: Vec<String> = SessionSearch::like_only(store)
        .search("needle", &options())
        .await?
        .into_iter()
        .map(|h| h.message_id)
        .collect();

    assert_eq!(
        indexed, fallback,
        "the fallback must return the same set as the index"
    );
    assert_eq!(indexed.len(), 2);
    Ok(())
}

/// The filters must survive the fallback too — a fallback that silently drops
/// the owner filter would leak another user's transcript.
#[tokio::test]
async fn the_fallback_honours_the_owner_and_role_filters() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "mine", "Mine", "alice", false).await?;
    add_session(&sessions, "theirs", "Theirs", "bob", false).await?;
    add_session(&sessions, "arch", "Arch", "alice", true).await?;
    add_message(&sessions, "mine", "m1", "user", "shared needle", 0.0).await?;
    add_message(&sessions, "theirs", "m2", "user", "shared needle", 60.0).await?;
    add_message(&sessions, "arch", "m3", "user", "shared needle", 120.0).await?;
    add_message(&sessions, "mine", "m4", "system", "shared needle", 180.0).await?;

    let search = SessionSearch::like_only(store);
    let alice = search
        .search(
            "needle",
            &SearchOptions {
                owner: Some("alice".to_string()),
                restrict_owner: true,
                ..options()
            },
        )
        .await?;
    assert_eq!(
        alice
            .iter()
            .map(|h| h.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["m1"],
        "the fallback must apply owner, archived and role filters"
    );
    Ok(())
}

/// `LIKE` metacharacters in the query are literal, not wildcards. Without the
/// escaping a query of `%` returns the entire history.
#[tokio::test]
async fn like_wildcards_in_the_query_are_escaped() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "s", "alice", false).await?;
    add_message(&sessions, "s1", "m1", "user", "literal 100% coverage", 0.0).await?;
    add_message(&sessions, "s1", "m2", "user", "no wildcard here", 60.0).await?;

    let search = SessionSearch::like_only(store);

    let percent = search.search("%", &options()).await?;
    assert_eq!(
        percent.len(),
        1,
        "'%' must match the one message containing it"
    );
    assert_eq!(percent[0].message_id, "m1");

    let underscore = search.search("_", &options()).await?;
    assert!(
        underscore.is_empty(),
        "'_' must not act as a single-char wildcard"
    );

    let backslash = search.search("\\", &options()).await?;
    assert!(
        backslash.is_empty(),
        "'\\' must not escape the following byte"
    );
    Ok(())
}

/// The fallback produces a usable excerpt rather than the whole message.
#[tokio::test]
async fn the_fallback_produces_a_snippet() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "s", "alice", false).await?;
    let long = format!("{} NEEDLE {}", "padding ".repeat(40), "tail");
    add_message(&sessions, "s1", "m1", "user", &long, 0.0).await?;

    let hits = SessionSearch::like_only(store)
        .search("NEEDLE", &options())
        .await?;
    assert_eq!(hits.len(), 1);
    let snip = &hits[0].content_snippet;
    assert!(
        snip.contains("NEEDLE"),
        "the snippet must contain the match"
    );
    assert!(
        snip.len() < long.len(),
        "the excerpt must be shorter than the message: got {} of {}",
        snip.len(),
        long.len()
    );
    Ok(())
}

/// A query is attacker-controlled input, so FTS5 operator syntax must not
/// reach `MATCH` as syntax.
#[tokio::test]
async fn fts_operator_syntax_cannot_reach_the_match_expression() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "s", "alice", false).await?;
    add_message(&sessions, "s1", "m1", "user", "ordinary content", 0.0).await?;

    for hostile in [
        "NEAR(",
        "\"unterminated",
        "a AND OR NOT",
        "*",
        "^",
        "column:value",
        "()",
    ] {
        let hits = SessionSearch::new(store.clone())
            .search(hostile, &options())
            .await?;
        assert!(hits.is_empty(), "{hostile:?} must not match or error");
    }
    Ok(())
}

/// The Python's entry guard (`session_search.py:315-317`). Without it the
/// fallback's pattern is `%%` and an empty search box returns the whole
/// history.
#[tokio::test]
async fn an_empty_or_blank_query_finds_nothing() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "s", "alice", false).await?;
    add_message(&sessions, "s1", "m1", "user", "content here", 0.0).await?;

    for query in ["", " ", "\t", "   \n  "] {
        let indexed = SessionSearch::new(store.clone())
            .search(query, &options())
            .await?;
        assert!(indexed.is_empty(), "fts: {query:?} must find nothing");
        let fallback = SessionSearch::like_only(store.clone())
            .search(query, &options())
            .await?;
        assert!(
            fallback.is_empty(),
            "fallback: {query:?} must find nothing, not every row"
        );
    }
    Ok(())
}

/// `limit` and `context_messages` are clamped, so a caller cannot ask for an
/// unbounded fetch (`session_search.py:319-320`).
#[tokio::test]
async fn limit_and_context_are_clamped_to_the_python_bounds() -> Result<(), Box<dyn Error>> {
    use odysseus_store::session_search::{MAX_CONTEXT_MESSAGES, MAX_LIMIT, MIN_LIMIT};

    let absurd = SearchOptions {
        limit: usize::MAX,
        context_messages: 9_999,
        ..SearchOptions::default()
    }
    .clamped();
    assert_eq!(absurd.limit, MAX_LIMIT);
    assert_eq!(absurd.context_messages, MAX_CONTEXT_MESSAGES);

    let zero = SearchOptions {
        limit: 0,
        context_messages: 0,
        ..SearchOptions::default()
    }
    .clamped();
    assert_eq!(zero.limit, MIN_LIMIT, "limit is floored at 1");
    assert_eq!(zero.context_messages, 0);

    // And the clamp actually holds against the database, not just the struct.
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "s", "alice", false).await?;
    for i in 0..4 {
        add_message(
            &sessions,
            "s1",
            &format!("m{i}"),
            "user",
            "needle",
            f64::from(i) * 60.0,
        )
        .await?;
    }
    let hits = SessionSearch::new(store).search("needle", &absurd).await?;
    assert_eq!(hits.len(), 4);
    assert!(
        hits[0].context_before.len() <= MAX_CONTEXT_MESSAGES,
        "context must be clamped"
    );
    Ok(())
}
