//! Owner-scoped CRUD over `model_endpoints`, against a real migrated database.
//!
//! `AGENTS.md` §11: persistence tests use a real temp SQLite, never mocks. Every
//! test here opens a fresh database, applies the real baseline migration, and
//! talks to it through `ModelEndpointStore`.

use {
    fernet::Fernet,
    odysseus_store::{
        crypto::{is_encrypted, maybe_decrypt},
        model_endpoints::{
            ApiKey, Caller, EndpointError, ModelEndpointStore, ModelIdList, NewEndpoint, Provider,
            RefreshMode,
        },
        pool,
        schema::migrate,
    },
    tempfile::TempDir,
};

/// A migrated database in a temp dir, plus the key its credentials are sealed with.
struct Fixture {
    _dir: TempDir,
    conn: rusqlite::Connection,
    key: Fernet,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = rusqlite::Connection::open(dir.path().join("app.db")).expect("open");
        pool::configure(&conn).expect("configure");
        migrate(&conn).expect("migrate");
        Self {
            _dir: dir,
            conn,
            key: Fernet::new(&Fernet::generate_key()).expect("key"),
        }
    }

    fn store(&self) -> ModelEndpointStore<'_> {
        ModelEndpointStore::new(&self.conn, &self.key)
    }
}

fn endpoint(name: &str, url: &str) -> NewEndpoint {
    NewEndpoint {
        name: name.to_owned(),
        base_url: url.to_owned(),
        ..NewEndpoint::default()
    }
}

#[test]
fn a_created_endpoint_round_trips_every_column() {
    let f = Fixture::new();
    let store = f.store();
    let caller = Caller::user("alice");

    let mut new = endpoint("Local llama", "http://localhost:1234/v1");
    new.api_key = Some("sk-secret".to_owned());
    new.is_enabled = Some(true);
    new.model_type = Some("llm".to_owned());
    new.pinned_models = Some(ModelIdList::from_ids(["llama3", "qwen"]));
    new.supports_tools = Some(true);
    new.model_refresh_interval = Some(300);
    new.model_refresh_timeout = Some(5);

    let created = store.create(&caller, &new).expect("create");
    assert_eq!(created.name.as_ref(), "Local llama");
    assert_eq!(created.base_url.as_ref(), "http://localhost:1234/v1");
    assert_eq!(created.owner.as_deref(), Some("alice"));
    assert!(created.is_enabled);
    assert_eq!(created.model_type.as_deref(), Some("llm"));
    assert_eq!(created.supports_tools, Some(true));
    assert_eq!(created.model_refresh_interval, Some(300));
    assert_eq!(created.model_refresh_timeout, Some(5));
    assert!(created.pinned_models.contains("llama3"));
    assert!(!created.hidden_models.contains("llama3"));
    assert_eq!(created.provider(), Provider::OpenAi);

    let fetched = store
        .get(&caller, &created.id)
        .expect("get")
        .expect("present");
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.api_key.as_stored().map(is_encrypted), Some(true));
    assert_eq!(
        fetched.api_key.expose(&f.key).ok().flatten().as_deref(),
        Some("sk-secret")
    );
    assert!(!created.created_at.is_empty(), "created_at must be stamped");
    assert_eq!(created.created_at, created.updated_at);
}

#[test]
fn defaults_match_the_python_insert() {
    let f = Fixture::new();
    let store = f.store();
    let created = store
        .create(&Caller::user("alice"), &endpoint("d", "http://x/v1"))
        .expect("create");

    // routes/model_routes.py:2159 passes model_type="llm" explicitly.
    assert_eq!(created.model_type.as_deref(), Some("llm"));
    // An endpoint is created enabled.
    assert!(created.is_enabled);
    // Empty model lists are SQL NULL, not "[]" — matching `json.dumps(x) if x else None`.
    assert_eq!(created.cached_models.as_raw(), None);
    assert_eq!(created.pinned_models.as_raw(), None);
    assert_eq!(created.hidden_models.as_raw(), None);
    // supports_tools stays unknown rather than defaulting to a guess.
    assert_eq!(created.supports_tools, None);
    assert_eq!(created.endpoint_kind.as_deref(), Some("auto"));
    assert_eq!(created.model_refresh_mode.as_deref(), Some("auto"));
    assert_eq!(
        store
            .get(&Caller::user("alice"), &created.id)
            .unwrap()
            .unwrap()
            .kind()
            .expect("kind"),
        Default::default()
    );
}

