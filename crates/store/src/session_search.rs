//! Full-text search over stored session transcripts.
//!
//! Ported from `src/session_search.py` (369 lines) and the FTS5 half of
//! `_migrate_chat_messages_fts` (`core/database.py:2178-2260`).
//!
//! # FTS5 is a runtime property, not a build flag
//!
//! The Python probes FTS5 by trying to create a throwaway table
//! (`database.py:2204-2207`) and falls back to `LIKE` when the probe fails. The
//! bundled `rusqlite` here *does* have FTS5, which is exactly why the fallback
//! needs testing rather than assuming: a fallback nobody exercises is a
//! fallback that is broken, and the moment it is needed is the moment it is
//! least welcome.
//!
//! So availability is a runtime check, never a `cfg`, and [`FtsMode::LikeOnly`]
//! forces the absent path so both branches are covered by the same suite.
//!
//! # Input is attacker-controlled
//!
//! A search box is a text field a user (or a script) can fill with anything.
//! Every value reaching SQL is a bound parameter; the only interpolation is
//! between fixed, non-user fragments chosen by [`FtsMode`]. The FTS5 query text
//! is additionally sanitised into bare words and balanced quoted phrases, both
//! because a raw `MATCH` on user text raises `OperationalError` and because the
//! operator syntax it would otherwise accept has no meaning here.

use {
    rusqlite::{Connection, ToSql, params},
    serde::Serialize,
};

use crate::{Store, StoreError, blocking::with_conn};

/// Roles whose messages are searchable.
///
/// The Python's `SEARCH_ROLES` (`session_search.py:17`): tool chatter and system
/// prompts are excluded from transcript search.
pub const SEARCH_ROLES: [&str; 2] = ["user", "assistant"];

/// Sessions whose names start with this are excluded from search.
///
/// Carried over verbatim from `session_search.py:263` and `:198`. These are
/// SFT (fine-tuning) trace batches, which are bulk data, not conversation, and
/// searching them drowns out real transcripts.
pub const EXCLUDED_NAME_PREFIX: &str = "SFT trace batch";

/// Upper bound on terms in a generated FTS5 query.
///
/// The Python has no cap. FTS5 accepts arbitrarily long expressions, so a
/// megabyte-long search box becomes a megabyte-long query. The cap is generous
/// enough that no real query reaches it, and a query that does is better served
/// by the `LIKE` path than by a pathological `MATCH`.
const MAX_FTS_TERMS: usize = 32;

/// How the search should treat the FTS5 index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FtsMode {
    /// Probe for the index and use it when present, otherwise `LIKE`.
    #[default]
    Auto,
    /// Skip the index entirely and always use `LIKE`.
    ///
    /// Exists so the fallback is exercised by the test suite rather than only
    /// reachable on a machine whose SQLite lacks FTS5.
    LikeOnly,
}

/// What to search and how much of it to return.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchOptions {
    /// Restrict to sessions owned by this user.
    ///
    /// `None` with [`SearchOptions::restrict_owner`] set means *unowned*
    /// sessions, matching `_owner_filter`'s `owner is None` branch
    /// (`session_search.py:129`).
    pub owner: Option<String>,
    /// Whether to apply the owner filter at all. `false` searches every owner.
    pub restrict_owner: bool,
    /// Also return unowned sessions alongside `owner`'s, for deployments that
    /// predate ownership.
    pub include_legacy_owner: bool,
    /// Include archived sessions. Off by default, as in the Python.
    pub include_archived: bool,
    /// Maximum hits.
    pub limit: usize,
    /// Neighbouring messages to fetch on each side of a hit.
    pub context_messages: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            owner: None,
            restrict_owner: false,
            include_legacy_owner: false,
            include_archived: false,
            limit: 20,
            context_messages: 1,
        }
    }
}

