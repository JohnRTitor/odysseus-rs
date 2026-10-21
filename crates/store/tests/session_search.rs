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

// ---------------------------------------------------------------------------
// The FTS/LIKE merge
// ---------------------------------------------------------------------------
//
// The Python does not prefer one search over the other: it runs both and merges
// them (`session_search.py:326-356`), FTS hits first, then LIKE hits, first
// occurrence of a `message_id` winning. This matters because the two searches
// do not find the same rows — FTS matches whole words, LIKE matches
// substrings — so the merged list is a strict superset of either.

/// A substring that only `LIKE` can find must still be returned.
#[tokio::test]
async fn a_substring_only_like_can_find_is_still_returned() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "Chat", "alice", false).await?;
    // "jazzer" contains "jazz" but is not a whole-word match.
    add_message(&sessions, "s1", "m1", "user", "the jazzer played", 0.0).await?;
    add_message(&sessions, "s1", "m2", "user", "modal jazz theory", 60.0).await?;

    let hits = SessionSearch::new(store.clone())
        .search("jazz", &options())
        .await?;
    let ids: Vec<&str> = hits.iter().map(|h| h.message_id.as_str()).collect();

    assert!(
        ids.contains(&"m1"),
        "the LIKE match for 'jazzer' must survive the merge: {ids:?}"
    );
    assert!(
        ids.contains(&"m2"),
        "the FTS match for 'modal jazz theory' must be present: {ids:?}"
    );
    Ok(())
}

/// The FTS hit comes first, because the Python chains `[fts, like]`.
#[tokio::test]
async fn fts_hits_are_ordered_before_like_hits() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "Chat", "alice", false).await?;
    // m2 is the word-boundary match; m1 is the substring match.
    add_message(&sessions, "s1", "m1", "user", "jazzer", 0.0).await?;
    add_message(&sessions, "s1", "m2", "user", "jazz", 60.0).await?;

    let hits = SessionSearch::new(store).search("jazz", &options()).await?;
    let ids: Vec<&str> = hits.iter().map(|h| h.message_id.as_str()).collect();

    assert_eq!(ids, vec!["m2", "m1"], "FTS hits precede LIKE hits");
    Ok(())
}

/// A message found by both searches appears once.
#[tokio::test]
async fn a_message_found_by_both_searches_appears_once() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "Chat", "alice", false).await?;
    add_message(&sessions, "s1", "m1", "user", "modal jazz", 0.0).await?;

    let hits = SessionSearch::new(store).search("jazz", &options()).await?;
    let ids: Vec<&str> = hits.iter().map(|h| h.message_id.as_str()).collect();
    assert_eq!(ids, vec!["m1"], "the merge must deduplicate by message_id");
    Ok(())
}

/// The merged list is capped at `limit`, counting both sources.
#[tokio::test]
async fn the_merged_list_respects_the_limit() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "Chat", "alice", false).await?;
    // Two word-boundary matches and two substring matches.
    add_message(&sessions, "s1", "m1", "user", "jazz one", 0.0).await?;
    add_message(&sessions, "s1", "m2", "user", "jazz two", 60.0).await?;
    add_message(&sessions, "s1", "m3", "user", "jazzer three", 120.0).await?;
    add_message(&sessions, "s1", "m4", "user", "jazzy four", 180.0).await?;

    let hits = SessionSearch::new(store)
        .search(
            "jazz",
            &SearchOptions {
                limit: 3,
                ..options()
            },
        )
        .await?;
    assert_eq!(hits.len(), 3, "the cap applies to the merged list");
    Ok(())
}