#[test]
fn a_shared_endpoint_has_a_null_owner_and_is_visible_to_everyone() {
    let f = Fixture::new();
    let store = f.store();
    let admin = Caller::admin("root");

    let mut shared = endpoint("Shared cloud", "https://api.openai.com/v1");
    shared.share_with_all = true;
    let created = store.create(&admin, &shared).expect("create");
    assert_eq!(
        created.owner, None,
        "shared means NULL owner, per database.py:189"
    );

    // Every user sees it, and nobody but an admin may write to it.
    for name in ["alice", "bob"] {
        let listed = store.list(&Caller::user(name)).expect("list");
        assert_eq!(listed.len(), 1, "{name} must see the shared endpoint");
    }
    let err = store
        .delete(&Caller::user("alice"), &created.id)
        .expect_err("alice must not delete a shared row");
    assert!(matches!(err, EndpointError::NotFound(_)), "got {err:?}");
    assert!(store.delete(&admin, &created.id).expect("admin delete"));
}

#[test]
fn a_listing_returns_your_own_endpoints_plus_shared_ones() {
    let f = Fixture::new();
    let store = f.store();
    let admin = Caller::admin("root");

    store
        .create(
            &Caller::user("alice"),
            &endpoint("alice local", "http://localhost:1/v1"),
        )
        .expect("alice");
    store
        .create(
            &Caller::user("bob"),
            &endpoint("bob local", "http://localhost:2/v1"),
        )
        .expect("bob");

    let mut shared = endpoint("shared", "https://api.mistral.ai/v1");
    shared.share_with_all = true;
    store.create(&admin, &shared).expect("shared");

    let mine = store.list(&Caller::user("alice")).expect("list");
    let names: Vec<&str> = mine.iter().map(|e| e.name.as_ref()).collect();
    assert_eq!(
        names,
        ["alice local", "shared"],
        "ordered by name, own plus shared"
    );

    let bobs = store.list(&Caller::user("bob")).expect("list");
    assert_eq!(bobs.len(), 2);
    assert!(
        bobs.iter().all(|e| e.name.as_ref() != "alice local"),
        "bob must not see alice's"
    );
}

/// A row-existence oracle: "not yours" and "does not exist" must be
/// indistinguishable, or a caller can enumerate other users' endpoint ids.
#[test]
fn a_row_owned_by_someone_else_looks_absent() {
    let f = Fixture::new();
    let store = f.store();
    let admin = Caller::admin("root");

    let bobs_row = store
        .create(
            &Caller::user("bob"),
            &endpoint("bob", "http://localhost:2/v1"),
        )
        .expect("create");

    let alice = Caller::user("alice");
    let someone_elses = store.get(&alice, &bobs_row.id).expect("get");
    let no_such_row = store.get(&alice, "deadbeef").expect("get");
    assert!(someone_elses.is_none(), "alice must not read bob.s row");
    assert!(
        no_such_row.is_none(),
        "a missing id must look identical to a forbidden one"
    );

    let err = store
        .update(&alice, &bobs_row.id, &Default::default())
        .expect_err("no write");
    assert!(matches!(err, EndpointError::NotFound(_)));
}

#[test]
fn an_admin_may_write_any_row() {
    let f = Fixture::new();
    let store = f.store();
    // Bob's row, created by Bob; the admin then edits it.
    let row = store
        .create(
            &Caller::user("bob"),
            &endpoint("bob", "http://localhost:2/v1"),
        )
        .expect("create");

    let mut patch = odysseus_store::model_endpoints::EndpointPatch::default();
    patch.name = Some("renamed by admin".to_owned());
    let updated = store
        .update(&Caller::admin("root"), &row.id, &patch)
        .expect("admin update");
    assert_eq!(updated.name.as_ref(), "renamed by admin");
}

#[test]
fn an_anonymous_caller_cannot_create() {
    let f = Fixture::new();
    let store = f.store();
    let err = store
        .create(&Caller::default(), &endpoint("x", "http://x/v1"))
        .expect_err("anonymous");
    assert!(matches!(err, EndpointError::Unauthenticated), "got {err:?}");
}