/// Smallest `limit` the Python accepts (`max(1, …)`).
pub const MIN_LIMIT: usize = 1;
/// Largest `limit` the Python accepts (`min(…, 100)`).
pub const MAX_LIMIT: usize = 100;
/// Largest `context_messages` the Python accepts (`min(…, 3)`).
pub const MAX_CONTEXT_MESSAGES: usize = 3;

impl SearchOptions {
    /// The options with `limit` and `context_messages` inside their bounds.
    ///
    /// A port of `limit = max(1, min(int(limit or 20), 100))` and
    /// `context_messages = max(0, min(int(context_messages or 0), 3))`
    /// (`session_search.py:319-320`).
    #[must_use]
    pub fn clamped(&self) -> Self {
        Self {
            limit: self.limit.clamp(MIN_LIMIT, MAX_LIMIT),
            context_messages: self.context_messages.min(MAX_CONTEXT_MESSAGES),
            ..self.clone()
        }
    }
}

/// One neighbouring message in a hit's context.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextMessage {
    /// Primary key.
    pub message_id: String,
    /// `user` or `assistant`.
    pub role: String,
    /// Raw stored body.
    pub content: String,
    /// Unix timestamp.
    pub timestamp: Option<f64>,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionSearchResult {
    /// Primary key of the matching message.
    pub message_id: String,
    /// Session the message belongs to.
    pub session_id: String,
    /// Session display name, or `Untitled`.
    pub session_name: String,
    /// Role of the matching message.
    pub role: String,
    /// Full body of the matching message.
    pub content: String,
    /// A short excerpt around the match.
    pub content_snippet: String,
    /// Unix timestamp of the matching message.
    pub timestamp: Option<f64>,
    /// Up to `context_messages` searchable messages before it, oldest first.
    pub context_before: Vec<ContextMessage>,
    /// Up to `context_messages` searchable messages after it, oldest first.
    pub context_after: Vec<ContextMessage>,
}

/// Transcript search over one store.
#[derive(Debug, Clone)]
pub struct SessionSearch {
    store: Store,
    mode: FtsMode,
}

impl SessionSearch {
    /// Search `store`, using the FTS5 index when it is available.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self {
            store,
            mode: FtsMode::Auto,
        }
    }

    /// Search `store`, always via `LIKE`.
    ///
    /// The same results the Python produces on a SQLite without FTS5, and the
    /// way the test suite drives the fallback branch.
    #[must_use]
    pub fn like_only(store: Store) -> Self {
        Self {
            store,
            mode: FtsMode::LikeOnly,
        }
    }

    /// Which branch this instance will take.
    #[must_use]
    pub fn mode(&self) -> FtsMode {
        self.mode
    }

    /// Finds messages matching `query`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the database cannot be opened or a query fails
    /// for a reason neither branch can handle.
    pub async fn search(
        &self,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<SessionSearchResult>, StoreError> {
        let store = self.store.clone();
        let mode = self.mode;
        let query = query.to_string();
        let options = options.clone();
        with_conn(store, move |conn| {
            search_blocking(conn, &query, &options, mode)
        })
        .await
    }
}

