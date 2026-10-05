//! T0220 — the FTS5 index must never contain inline base64 media.
//!
//! The Python sibling is `test_sqlite_base64_media`; the scrubbing expression
//! lives in `_migrate_chat_messages_fts` (`core/database.py:2193-2206`).
//!
//! # Why this is a security test, not a nicety
//!
//! A `data:` URI is the *message content*. One screenshot pasted into a chat is
//! megabytes of base64. The `chat_messages_fts` triggers replace such a body with
//! the literal `[inline media omitted from search index]`, and that replacement
//! is the only thing standing between a 4 MB image and a duplicate of it in an
//! index that is scanned on every keystroke of the search box, replicated into
//! the write-ahead log, and copied by every backup.
//!
//! The assertions below deliberately read **the index**, not the row. The row
//! keeping the image is correct and expected; the index keeping it is the defect.

use std::error::Error;

use odysseus_store::{
    Store,
    sessions::{NewMessage, NewSession, SessionStore},
};

fn store() -> Result<(tempfile::TempDir, Store), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("app.db"));
    store.migrate()?;
    Ok((dir, store))
}

const T0: f64 = 1_800_000_000.0;

/// A plausible base64 image data URI — long enough that its presence in an index
/// would be obvious in a size check.
fn image_data_uri() -> String {
    let payload = "iVBORw0KGgoAAAANSUhEUg".repeat(64);
    format!("data:image/png;base64,{payload}")
}

/// An audio data URI: the trigger matches `data:audio/` too, not just images.
fn audio_data_uri() -> String {
    let payload = "SUQzBAAAAAAAI1RTU0UAAAAPAAADTGF2ZjU4Ljc2LjEwMA".repeat(64);
    format!("data:audio/mpeg;base64,{payload}")
}

/// The indexed text for a message, read from the index itself.
fn indexed_text(store: &Store, message_id: &str) -> Result<Option<String>, Box<dyn Error>> {
    let conn = store.connect()?;
    let mut stmt =
        conn.prepare("SELECT content FROM chat_messages_fts WHERE message_id = ?1 LIMIT 1")?;
    let mut rows = stmt.query([message_id])?;
    Ok(match rows.next()? {
        Some(row) => Some(row.get::<_, String>(0)?),
        None => None,
    })
}

/// The row's own content, for the control assertion that the message itself is
/// intact.
fn row_content(store: &Store, message_id: &str) -> Result<String, Box<dyn Error>> {
    let conn = store.connect()?;
    Ok(conn.query_row(
        "SELECT content FROM chat_messages WHERE id = ?1",
        [message_id],
        |row| row.get(0),
    )?)
}

/// A message with an inline image must be indexed with the body replaced.
#[tokio::test]
async fn an_inline_image_is_not_indexed() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    sessions
        .create(NewSession {
            id: "s1".into(),
            name: "Chat".into(),
            endpoint_url: "http://example.test".into(),
            model: "m".into(),
            owner: "alice".into(),
            rag: false,
            now: T0,
        })
        .await?;

    let content = format!("Look at this screenshot please\n{}", image_data_uri());
    sessions
        .add_message("s1", NewMessage::new("m1", "user", &content, T0))
        .await?;

    let indexed = indexed_text(&store, "m1")?;
    let indexed = indexed.ok_or("the message must be present in the index")?;

    assert_eq!(
        indexed, "[inline media omitted from search index]",
        "the trigger must replace the body, not index it"
    );
    assert!(
        !indexed.contains("base64"),
        "no base64 marker may survive into the index"
    );
    assert!(
        !indexed.contains("data:image/"),
        "no data: URI may survive into the index"
    );

    // The control: the message row still holds the image. Scrubbing the index
    // must not destroy the transcript.
    assert_eq!(
        row_content(&store, "m1")?,
        content,
        "the stored message must keep its content"
    );
    Ok(())
}

/// `data:audio/` is matched as well as `data:image/`.
#[tokio::test]
async fn inline_audio_is_not_indexed() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    sessions
        .create(NewSession {
            id: "s1".into(),
            name: "Chat".into(),
            endpoint_url: "http://example.test".into(),
            model: "m".into(),
            owner: "alice".into(),
            rag: false,
            now: T0,
        })
        .await?;
    sessions
        .add_message(
            "s1",
            NewMessage::new("m1", "assistant", audio_data_uri(), T0),
        )
        .await?;

    let indexed = indexed_text(&store, "m1")?.ok_or("message must be indexed")?;
    assert_eq!(indexed, "[inline media omitted from search index]");
    Ok(())
}