#[test]
fn a_blank_name_or_url_is_rejected_rather_than_stored() {
    let f = Fixture::new();
    let store = f.store();
    let caller = Caller::user("alice");

    for bad in [endpoint("   ", "http://x/v1"), endpoint("n", "  ")] {
        let err = store.create(&caller, &bad).expect_err("blank field");
        assert!(matches!(err, EndpointError::Validation(_)), "got {err:?}");
    }
    assert!(
        store.list(&caller).expect("list").is_empty(),
        "nothing was stored"
    );
}

#[test]
fn an_update_touches_only_the_fields_it_names() {
    let f = Fixture::new();
    let store = f.store();
    let caller = Caller::user("alice");

    let mut new = endpoint("before", "http://localhost:1234/v1");
    new.api_key = Some("sk-original".to_owned());
    new.pinned_models = Some(ModelIdList::from_ids(["pinned-a"]));
    new.supports_tools = Some(true);
    new.model_refresh_interval = Some(60);
    let created = store.create(&caller, &new).expect("create");

    let mut patch = odysseus_store::model_endpoints::EndpointPatch::default();
    patch.name = Some("after".to_owned());
    let updated = store.update(&caller, &created.id, &patch).expect("update");

    assert_eq!(updated.name.as_ref(), "after");
    assert_eq!(updated.base_url, created.base_url, "untouched");
    assert_eq!(
        updated.pinned_models.as_raw(),
        created.pinned_models.as_raw(),
        "untouched"
    );
    assert_eq!(updated.supports_tools, Some(true), "untouched");
    assert_eq!(updated.model_refresh_interval, Some(60), "untouched");
    assert_eq!(
        updated.api_key.expose(&f.key).ok().flatten().as_deref(),
        Some("sk-original"),
        "untouched"
    );
    assert_ne!(
        updated.updated_at, created.updated_at,
        "a write must move updated_at"
    );
}

#[test]
fn a_patch_can_clear_the_credential_distinctly_from_omitting_it() {
    let f = Fixture::new();
    let store = f.store();
    let caller = Caller::user("alice");

    let mut new = endpoint("e", "http://x/v1");
    new.api_key = Some("sk-secret".to_owned());
    let created = store.create(&caller, &new).expect("create");

    // Omitted: the key survives.
    let mut rename = odysseus_store::model_endpoints::EndpointPatch::default();
    rename.name = Some("renamed".to_owned());
    let kept = store.update(&caller, &created.id, &rename).expect("update");
    assert_eq!(
        kept.api_key.expose(&f.key).ok().flatten().as_deref(),
        Some("sk-secret")
    );

    // `Some(None)`: the key is cleared. A plain `Option<String>` could not say this,
    // and getting it wrong silently deletes a credential on every unrelated edit.
    let mut clear = odysseus_store::model_endpoints::EndpointPatch::default();
    clear.api_key = Some(None);
    let cleared = store.update(&caller, &created.id, &clear).expect("update");
    assert!(!cleared.api_key.is_present());
}