/// Runs a search on an open connection.
///
/// # Errors
///
/// Returns [`StoreError`] on a query failure in both branches.
pub fn search_blocking(
    conn: &Connection,
    query: &str,
    options: &SearchOptions,
    mode: FtsMode,
) -> Result<Vec<SessionSearchResult>, StoreError> {
    // The Python's entry guard (`session_search.py:315-317`): a query that is
    // empty once trimmed finds nothing. Without this the `LIKE` pattern would
    // be `%%` and match every message in the history.
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }

    // The Python's clamps (`session_search.py:319-320`). Both bound the work a
    // single request can do, which is why they belong here rather than in the
    // web layer: a caller that forgets them must not be able to ask for a
    // million rows of context.
    let clamped = options.clamped();

    if mode == FtsMode::Auto && fts_available(conn) {
        // Any FTS error falls through to LIKE, as in the Python's
        // `except Exception: return None` (`session_search.py:281-283`).
        //
        // `None` also means "no hits", not merely "failed" — the Python returns
        // `None` for an empty result set too (`session_search.py:285-286`).
        let Some(fts_hits) = search_fts(conn, query, &clamped)? else {
            return search_like(conn, query, &clamped);
        };

        // The Python *merges* the two result sets rather than preferring one
        // (`session_search.py:326-356`): FTS hits first, then LIKE hits, with
        // the first occurrence of a `message_id` winning, and the list cut at
        // `limit`.
        //
        // This matters because the two searches do not find the same rows. FTS
        // matches whole *words*; `LIKE` matches substrings. "modal jazz" finds
        // "modal jazz theory" through the index but also "jazzy" through LIKE,
        // and a message containing "jazzer" is reachable only by LIKE. The
        // merged list is strictly a superset of either alone.
        let like_hits = search_like(conn, query, &clamped)?;

        let mut merged: Vec<SessionSearchResult> = Vec::with_capacity(clamped.limit);
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for result in fts_hits.iter().chain(like_hits.iter()) {
            if !seen.insert(result.message_id.as_str()) {
                continue;
            }
            merged.push(result.clone());
            if merged.len() >= clamped.limit {
                break;
            }
        }
        return Ok(merged);
    }
    search_like(conn, query, &clamped)
}

/// Whether the `chat_messages_fts` index exists.
///
/// The Python's `_has_fts_table` (`session_search.py:118-126`): a `sqlite_master`
/// lookup, not a compile-time constant, because a table created by a migration
/// on one machine is absent on another.
fn fts_available(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'chat_messages_fts' LIMIT 1",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

/// A row shared by both search paths: the message plus its session's name.
struct Hit {
    message_id: String,
    session_id: String,
    session_name: String,
    role: String,
    content: String,
    timestamp: Option<f64>,
}

/// The filter fragments shared by both paths.
///
/// Every fragment is a fixed string chosen by a `bool` or an `Option`; no part of
/// a fragment is ever built from the query, and no query value is ever
/// interpolated. The user's text only ever reaches SQLite as a bound parameter.
struct Filters {
    archived: &'static str,
    owner: &'static str,
    /// Present only when the fragment actually references `:owner`.
    owner_value: Option<String>,
    limit: i64,
    excluded: String,
}

impl Filters {
    fn new(options: &SearchOptions) -> Self {
        let archived = if options.include_archived {
            ""
        } else {
            "AND s.archived = 0"
        };
        let (owner, owner_value) = if options.restrict_owner {
            match options.owner.as_deref() {
                None => ("AND s.owner IS NULL", None),
                Some(owner) if options.include_legacy_owner => (
                    "AND (s.owner = :owner OR s.owner IS NULL)",
                    Some(owner.to_string()),
                ),
                Some(owner) => ("AND s.owner = :owner", Some(owner.to_string())),
            }
        } else {
            ("", None)
        };
        Self {
            archived,
            owner,
            owner_value,
            limit: i64::try_from(options.limit).unwrap_or(i64::MAX),
            excluded: format!("{EXCLUDED_NAME_PREFIX}%"),
        }
    }

    /// The named parameters this filter set actually needs.
    ///
    /// Conditional, because SQLite rejects a bound name the statement never
    /// mentions — so `:owner` is supplied only when a fragment references it.
    fn bind<'f>(&'f self, extra: Vec<(&'f str, &'f dyn ToSql)>) -> Vec<(&'f str, &'f dyn ToSql)> {
        let mut params = extra;
        params.push((":excluded", &self.excluded as &dyn ToSql));
        params.push((":limit", &self.limit as &dyn ToSql));
        if let Some(owner) = &self.owner_value {
            params.push((":owner", owner as &dyn ToSql));
        }
        params
    }
}