/// The whole index, not one message: this is the assertion that would catch a
/// regression where *some* path writes the raw body.
#[tokio::test]
async fn no_index_entry_contains_inline_media() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    sessions
        .create(NewSession {
            id: "s1".into(),
            name: "Chat".into(),
            endpoint_url: "http://example.test".into(),
            model: "m".into(),
            owner: "alice".into(),
            rag: false,
            now: T0,
        })
        .await?;

    let bodies = [
        format!("see this {}", image_data_uri()),
        format!("listen {}", audio_data_uri()),
        "an ordinary message with no media".to_string(),
        "plain text".to_string(),
    ];
    for (i, body) in bodies.iter().enumerate() {
        sessions
            .add_message(
                "s1",
                NewMessage::new(
                    format!("m{i}"),
                    "user",
                    body,
                    T0 + f64::from(u32::try_from(i).unwrap_or(0)),
                ),
            )
            .await?;
    }

    let conn = store.connect()?;
    let mut stmt = conn.prepare("SELECT content FROM chat_messages_fts")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let indexed = row?;
        assert!(
            !indexed.contains(";base64,"),
            "an index entry leaked a base64 payload: {} bytes",
            indexed.len()
        );
        assert!(
            !indexed.contains("data:image/") && !indexed.contains("data:audio/"),
            "an index entry leaked a data URI"
        );
    }
    Ok(())
}

/// The index must not grow by the size of the payload. A size assertion is the
/// one that survives someone swapping the scrub for a truncation.
#[tokio::test]
async fn the_index_does_not_balloon_with_media_size() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    sessions
        .create(NewSession {
            id: "s1".into(),
            name: "Chat".into(),
            endpoint_url: "http://example.test".into(),
            model: "m".into(),
            owner: "alice".into(),
            rag: false,
            now: T0,
        })
        .await?;

    // A payload far larger than anything a human pastes.
    let huge = format!("data:image/png;base64,{}", "QUJDREVG".repeat(50_000));
    sessions
        .add_message("s1", NewMessage::new("m1", "user", &huge, T0))
        .await?;

    let indexed = indexed_text(&store, "m1")?.ok_or("message must be indexed")?;
    assert!(
        indexed.len() < 100,
        "the index entry must be the placeholder, not {} bytes",
        indexed.len()
    );
    Ok(())
}

/// An update that introduces media must be re-scrubbed: the `AFTER UPDATE`
/// trigger deletes and re-inserts (`database.py:2234-2239`).
#[tokio::test]
async fn an_update_introducing_media_is_re_scrubbed() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    sessions
        .create(NewSession {
            id: "s1".into(),
            name: "Chat".into(),
            endpoint_url: "http://example.test".into(),
            model: "m".into(),
            owner: "alice".into(),
            rag: false,
            now: T0,
        })
        .await?;
    sessions
        .add_message(
            "s1",
            NewMessage::new("m1", "user", "harmless original text", T0),
        )
        .await?;
    assert_eq!(
        indexed_text(&store, "m1")?.as_deref(),
        Some("harmless original text")
    );

    // Replace the row directly, which is what the `AFTER UPDATE` trigger sees.
    let conn = store.connect()?;
    conn.execute(
        "UPDATE chat_messages SET content = ?1 WHERE id = 'm1'",
        [format!("now with {}", image_data_uri())],
    )?;

    let indexed = indexed_text(&store, "m1")?.ok_or("message must be indexed")?;
    assert_eq!(
        indexed, "[inline media omitted from search index]",
        "the AFTER UPDATE trigger must re-scrub"
    );
    Ok(())
}

/// A deleted message must leave the index: the `AFTER DELETE` trigger, and a
/// guard against orphaned index rows accumulating payload forever.
#[tokio::test]
async fn deleting_a_message_removes_its_index_entry() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    sessions
        .create(NewSession {
            id: "s1".into(),
            name: "Chat".into(),
            endpoint_url: "http://example.test".into(),
            model: "m".into(),
            owner: "alice".into(),
            rag: false,
            now: T0,
        })
        .await?;
    sessions
        .add_message(
            "s1",
            NewMessage::new("m1", "user", format!("hi {}", image_data_uri()), T0),
        )
        .await?;
    assert!(indexed_text(&store, "m1")?.is_some());

    let conn = store.connect()?;
    conn.execute("DELETE FROM chat_messages WHERE id = 'm1'", [])?;

    assert_eq!(
        indexed_text(&store, "m1")?,
        None,
        "the AFTER DELETE trigger must remove the index row"
    );
    Ok(())
}