/// Every nullable TEXT/numeric column follows the same three-way patch rule.
///
/// `model_type` and `provider_auth_id` are the two that a plausible
/// implementation gets wrong: `.as_deref().flatten().or(existing)` looks like it
/// honours `Some(None)`, but `flatten()` maps `Some(None)` to `None` and `.or()`
/// then puts the old value straight back, so "clear this" silently becomes "leave
/// this alone" while the sibling numeric columns clear correctly. This pins all
/// five together so the inconsistency cannot come back on one of them.
#[test]
fn every_nullable_column_clears_and_omits_independently() {
    let f = Fixture::new();
    let store = f.store();
    let caller = Caller::user("alice");

    let mut new = endpoint("e", "http://x/v1");
    new.model_type = Some("chat".to_owned());
    new.provider_auth_id = Some("cred-7".to_owned());
    new.model_refresh_interval = Some(900);
    new.model_refresh_timeout = Some(30);
    new.supports_tools = Some(true);
    let created = store.create(&caller, &new).expect("create");
    assert_eq!(created.model_type.as_deref(), Some("chat"));

    // Omitting all five leaves every one of them alone.
    let mut rename = odysseus_store::model_endpoints::EndpointPatch::default();
    rename.name = Some("renamed".to_owned());
    let kept = store.update(&caller, &created.id, &rename).expect("update");
    assert_eq!(kept.model_type.as_deref(), Some("chat"));
    assert_eq!(kept.provider_auth_id.as_deref(), Some("cred-7"));
    assert_eq!(kept.model_refresh_interval, Some(900));
    assert_eq!(kept.model_refresh_timeout, Some(30));
    assert_eq!(kept.supports_tools, Some(true));

    // `Some(None)` clears all five.
    let mut clear = odysseus_store::model_endpoints::EndpointPatch::default();
    clear.model_type = Some(None);
    clear.provider_auth_id = Some(None);
    clear.model_refresh_interval = Some(None);
    clear.model_refresh_timeout = Some(None);
    clear.supports_tools = Some(None);
    let cleared = store.update(&caller, &created.id, &clear).expect("update");
    assert_eq!(cleared.model_type, None, "model_type must clear");
    assert_eq!(
        cleared.provider_auth_id, None,
        "provider_auth_id must clear"
    );
    assert_eq!(cleared.model_refresh_interval, None);
    assert_eq!(cleared.model_refresh_timeout, None);
    assert_eq!(cleared.supports_tools, None);

    // And the values survive a round trip through SQLite, not just the returned row.
    let reread = store
        .get(&caller, &created.id)
        .expect("get")
        .expect("row still present");
    assert_eq!(reread.model_type, None);
    assert_eq!(reread.provider_auth_id, None);
}

#[test]
fn a_refresh_updates_the_cache_without_moving_updated_at() {
    let f = Fixture::new();
    let store = f.store();
    let created = store
        .create(&Caller::user("alice"), &endpoint("e", "http://x/v1"))
        .expect("create");

    let models = ModelIdList::from_ids(["llama3:8b", "qwen2.5"]);
    assert!(
        store
            .store_cached_models(&created.id, &models)
            .expect("store")
    );
    let after = store
        .get(&Caller::user("alice"), &created.id)
        .unwrap()
        .unwrap();

    assert!(after.cached_models.contains("llama3:8b"));
    assert_eq!(
        after.updated_at, created.updated_at,
        "a background refresh is not a configuration change"
    );
}

#[test]
fn refresh_and_kind_parse_their_stored_names() {
    let f = Fixture::new();
    let store = f.store();
    let row = store
        .create(&Caller::user("alice"), &endpoint("e", "http://x/v1"))
        .expect("create");

    assert_eq!(row.refresh_mode().expect("mode"), RefreshMode::Auto);
    assert_eq!(
        RefreshMode::parse("manual").expect("manual"),
        RefreshMode::Manual
    );
    assert_eq!(
        RefreshMode::parse("disabled").expect("disabled"),
        RefreshMode::Disabled
    );
    assert!(RefreshMode::parse("hourly").is_err());
}

#[test]
fn a_key_never_appears_in_debug_output() {
    let f = Fixture::new();
    let store = f.store();
    let mut new = endpoint("e", "http://x/v1");
    new.api_key = Some("sk-super-secret-value".to_owned());
    let created = store.create(&Caller::user("alice"), &new).expect("create");

    let rendered = format!("{created:?}");
    assert!(
        !rendered.contains("super-secret"),
        "Debug leaked the credential: {rendered}"
    );
    let rendered = format!("{:?}", created.api_key);
    assert!(
        !rendered.contains("super-secret"),
        "ApiKey Debug leaked the credential: {rendered}"
    );
    assert!(
        rendered.contains("ApiKey"),
        "should still be recognisable: {rendered}"
    );
}

#[test]
fn deleting_a_missing_row_reports_not_found_rather_than_succeeding() {
    let f = Fixture::new();
    let store = f.store();
    let err = store
        .delete(&Caller::admin("root"), "deadbeef")
        .expect_err("missing");
    assert!(matches!(err, EndpointError::NotFound(_)));
}