/// Reads a hit row in the column order both queries share.
fn hit_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Hit, String)> {
    Ok((
        Hit {
            message_id: row.get(0)?,
            session_id: row.get(1)?,
            session_name: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            role: row.get(3)?,
            content: row.get(4)?,
            timestamp: row.get(5)?,
        },
        row.get::<_, Option<String>>(6)?.unwrap_or_default(),
    ))
}

/// The `LIKE` fallback, used when the index is absent or unusable.
///
/// A direct port of `_search_like` (`session_search.py:178-202`).
///
/// # Errors
///
/// Returns [`StoreError`] on a query failure.
fn search_like(
    conn: &Connection,
    query: &str,
    options: &SearchOptions,
) -> Result<Vec<SessionSearchResult>, StoreError> {
    let filters = Filters::new(options);
    let sql = format!(
        "SELECT m.id, m.session_id, s.name, m.role, m.content, m.timestamp, NULL AS snip
           FROM chat_messages m
           JOIN sessions s ON s.id = m.session_id
          WHERE m.content LIKE :pattern ESCAPE '\\'
            {archived}
            {owner}
            AND s.name NOT LIKE :excluded
            AND m.role IN ('user', 'assistant')
          ORDER BY m.timestamp DESC
          LIMIT :limit",
        archived = filters.archived,
        owner = filters.owner,
    );

    let mut stmt = conn.prepare(&sql)?;
    // The pattern is assembled here and bound; the SQL carries only the clause.
    let pattern = format!("%{}%", escape_like(query));
    let params = filters.bind(vec![(":pattern", &pattern as &dyn ToSql)]);
    let rows = stmt.query_map(params.as_slice(), hit_from_row)?;

    let mut out = Vec::new();
    for row in rows {
        let (hit, _) = row?;
        let excerpt = snippet(&hit.content, query);
        out.push(finish_hit(conn, hit, &excerpt, options)?);
    }
    Ok(out)
}

/// The FTS5 path. `Ok(None)` means "fall back to `LIKE`".
///
/// A direct port of `_search_fts` (`session_search.py:225-295`), including its
/// failure semantics: a query the sanitiser could not make sense of, or one
/// SQLite rejects, yields `None` rather than an error, because a search box must
/// never produce an error page.
///
/// # Errors
///
/// Returns [`StoreError`] only for failures the fallback cannot paper over.
fn search_fts(
    conn: &Connection,
    query: &str,
    options: &SearchOptions,
) -> Result<Option<Vec<SessionSearchResult>>, StoreError> {
    let Some(fts_query) = sanitize_fts_query(query) else {
        return Ok(None);
    };
    let filters = Filters::new(options);
    let sql = format!(
        "SELECT m.id, m.session_id, s.name, m.role, m.content, m.timestamp,
                snippet(chat_messages_fts, 0, '', '', '...', 24) AS snip
           FROM chat_messages_fts
           JOIN chat_messages m ON m.id = chat_messages_fts.message_id
           JOIN sessions s ON s.id = m.session_id
          WHERE chat_messages_fts MATCH :fts_query
            {archived}
            {owner}
            AND s.name NOT LIKE :excluded
            AND m.role IN ('user', 'assistant')
          ORDER BY bm25(chat_messages_fts), m.timestamp DESC
          LIMIT :limit",
        archived = filters.archived,
        owner = filters.owner,
    );

    // A statement SQLite will not prepare is the Python's `except` case
    // (`session_search.py:281-283`): fall back rather than error.
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Ok(None);
    };
    let params = filters.bind(vec![(":fts_query", &fts_query as &dyn ToSql)]);
    let rows = stmt.query_map(params.as_slice(), hit_from_row);
    // `OperationalError` from a `MATCH` the sanitiser mis-scoped.
    let Ok(rows) = rows else {
        return Ok(None);
    };

    let mut out = Vec::new();
    for row in rows {
        let Ok((hit, snip)) = row else {
            return Ok(None);
        };
        // The Python reuses the hit's snippet, falling back to its own when the
        // column is empty (`session_search.py:181`).
        let excerpt = if snip.is_empty() {
            snippet(&hit.content, query)
        } else {
            snip
        };
        out.push(finish_hit(conn, hit, &excerpt, options)?);
    }
    // The Python returns `None` for an empty hit set, not an empty list
    // (`session_search.py:285-286`). The caller then falls back to `LIKE`,
    // which is what makes a substring match reachable when the index has no
    // word-boundary hit. Returning `Some(vec![])` here would silently drop
    // every LIKE-only match.
    if out.is_empty() {
        return Ok(None);
    }
    Ok(Some(out))
}