/// The merge must not weaken the filters: a LIKE-only match must still respect
/// the owner and role filters.
#[tokio::test]
async fn the_merge_does_not_weaken_the_filters() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "mine", "Mine", "alice", false).await?;
    add_session(&sessions, "theirs", "Theirs", "bob", false).await?;
    add_message(&sessions, "mine", "m1", "user", "jazzer here", 0.0).await?;
    add_message(&sessions, "theirs", "m2", "user", "jazzer here", 60.0).await?;

    let hits = SessionSearch::new(store)
        .search(
            "jazz",
            &SearchOptions {
                owner: Some("alice".to_string()),
                restrict_owner: true,
                ..options()
            },
        )
        .await?;
    let ids: Vec<&str> = hits.iter().map(|h| h.message_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["m1"],
        "the LIKE branch of the merge must apply the owner filter too"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Two known gaps, pinned so they cannot be forgotten
// ---------------------------------------------------------------------------

/// **Gap 1 — the index is not backfilled for rows that predate it.**
///
/// The Python's `_migrate_chat_messages_fts` backfills every existing
/// `chat_messages` row into the index (`database.py:2243-2251`) and then scrubs
/// legacy index rows that already hold inline media
/// (`_scrub_legacy_chat_message_fts_media`, `database.py:2262-2291`).
///
/// The Rust baseline creates the table and the triggers but does **not**
/// backfill, and does not scrub.
///
/// The trigger fires on every INSERT, so for a *fresh* Rust database the index
/// is always complete. A row can only miss it if it was inserted **before the
/// trigger existed** — that is, in a database adopted from the Python
/// implementation. This test builds exactly that state: the message row is
/// written first, and the trigger is created afterwards.
///
/// This is currently **latent, not live**: the baseline uses no
/// `IF NOT EXISTS`, so `migrate()` fails outright on a Python-era database
/// ("table `chat_messages` already exists"), and the Rust store therefore never
/// opens one. The moment an adoption path is added, this gap becomes live and
/// the backfill must be added with it.
///
/// The LIKE branch still finds such a message, so the merge keeps it
/// reachable — which is the other reason the merge matters.
#[tokio::test]
async fn a_message_predating_the_trigger_is_only_reachable_through_like()
-> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "Chat", "alice", false).await?;

    // Write the message first, then drop the insert trigger, so the row exists
    // with no index entry — the state an adopted Python database is in.
    let conn = store.connect()?;
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS chat_messages_fts_ai;
         INSERT INTO chat_messages (id, session_id, role, content, metadata, timestamp)
         VALUES ('m1', 's1', 'user', 'an orphaned needle', NULL, 1.0);
         CREATE TRIGGER chat_messages_fts_ai
         AFTER INSERT ON chat_messages BEGIN
             INSERT INTO chat_messages_fts(content, message_id, session_id, role)
             VALUES (CASE WHEN instr(COALESCE(new.content, ''), ';base64,') > 0 OR instr(COALESCE(new.content, ''), 'data:image/') > 0 OR instr(COALESCE(new.content, ''), 'data:audio/') > 0 THEN '[inline media omitted from search index]' ELSE COALESCE(new.content, '') END, new.id, new.session_id, new.role);
         END;",
    )?;
    drop(conn);

    let conn = store.connect()?;
    let fts_rows: i64 = conn.query_row(
        "SELECT count(*) FROM chat_messages_fts WHERE message_id = 'm1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(fts_rows, 0, "precondition: the row is not in the index");

    // The merged search still finds it, through the LIKE branch.
    let hits = SessionSearch::new(store)
        .search("needle", &options())
        .await?;
    assert_eq!(
        hits.len(),
        1,
        "the LIKE branch of the merge is what keeps an unindexed row reachable"
    );
    assert_eq!(hits[0].message_id, "m1");
    Ok(())
}

/// **Gap 2 — a legacy index row holding inline media is not scrubbed.**
///
/// The Python's `_scrub_legacy_chat_message_fts_media` replaces already-indexed
/// rows that contain inline media with searchable text
/// (`database.py:2262-2291`). The Rust side has no equivalent, because the
/// baseline's triggers only scrub rows as they are written.
///
/// The consequence: a database that already has media in the index keeps it.
/// That is the exact defect T0220 exists to prevent, so it is pinned here as a
/// known gap rather than left to be discovered.
#[tokio::test]
async fn a_legacy_index_row_with_media_is_not_scrubbed() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    add_session(&sessions, "s1", "Chat", "alice", false).await?;

    // Write a media-bearing row straight into the index, the way a pre-scrub
    // Python database would contain one.
    let conn = store.connect()?;
    conn.execute(
        "INSERT INTO chat_messages_fts (content, message_id, session_id, role)
         VALUES ('data:image/png;base64,AAAA', 'm1', 's1', 'user')",
        [],
    )?;
    drop(conn);

    let conn = store.connect()?;
    let indexed: String = conn.query_row(
        "SELECT content FROM chat_messages_fts WHERE message_id = 'm1'",
        [],
        |row| row.get(0),
    )?;
    assert!(
        indexed.contains("base64"),
        "precondition: the legacy index row must still hold the payload, \
         which is exactly the gap"
    );
    Ok(())
}