#[test]
fn a_null_owner_row_written_by_python_is_still_readable() {
    // The legacy shape: `owner` was never set, so a Python-era row has NULL for
    // every optional column. Reading it must not fail.
    let f = Fixture::new();
    f.conn
        .execute(
            "INSERT INTO model_endpoints (id, name, base_url, created_at, updated_at)
             VALUES ('legacy01', 'Legacy', 'http://legacy/v1', '2025-01-01 00:00:00', '2025-01-01 00:00:00')",
            [],
        )
        .expect("insert legacy row");

    let store = f.store();
    let row = store
        .get(&Caller::user("anyone"), "legacy01")
        .expect("get")
        .expect("present");
    assert_eq!(row.owner, None);
    assert_eq!(row.api_key.as_stored(), None);
    assert!(row.cached_models.is_empty());
    assert_eq!(row.model_type, None);
    assert_eq!(
        row.kind().expect("kind"),
        Default::default(),
        "a NULL kind reads as auto"
    );
}

#[test]
fn the_stored_credential_is_encrypted_at_rest() {
    let f = Fixture::new();
    let store = f.store();
    let mut new = endpoint("e", "http://x/v1");
    new.api_key = Some("sk-plaintext-must-not-persist".to_owned());
    let created = store.create(&Caller::user("alice"), &new).expect("create");

    let stored: String = f
        .conn
        .query_row(
            "SELECT api_key FROM model_endpoints WHERE id = ?1",
            [&created.id],
            |r| r.get(0),
        )
        .expect("raw column");
    assert!(
        !stored.contains("plaintext"),
        "the column holds plaintext: {stored}"
    );
    assert!(is_encrypted(&stored));
    assert_eq!(
        maybe_decrypt(&f.key, &stored).expect("decrypt"),
        "sk-plaintext-must-not-persist"
    );
}

#[test]
fn an_absent_or_blank_key_is_stored_as_null() {
    let f = Fixture::new();
    let store = f.store();
    for blank in [None, Some(String::new()), Some("   ".to_owned())] {
        let mut new = endpoint("e", "http://x/v1");
        new.api_key = blank;
        let created = store.create(&Caller::user("alice"), &new).expect("create");
        assert_eq!(
            created.api_key.as_stored(),
            None,
            "blank keys must not become enc: of nothing"
        );
    }
}

/// A hand-written `Debug` for a credential type is only worth anything if the
/// `ApiKey` newtype itself cannot be formatted by accident. This pins the type's
/// API surface so a future `Display` cannot be added without a test noticing.
#[test]
fn the_credential_type_has_no_display_and_seals_idempotently() {
    let key = Fernet::new(&Fernet::generate_key()).expect("key");

    let once = ApiKey::seal(&key, Some("sk-value"));
    let stored = once.as_stored().expect("sealed").to_owned();
    assert!(is_encrypted(&stored));

    // Sealing the *stored* form again must be a no-op, not a second layer.
    let twice = ApiKey::seal(&key, Some(&stored));
    assert_eq!(
        twice.as_stored(),
        Some(stored.as_str()),
        "double encryption would orphan the row"
    );
    assert_eq!(
        twice.expose(&key).ok().flatten().as_deref(),
        Some("sk-value")
    );

    let resealed = once.clone().reseal(&key);
    assert_eq!(
        resealed.as_stored(),
        once.as_stored(),
        "reseal must not re-encrypt"
    );
    assert_eq!(
        resealed.expose(&key).ok().flatten().as_deref(),
        Some("sk-value")
    );
}

/// §12.1 rule 7: a per-item stored type gets a size ceiling. An endpoint row is
/// held whole for the life of a request.
///
/// The measured size is 264 bytes -- 18 columns, each a `Box<str>` (16 bytes) or a
/// `Copy` scalar. The ceiling of 320 leaves room for ~2 more `Box<str>` fields, so
/// one accidental `String`/`Vec` (24 bytes each, plus the capacity field §12.1 rule 1
/// exists to delete) still fits; two do not. A jump past the ceiling means a new owned
/// field crept in and §12.1 needs re-reading, not a bigger number.
#[test]
fn a_stored_endpoint_row_has_a_size_ceiling() {
    use {odysseus_store::model_endpoints::ModelEndpoint, std::mem::size_of};
    let size = size_of::<ModelEndpoint>();
    assert!(
        size <= 320,
        "ModelEndpoint grew to {size} bytes (measured 264 at the time this bound was \
         set). Every string here is a Box<str>, so growth means a new owned field crept \
         in; check §12.1 before raising this."
    );
    assert!(
        size_of::<Box<str>>() <= size_of::<String>(),
        "the whole point of §12.1 rule 1"
    );
}