/// Assembles a result, fetching the surrounding context.
fn finish_hit(
    conn: &Connection,
    hit: Hit,
    excerpt: &str,
    options: &SearchOptions,
) -> Result<SessionSearchResult, StoreError> {
    let (context_before, context_after) = context_for_message(
        conn,
        &hit.session_id,
        hit.timestamp,
        options.context_messages,
    )?;
    Ok(SessionSearchResult {
        session_name: if hit.session_name.is_empty() {
            "Untitled".to_string()
        } else {
            hit.session_name
        },
        message_id: hit.message_id,
        session_id: hit.session_id,
        role: hit.role,
        content: hit.content,
        content_snippet: excerpt.to_string(),
        timestamp: hit.timestamp,
        context_before,
        context_after,
    })
}

/// Fetches the searchable messages either side of `timestamp`.
///
/// The Python runs two queries per hit (`session_search.py:138-158`); this runs
/// one, splitting the rows on either side of the timestamp. Same result, half
/// the round trips, and the ordering the Python asks for is preserved: `before`
/// comes back oldest-first because the Python reverses its descending fetch.
fn context_for_message(
    conn: &Connection,
    session_id: &str,
    timestamp: Option<f64>,
    count: usize,
) -> Result<(Vec<ContextMessage>, Vec<ContextMessage>), StoreError> {
    if count == 0 {
        return Ok((Vec::new(), Vec::new()));
    }
    // A message with no timestamp cannot be ordered against its neighbours.
    let Some(at) = timestamp else {
        return Ok((Vec::new(), Vec::new()));
    };

    let mut stmt = conn.prepare(
        "SELECT id, role, content, timestamp
           FROM chat_messages
          WHERE session_id = ?1
            AND role IN ('user', 'assistant')
            AND (timestamp < ?2 OR timestamp > ?2)
          ORDER BY timestamp ASC",
    )?;
    let rows = stmt.query_map(params![session_id, at], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<f64>>(3)?,
        ))
    })?;

    let limit = usize::try_from(i64::try_from(count).unwrap_or(i64::MAX)).unwrap_or(usize::MAX);
    let mut before: Vec<ContextMessage> = Vec::new();
    let mut after: Vec<ContextMessage> = Vec::new();
    for row in rows {
        let (id, role, content, ts) = row?;
        // A NULL timestamp cannot be placed on either side, so it is dropped
        // rather than guessed at.
        let Some(ts) = ts else { continue };
        let msg = ContextMessage {
            message_id: id,
            role,
            content,
            timestamp: Some(ts),
        };
        if ts < at {
            if before.len() < limit {
                before.push(msg);
            }
        } else if after.len() < limit {
            after.push(msg);
        }
    }
    // `before` was collected ascending but the Python wants the *nearest*
    // neighbours first in its own fetch and then reverses to chronological.
    before.reverse();
    Ok((before, after))
}

/// Escapes the `LIKE` metacharacters `%`, `_` and the escape character itself.
///
/// The Python's `_escape_like` (`session_search.py:57-58`), and the SQL uses
/// `ESCAPE '\'` so the backslashes mean something. Without this a query of
/// `%` would match every message.
fn escape_like(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Turns free text into a conservative FTS5 `MATCH` expression.
///
/// A port of `_sanitize_fts_query` (`session_search.py:76-106`), including its
/// scope. The Python uses the regex
/// `"([^"]+)"|[\w][\w._-]*`, so two things are extracted in order: a
/// double-quoted phrase, or a word starting with a word character and
/// continuing with word characters, dots, underscores and hyphens.
///
/// This is hand-rolled rather than a `regex` dependency: the grammar is six
/// characters wide, and adding a dependency to the store crate for it would be
/// a poor trade (AGENTS.md §12.3).
///
/// Returns `None` when nothing usable is left, which the caller turns into a
/// `LIKE` search — the Python's `if not fts_query: return None`.
fn sanitize_fts_query(query: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut chars = query.chars().peekable();

    while let Some(&ch) = chars.peek() {
        if ch == '"' {
            chars.next();
            let mut phrase = String::new();
            let mut closed = false;
            for c in chars.by_ref() {
                if c == '"' {
                    closed = true;
                    break;
                }
                phrase.push(c);
            }
            // An unterminated quote consumes the rest of the input and is
            // dropped, exactly as the Python's `[^"]+` alternative not matching.
            if closed && !phrase.trim().is_empty() {
                push_fts_part(&mut parts, phrase.trim());
            }
            continue;
        }

        if is_word_char(ch) {
            let mut token = String::new();
            while let Some(&c) = chars.peek() {
                if is_word_char(c) || matches!(c, '.' | '_' | '-') {
                    token.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            push_fts_part(&mut parts, token.trim_matches(['.', '_', '-']));
            continue;
        }

        chars.next();
    }

    if parts.is_empty() {
        return None;
    }
    Some(parts.join(" "))
}

/// Adds one term, quoting it when it contains FTS operator punctuation.
///
/// FTS5 treats `.`, `-` and `_` inside an unquoted term as syntax in some
/// contexts, so the Python quotes those (`session_search.py:103-104`). An inner
/// `"` is doubled, which FTS5 understands as an escaped quote.
fn push_fts_part(parts: &mut Vec<String>, token: &str) {
    if token.is_empty() {
        return;
    }
    if parts.len() >= MAX_FTS_TERMS {
        return;
    }
    if token.contains(['.', '_', '-']) {
        parts.push(format!("\"{}\"", token.replace('"', "\"\"")));
    } else {
        parts.push(token.to_string());
    }
}

/// `\w` in the Python's regex: alphanumeric, or underscore.
///
/// The Python passes `re.UNICODE`, so this covers non-ASCII letters too.
fn is_word_char(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

/// A short excerpt around the first case-insensitive match of `query`.
///
/// The Python's `_snippet` (`session_search.py:62-73`), used when the FTS5
/// `snippet()` column is empty and on the `LIKE` path entirely.
fn snippet(content: &str, query: &str) -> String {
    snippet_with_radius(content, query, 60)
}

/// As `snippet`, with the Python's default radius of 60.
fn snippet_with_radius(content: &str, query: &str, radius: usize) -> String {
    let haystack = content.to_lowercase();
    let needle = query.to_lowercase();

    if needle.is_empty() {
        return take(content, 0, radius * 2);
    }
    let Some(idx) = haystack.find(&needle) else {
        return take(content, 0, radius * 2);
    };

    // Byte offsets from `find` are not char boundaries; walk back to one.
    let start = floor_boundary(content, idx.saturating_sub(radius));
    let end_finder = (idx + needle.len())
        .saturating_add(radius)
        .min(content.len());
    let end = ceil_boundary(content, end_finder);

    let mut out = String::new();
    if start > 0 {
        out.push_str("...");
    }
    out.push_str(&content[start..end]);
    if end < content.len() {
        out.push_str("...");
    }
    out
}

/// The `radius * 2` prefix the Python returns when there is no match.
fn take(content: &str, _start: usize, len: usize) -> String {
    let end = floor_boundary(content, len.min(content.len()));
    content[..end].to_string()
}

/// Walks `idx` back to the nearest UTF-8 character boundary.
fn floor_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Walks `idx` forward to the nearest UTF-8 character boundary.
fn ceil_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}
