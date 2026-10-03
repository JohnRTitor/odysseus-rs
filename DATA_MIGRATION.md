# Odysseus — Data & State Migration

Inventory of every persistent store, its disposition, and its migration risk.

---

## 1. Store inventory

| Store | Location | Tech | Disposition |
|---|---|---|---|
| `data/app.db` | Primary | SQLite + SQLAlchemy, 27 tables + FTS5 | **RETAIN schema.** Port to `rusqlite`. Single baseline migration from live `sqlite_master`. |
| `data/scheduled_emails.db` | Email sidecar | Raw `sqlite3`, 15 tables, **not in the ORM** | **MIGRATE.** Decision at Phase 15: fold into `app.db` or keep separate. |
| `data/auth.json` | Users | JSON, atomic write | **RETAIN format.** Must interoperate with existing installs during transition. |
| `data/sessions.json` | Browser session tokens | JSON | **RETAIN format.** |
| `data/settings.json` | ~60 settings | JSON | **RETAIN format.** |
| `data/features.json` | 8 feature toggles | JSON | **RETAIN format.** |
| `data/user_prefs.json` | Per-user prefs | JSON, `_users: {user: {…}}` | **RETAIN format.** |
| `data/presets.json` | Presets + templates | JSON | **RETAIN format.** |
| `data/integrations.json` | Integration configs, Fernet-encrypted keys | JSON | **RETAIN format.** |
| `data/contacts.json` | CardDAV contacts | JSON | **RETAIN format.** |
| `data/vault.json` | Vault | JSON | **RETAIN format.** |
| `data/cookbook_state.json` | Cookbook state | JSON | **RETAIN format.** |
| `data/bg_jobs.json` | Detached job records | JSON | **RETAIN format.** |
| `data/.app_key` | Fernet key, 44 bytes | Binary | **RETAIN format.** Non-negotiable — changing it orphans every encrypted column. |
| `data/skills/` | `SKILL.md` per skill + `_usage.json` | Filesystem | **RETAIN.** |
| `data/skills/_usage.json` | Usage counters | JSON | **RETAIN.** |
| `data/uploads/` | Uploaded files + `.vision/{id}.txt` | Filesystem | **RETAIN.** |
| `data/uploads/COMPOSE_UPLOADS/_compose/` | Staged email attachments | Filesystem | **RETAIN.** |
| `data/mail-attachments/` | Extracted email attachments | Filesystem | **RETAIN.** |
| `data/logs/app.log` | Rotating log, 5 MB × 3 backups | File | **RETAIN format.** `tracing` + `tracing-appender`. |
| `data/cache/{search,content}/` | On-disk JSON cache, 1,000 entries, LRU | Filesystem | **RETAIN semantics.** Cold-cache on first boot is acceptable; port the cache. |
| `data/deep_research/{sid}.json` | Research reports | JSON | **RETAIN.** |
| `data/tts_cache/` | TTS WAV/MP3, 500 MB LRU | Filesystem | **RETAIN.** |
| `data/chroma/` | ChromaDB persistence | External service | **REPLACED** by `sqlite-vec`. |
| `data/memory_tidy_state.json` | Extraction fingerprints | JSON | **RETAIN.** |
| `data/mcp_oauth/` | OAuth state | Filesystem | **RETAIN.** |
| `data/hwfit/*.json` | Hardware + model catalog cache, 24 h TTL | JSON | **RETAIN.** |
| `data/hf_models.json`, `mlx_community_models.json` | Model catalogs | JSON | **RETAIN.** |
| `localStorage` (17 keys) | Theme, toggles, model prefs, calendar state | Browser | **RETAIN key strings.** See `UI_MIGRATION.md` §7.1. |
| `sessionStorage` | Incognito set, settings prefetch handoff | Browser | **RETAIN.** |
| Service worker Cache API | 112 precache entries | Browser | **REPLACED** — rewritten for hashed assets. |
| ChromeDB HTTP collections (4) | tool_index, rag, memory, embeddings | External service | **REPLACED** by `sqlite-vec` tables. |

---

## 2. `data/app.db` — full schema

27 tables. Verified against the live `sqlite_master`, not the SQLAlchemy models.

Every one of the 27 SQLAlchemy models in `core/database.py` was then diffed against that
baseline — column by column, plus PKs, FKs, unique constraints and indexes — and the
resulting inventory was checked against all 49 `database.py` migration helpers. Coverage
is in **§2.3**; every discrepancy found is in **§2.2**. Nothing in either list has been
applied to `0001_baseline.sql`: decision D6 (§3.1) makes the live database the winner,
and a human decides whether a follow-up migration is warranted.

### 2.1 DDL

```sql
sessions(
  id VARCHAR NOT NULL, name VARCHAR NOT NULL, endpoint_url VARCHAR NOT NULL,
  model VARCHAR NOT NULL, owner VARCHAR, rag BOOLEAN, archived BOOLEAN,
  folder VARCHAR, headers JSON, last_accessed DATETIME, last_message_at DATETIME,
  is_important BOOLEAN, message_count INTEGER, total_input_tokens INTEGER,
  total_output_tokens INTEGER, mode VARCHAR, crew_member_id VARCHAR,
  created_at DATETIME NOT NULL, updated_at DATETIME NOT NULL, PRIMARY KEY(id))
  IDX ix_sessions_id(id) ix_sessions_owner(owner)
  IDX ix_sessions_active(archived,last_accessed) ix_sessions_search(name,archived)
  IDX ix_sessions_last_message_at(archived,last_message_at)   -- migration-only, absent from model

chat_messages(
  id VARCHAR NOT NULL, session_id VARCHAR NOT NULL, role VARCHAR NOT NULL,
  content TEXT NOT NULL, "metadata" TEXT, timestamp DATETIME, PRIMARY KEY(id),
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE)
  IDX ix_chat_messages_id(id) ix_chat_messages_session_id(session_id)
  IDX ix_messages_session_time(session_id,timestamp)
-- deliberately NO created_at/updated_at

chat_messages_fts USING fts5(
  content, message_id UNINDEXED, session_id UNINDEXED, role UNINDEXED)
TRIGGER chat_messages_fts_ai AFTER INSERT ON chat_messages
TRIGGER chat_messages_fts_ad AFTER DELETE ON chat_messages
TRIGGER chat_messages_fts_au AFTER UPDATE ON chat_messages
-- both triggers replace content containing ';base64,', 'data:image/',
-- 'data:audio/' with '[inline media omitted from search index]'

documents(
  id VARCHAR NOT NULL, session_id VARCHAR, title VARCHAR NOT NULL, language VARCHAR,
  current_content TEXT NOT NULL, version_count INTEGER, is_active BOOLEAN,
  archived BOOLEAN, owner VARCHAR, tidy_verdict VARCHAR,
  source_email_uid VARCHAR, source_email_folder VARCHAR,
  source_email_account_id VARCHAR, source_email_message_id VARCHAR,
  created_at/updated_at DATETIME NOT NULL, PRIMARY KEY(id),
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE SET NULL)
  IDX ix_documents_{id,owner,session_id,source_email_message_id}

document_versions(
  id VARCHAR NOT NULL, document_id VARCHAR NOT NULL, version_number INTEGER NOT NULL,
  content TEXT NOT NULL, summary VARCHAR, source VARCHAR, created_at DATETIME,
  PRIMARY KEY(id), FOREIGN KEY(document_id) REFERENCES documents(id) ON DELETE CASCADE)

gallery_albums(id, name NOT NULL, description, cover_id, owner, created_at/updated_at, PK(id))
gallery_images(
  id VARCHAR NOT NULL, filename VARCHAR NOT NULL, prompt TEXT NOT NULL, caption,
  model, size, quality, tags, ai_tags TEXT, session_id, album_id, owner,
  is_active BOOLEAN, favorite BOOLEAN, file_hash VARCHAR(64), taken_at DATETIME,
  camera_make, camera_model, gps_lat, gps_lng, width, height, file_size INTEGER,
  created_at/updated_at, PRIMARY KEY(id), UNIQUE(filename),
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE SET NULL,
  FOREIGN KEY(album_id) REFERENCES gallery_albums(id) ON DELETE SET NULL)
  9 indexes incl. ix_gallery_images_active(is_active,created_at)

email_accounts(
  id VARCHAR NOT NULL, owner VARCHAR, name VARCHAR NOT NULL,
  is_default BOOLEAN NOT NULL, enabled BOOLEAN NOT NULL,
  imap_host, imap_port INTEGER, imap_user, imap_password, imap_starttls BOOLEAN,
  smtp_host, smtp_port INTEGER, smtp_security, smtp_user, smtp_password,
  from_address, display_name, oauth_provider VARCHAR,
  oauth_access_token TEXT, oauth_refresh_token TEXT, oauth_token_expiry VARCHAR,
  created_at/updated_at, PRIMARY KEY(id))
  UNIQUE PARTIAL ux_email_accounts_one_default_per_owner
      ON email_accounts (COALESCE(owner,'')) WHERE is_default = 1

email_account_owner_locks(owner_key VARCHAR NOT NULL, PRIMARY KEY(owner_key))

model_endpoints(
  id VARCHAR NOT NULL, name NOT NULL, base_url NOT NULL, api_key TEXT(encrypted),
  is_enabled BOOLEAN, hidden_models TEXT, cached_models TEXT, pinned_models TEXT,
  model_type, endpoint_kind, model_refresh_mode, model_refresh_interval INTEGER,
  model_refresh_timeout INTEGER, supports_tools BOOLEAN, owner VARCHAR,
  provider_auth_id VARCHAR, created_at/updated_at, PRIMARY KEY(id))
-- provider_auth_id is a plain indexed string: NO ForeignKey, NO relationship

provider_auth_sessions(id, provider NOT NULL, owner, label, base_url NOT NULL,
  access_token TEXT(encrypted), refresh_token TEXT(encrypted), last_refresh DATETIME,
  auth_mode, created_at/updated_at, PK(id))

mcp_servers(id, name NOT NULL, transport NOT NULL, command, args TEXT, env TEXT,
  url, is_enabled BOOLEAN, oauth_config TEXT, disabled_tools TEXT,
  oauth_tokens TEXT(encrypted), created_at/updated_at, PK(id))
-- NOTE: mcp_servers has NO owner column

comparisons(id, session_id, owner, prompt NOT NULL,
  model_a/model_b NOT NULL, endpoint_a/endpoint_b NOT NULL,
  response_a/response_b TEXT, metrics_a/metrics_b TEXT, winner, is_blind BOOLEAN,
  blind_mapping TEXT, voted_at DATETIME, created_at/updated_at, PK(id))

signatures(id, owner, name NOT NULL, data_png TEXT(encrypted) NOT NULL,
  width, height, svg TEXT(encrypted), created_at/updated_at, PK(id))

api_tokens(id, owner, name NOT NULL, token_hash NOT NULL, token_prefix NOT NULL,
  scopes NOT NULL, is_active BOOLEAN, last_used_at DATETIME,
  created_at/updated_at, PK(id))
-- token_prefix is the lookup index

webhooks(id, name NOT NULL, url NOT NULL, secret, events NOT NULL,
  is_active BOOLEAN, last_triggered_at DATETIME, last_status_code INTEGER,
  last_error, created_at/updated_at, PK(id))
-- NOTE: webhooks has NO owner column

user_tools(id, name NOT NULL, description, icon, html_content NOT NULL,
  scope NOT NULL, session_id, owner, is_pinned, is_active, version,
  author, created_at/updated_at, PK(id),
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE SET NULL)

user_tool_data(id INTEGER PK (rowid alias, NOT AUTOINCREMENT), tool_id NOT NULL,
  "key" NOT NULL, value TEXT, created_at, updated_at,
  FOREIGN KEY(tool_id) REFERENCES user_tools(id) ON DELETE CASCADE)
  UNIQUE IDX ix_user_tool_data_tool_key(tool_id,"key")

crew_members(id, owner, name NOT NULL, avatar, user_name, personality TEXT, model,
  endpoint_url, greeting TEXT, enabled_tools TEXT, session_id,
  is_active BOOLEAN, sort_order INTEGER, is_default_assistant BOOLEAN, timezone,
  created_at/updated_at, PK(id),
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE SET NULL)

scheduled_tasks(id, owner, name NOT NULL, prompt TEXT, task_type, action, schedule,
  scheduled_time, scheduled_day INTEGER, scheduled_date DATETIME,
  trigger_type, trigger_event, trigger_count, trigger_counter,
  next_run, last_run, status, output_target, session_id, model, endpoint_url,
  run_count INTEGER, cron_expression, then_task_id, webhook_token,
  crew_member_id, character_id, max_steps INTEGER, email_results BOOLEAN,
  notifications_enabled BOOLEAN, created_at/updated_at NOT NULL, PK(id),
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE SET NULL,
  FOREIGN KEY(then_task_id) REFERENCES scheduled_tasks(id) ON DELETE SET NULL,
  UNIQUE(webhook_token))
  IDX ix_scheduled_tasks_{id,owner,next_run} ix_scheduled_tasks_due(status,next_run)
      ix_scheduled_tasks_event(trigger_type,trigger_event,status)
-- character_id is a vestigial reference to a never-created agent_characters table;
-- the FK was deliberately removed (database.py:761-764)

editor_drafts(id, owner, name NOT NULL, source_image_id, width, height,
  payload TEXT NOT NULL, thumbnail TEXT, is_active BOOLEAN,
  created_at/updated_at, PK(id))

task_runs(id, task_id NOT NULL, started_at NOT NULL, finished_at, status, result,
  error, tokens_used INTEGER, steps TEXT, model,
  PRIMARY KEY(id), FOREIGN KEY(task_id) REFERENCES scheduled_tasks(id) ON DELETE CASCADE)
-- NOTE: task_runs has NO owner column

memories(id, text NOT NULL, category, source, owner, session_id,
  timestamp INTEGER, PRIMARY KEY(id),
  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE SET NULL)
-- deliberately NO created_at/updated_at; timestamp is INTEGER unix

notes(id, owner, title, content TEXT, items TEXT, note_type, color, label,
  pinned BOOLEAN, archived BOOLEAN, due_date, source, session_id, sort_order INTEGER,
  image_url, repeat, ai_classification TEXT, ai_content_hash TEXT,
  agent_session_id, created_at/updated_at, PK(id))

calendars(id, owner, name NOT NULL, color, source, account_id, caldav_base_url,
  created_at/updated_at, PK(id))

calendar_events(uid VARCHAR NOT NULL, calendar_id VARCHAR NOT NULL, summary NOT NULL,
  description TEXT, location, dtstart NOT NULL, dtend NOT NULL, all_day BOOLEAN,
  is_utc BOOLEAN NOT NULL, rrule, recurrence_exdates TEXT, color, status,
  importance, event_type, last_pinged, origin, remote_href, remote_etag,
  caldav_sync_pending, created_at/updated_at, PRIMARY KEY(uid),
  FOREIGN KEY(calendar_id) REFERENCES calendars(id))   -- NO ondelete clause

caldav_deleted_events(uid VARCHAR NOT NULL, owner, calendar_id, remote_href,
  remote_etag, caldav_base_url, summary, last_error TEXT,
  created_at/updated_at, PRIMARY KEY(uid))

integrations(id, owner, name NOT NULL, type NOT NULL, config JSON,
  enabled BOOLEAN, created_at/updated_at, PK(id))
```

### 2.2 Schema drift found (must be recorded, not silently fixed)

Diffed 2026-10-03 (T0044). Every row cites **both** sides: the baseline SQL line in
`crates/store/migrations/0001_baseline.sql` and the model or helper line in
`core/database.py`. Two rows predate this diff and are marked where T0044 corrected them;
the correction is part of the record, not a fix.

| Item | Evidence | Action |
|---|---|---|
| `gallery_people` | In the owner-sweep list (`database.py:1447`); **no such table or model exists anywhere** | Do not create. REMOVED. |
| `gallery_people` sweep statement | **Corrected by T0044 — cannot fail.** The loop is guarded by `if "owner" in columns` (`database.py:1452-1453`), and `PRAGMA table_info` on a missing table returns zero rows, so the `UPDATE` is skipped rather than raising | No-op. The "guard or drop" action is already satisfied; no Python change needed. |
| `ix_sessions_last_message_at` | Created by `_migrate_add_last_message_at_column`, **absent from the model** | Create in baseline. **Done** — `0001_baseline.sql:733`. |
| `sessions.character_id` equivalent | `ScheduledTask.character_id` documents itself as vestigial | Preserve the column; do not add an FK. |
| `webhooks`, `task_runs` | In the owner sweep (`database.py:1446,1448`) but have **no `owner` column** (`0001_baseline.sql:508-521`, `:460-473`; models `database.py:648`, `:810`) | No-op; drop from sweep. |
| `mcp_servers` | **Corrected by T0044 — it was never in the sweep.** It has no `owner` column (`0001_baseline.sql:286-301`, model `database.py:570-585`), but it does not appear in the list at `database.py:1442-1448`, so the earlier version of this row was wrong to claim it was swept | No-op. `mcp_servers` needs no sweep decision; `calendar_events` and `user_tool_data` do — see the two rows below. |
| `ModelEndpoint.provider_auth_id` | Plain indexed string, no FK, no relationship | Preserve as-is; do not "fix". |
| Two migrations break on Postgres | Raw `sqlite3` + `.replace("sqlite:///","")` (`database.py:868,1207,2295`) | Do not port. Single baseline is dialect-portable. |
| `calendar_events`, `user_tool_data` in the owner sweep | Listed at `database.py:1445,1447`; neither table has an `owner` column — not in `0001_baseline.sql:43-68` / `:476-485`, nor in the models (`database.py:1856`, `:688`) | No-op; same disposition as `webhooks` / `task_runs`. Two more inert entries in a 19-entry list. |
| **5 owner-bearing tables are missing from the owner sweep** | The list at `database.py:1442-1448` covers 14 of the 19 tables that have an `owner` column. Absent: `caldav_deleted_events`, `editor_drafts`, `email_accounts`, `model_endpoints`, `provider_auth_sessions` — all of which have one (`0001_baseline.sql:30`, `:180`, `:202`, `:332`, `:369`) | **Recommendation, not implemented.** Their rows keep `owner IS NULL`, which every model documents as "legacy/shared" (`database.py:189`) — i.e. visible to all users. Three of the five hold Fernet-encrypted secrets (§6). §3.4 says to port the sweep, so a human must decide whether to port the gap verbatim or close it. |
| `user_tool_data.id` is **not** `AUTOINCREMENT` | `0001_baseline.sql:477,483` (`id INTEGER NOT NULL, PRIMARY KEY (id)`, and the live DB has no `sqlite_sequence` table) vs `database.py:692` `Column(Integer, primary_key=True, autoincrement=True)` — SQLAlchemy's SQLite dialect drops the flag, so no SQLAlchemy-created schema has it either | Baseline wins. §2.1 corrected. **Do not add `AUTOINCREMENT`** in a follow-up: it would create a `sqlite_sequence` table the live DB does not have and change row-reuse semantics. |
| `ix_scheduled_tasks_webhook` | Created by name only by the helper (`database.py:1731`), while the model's `unique=True` (`database.py:759`) materialises as the inline `UNIQUE (webhook_token)` at `0001_baseline.sql:418` — i.e. `sqlite_autoindex_scheduled_tasks_2` in the baseline | No schema change; the uniqueness is identical. But **never refer to this constraint by name** — a legacy Python DB carries the index under a different name. Use `PRAGMA index_list`. |
| `scheduled_tasks` FKs are lost by the Python table rebuild | `0001_baseline.sql:416-417` declares two FKs; the hardcoded `CREATE TABLE scheduled_tasks` at `database.py:1613-1639`, entered when `prompt`/`schedule`/`scheduled_time` are still `NOT NULL` (`database.py:1604-1608`), declares **no** `FOREIGN KEY` at all | Baseline wins. **Recommendation, not implemented:** the data-migration preflight should diff `PRAGMA foreign_key_list('scheduled_tasks')` against the baseline, because `PRAGMA foreign_key_check` (§11.4) cannot detect a *missing* FK and would pass on such a database. |
| 19 columns carry a SQL `DEFAULT` on the helper path only | `0001_baseline.sql:52` (`is_utc BOOLEAN NOT NULL`) and `:436` (`total_input_tokens INTEGER`) vs `database.py:2430` (`DEFAULT 0 NOT NULL`) and `database.py:1297` (`DEFAULT 0`); also `:164` vs `database.py:925`, `:19` vs `database.py:1384`. The baseline contains **zero** `DEFAULT` clauses because SQLAlchemy's `default=` is a Python-side default that never emits DDL | Baseline wins. Rust must supply all 19 in its write path and must never rely on a DB-level default: `documents.archived`, `notes.sort_order`, `notes.repeat`, `gallery_images.caption`, `api_tokens.scopes`, `sessions.total_input_tokens`, `sessions.total_output_tokens`, `model_endpoints.{model_type,endpoint_kind,model_refresh_mode}`, `scheduled_tasks.{task_type,trigger_type,trigger_counter,notifications_enabled}`, `crew_members.is_default_assistant`, `email_accounts.smtp_security`, `calendar_events.{is_utc,importance,recurrence_exdates}`. |
| 35 columns are `TEXT` on the helper path, `VARCHAR` in the baseline | `0001_baseline.sql:309` (`memories.owner VARCHAR`), `:439` (`sessions.crew_member_id VARCHAR`), `:135` (`crew_members.timezone VARCHAR`) vs `database.py:1321` (`ADD COLUMN owner TEXT`), `:1777`, `:1798`. Same class for `notes.{image_url,repeat,ai_content_hash,agent_session_id}`, `email_accounts.{oauth_provider,oauth_access_token,oauth_refresh_token,oauth_token_expiry,display_name,smtp_security}`, `calendar_events.{origin,importance,event_type,remote_href,remote_etag,caldav_sync_pending}`, `calendars.{account_id,caldav_base_url}`, `model_endpoints.{model_type,endpoint_kind,model_refresh_mode}`, `task_runs.model`, `scheduled_tasks.crew_member_id` | Baseline wins. Both spellings carry TEXT affinity in SQLite, so values are interchangeable; do not "correct" the baseline and do not assert on `typeof()`. The 9 helper `TEXT` columns that *do* match the baseline (`model_endpoints.{hidden_models,cached_models,pinned_models}`, `notes.ai_classification`, `gallery_images.caption`, `mcp_servers.{oauth_config,disabled_tools,oauth_tokens}`, `calendar_events.recurrence_exdates`) are correct as written. |
| Column **order** differs on the helper path | `0001_baseline.sql:433` puts `last_message_at` 10th of 19, between `last_accessed` and `is_important`; `database.py:884` `ALTER TABLE sessions ADD COLUMN last_message_at DATETIME` appends after `updated_at`. All 72 helper-added columns behave this way | Baseline wins. **Rust must name columns explicitly in every `INSERT`/`SELECT` and must never use positional `VALUES` or `SELECT *`** — the same column set can arrive in two orders. Note for the legacy-DB migration runner. |
| FTS5 objects are conditional in Python, unconditional in the baseline | `database.py:2204-2207` probes for FTS5 and `return`s early if it is missing, so a Python install without FTS5 has neither the table nor the triggers; `0001_baseline.sql:524` and `:775-793` create them unconditionally | No schema change. But it is a **fail-closed** choice: a `rusqlite` built without FTS5 fails `migrate()` outright instead of degrading to LIKE search. Confirm that is the intent (§3.3 requires the probe; it does not require making migration 0001 fallible). |
| `0001_baseline.sql:8` says `-- Tables: 33` | The file creates 27 `CREATE TABLE` + 1 `CREATE VIRTUAL TABLE`. 33 is the source database's `sqlite_master` row count for `type='table'`, which includes the 5 FTS5 shadow tables | Comment only, and `0001_baseline.sql` must not be edited (D6, §3.2). **Recommendation, not implemented:** `tools/dump-baseline` should print "created tables" and "sqlite_master rows scanned" as separate numbers. |

### 2.3 Coverage of the T0044 diff

**Coverage: 27 of 27 models, 27 of 27 baseline tables, 81 of 81 indexes, 13 of 13 foreign
keys, 4 of 4 unique constraints, 3 of 3 triggers, and all 49 `_migrate_*` helpers plus
`_scrub_legacy_chat_message_fts_media` and the `after_create` DDL hook at
`database.py:466-470`.** Both directions were walked: every model was looked up in the
baseline, and every baseline object was traced back to a model, a helper, or an
explanation below.

| Dimension | Count | Result |
|---|---|---|
| Models inheriting `Base` | 27 | 27 matched to a `CREATE TABLE`; **0 unmatched** |
| Columns | 361 in the baseline, 361 declared by the models | **Exact parity**, per table: same names, same order, same SQL type, same `NOT NULL` flags, same PKs |
| Real-table columns in the baseline with no model | **0** | Every one traces to a `Column(...)`. Only the FTS5 table's 4 columns are model-less (see below) |
| Model columns missing from the baseline | **0** | — |
| Model-declared indexes | 79 | 79 present, same names, same columns, same order, same uniqueness |
| Helper-created indexes | 2 | `ix_sessions_last_message_at` (`:733`), `ux_email_accounts_one_default_per_owner` (`:772`) — both accounted for |
| Foreign keys | 13 across 11 tables | Identical `ondelete`/`onupdate` to the models; `calendar_events.calendar_id` has **no** `ondelete` in either, which is deliberate (§2.1) |
| Unique constraints | 4 | `gallery_images.filename`, `scheduled_tasks.webhook_token`, `user_tool_data(tool_id,key)`, the partial `ux_email_accounts_one_default_per_owner` |
| Triggers | 3 | `chat_messages_fts_ai/_ad/_au`, semantically identical to `database.py:2223-2241` including the `;base64,` / `data:image/` / `data:audio/` scrub |
| Baseline objects with **no** model | 6 | `chat_messages_fts` + `chat_messages_fts_{config,content,data,docsize,idx}` |
| `owner` columns | 18 of the 28 created tables | The other 9 are listed in §2.2; of the 19 tables the Python sweep names, 14 have `owner` and 5 are inert |

**Models matched with zero drift: 27 of 27 on the ORM-declared surface.** Not one column,
type, nullability, PK, FK, unique constraint or model-declared index differs between the
classes and the baseline. Every new finding in §2.2 therefore comes from one of three
places: the migration-helper path (which can build a different *shape* for the same
column), a comment, or a Python file that is not a model.

**Tables whose shape a helper can still change: 16 of 27** — the 72 columns added by
`ALTER TABLE ... ADD COLUMN` across `scheduled_tasks` (11), `model_endpoints` (11),
`calendar_events` (9), `sessions` (7), `documents` (7), `notes` (6), `email_accounts` (6),
`mcp_servers` (3), `gallery_images` (2), `crew_members` (2), `calendars` (2),
`api_tokens` (2), and 1 each in `user_tools`, `task_runs`, `memories`, `comparisons`. All
72 are present in the baseline; only the type spelling, the SQL `DEFAULT`, and the column
position differ, and those are the three rows above.

**Baseline objects with no model, stated explicitly:** `chat_messages_fts` is an FTS5
virtual table created by `_migrate_chat_messages_fts` (`database.py:2209-2241`), which
SQLAlchemy cannot express, so it has no ORM class and never will. Its five shadow tables
(`_config`, `_content`, `_data`, `_docsize`, `_idx`) are created by SQLite itself on first
use and must **not** appear in the baseline — `schema.rs` asserts both halves of that.
The only other `__tablename__` occurrences in the Python tree are two test-local
re-declarations of the existing `scheduled_tasks` / `task_runs` tables
(`tests/test_task_scheduler_cancel.py:13,23`, `tests/test_scheduler_restart_doublefire.py:37,49`),
which add nothing new.

**The 29-vs-27 `__tablename__` gap:** resolved. `database.py` contains 29 occurrences of
the string, but only **27** are declarations of the form `__tablename__ = "..."`, one per
model class. The other two are *attribute reads* — `EmailAccount.__tablename__` at
`database.py:1939` and `:2007`, used as a table-name argument to `inspect().has_table()`.
There is no 28th table hiding in that gap: the ORM has 27 tables, the baseline has 27
`CREATE TABLE` statements, and they correspond one-to-one.

The shape findings are pinned as a regression guard in `crates/store/tests/baseline_vs_models.rs`
(the FK inventory, the `scheduled_tasks` FKs, the absence of SQL `DEFAULT`s, the `owner`-less
set, the non-`AUTOINCREMENT` id, and the FTS5 object). The object *counts* are pinned
separately in `crates/store/src/schema.rs`.

---

## 3. Migration strategy

### 3.1 Single baseline (decision D6)

**Generate the baseline from the live `data/app.db` `sqlite_master`, not from the
SQLAlchemy models.** The live database is ground truth; the models have known drift
(§2.2).

Process:

1. Dump `SELECT type, name, sql FROM sqlite_master WHERE sql IS NOT NULL` from a real
   `app.db`.
2. Emit as ordered migration `0001_baseline.sql`.
3. Diff the emitted DDL against each SQLAlchemy model's expected columns; **record
   every discrepancy in a test fixture** so drift is visible forever.
4. Verify by opening a **copy** of the real database with the Rust binary and comparing
   `PRAGMA table_info` output for all 27 tables + the FTS5 virtual table.

### 3.2 Forward-only chain

```
0001_baseline            from live sqlite_master
0002_fts5_indexes        (created if absent — baseline may predate them)
0003_vector_tables       sqlite-vec virtual tables + rowid mapping
0004_indexes             any index present live but absent from baseline
```

Each must be idempotent, transactional, and safe to run on a fresh DB.

### 3.3 Hard requirements

| Requirement | Reason |
|---|---|
| **`PRAGMA foreign_keys=ON` on every connection** | Applied via a class-level `Engine` listener today (`database.py:142-147`). Must be a `rusqlite` connect hook. |
| **`chat_messages_fts` + 3 triggers** | Chat search depends on them. Triggers must keep scrubbing base64 media. |
| **FTS5 availability probe** | `database.py:2204-2208` guards creation with `temp._odysseus_fts5_probe`. Rust must verify `rusqlite` is built with FTS5 (spike S7) and degrade to LIKE search if not. |
| **`chat_messages.metadata` is TEXT, not JSON** | Holds serialised JSON, sometimes a *string that looks like* JSON. `session_manager.py:39-62` `_parse_msg_content` only deserialises when the string starts with `[{`, contains `"type"`, **and** every element's type is in the allowed set — protecting against a user pasting a JSON array. Port this heuristic exactly. |
| **Naive-UTC datetimes** | `utcnow_naive()` strips tzinfo. Rust must store naive UTC to match existing rows. |
| **`last_message_at` bumped only in `_persist_message`** | Deliberately not `onupdate` (`database.py:203-208`). |
| **File permissions 0600** | `safe_chmod` applied to `app.db` **and** its `-journal`/`-wal`/`-shm` sidecars (`database.py:2075-2102`). Rust must do the same. |
| **`PRAGMA busy_timeout`** | Required once `spawn_blocking` allows real concurrency. |
| **WAL mode** | Recommended; source does not set it. Note as an improvement. |

### 3.4 Data ownership invariant

`_migrate_assign_legacy_owner` (`database.py:1396-1517`) runs **at every startup and on
an hourly sweep**: blanket `UPDATE <table> SET owner=? WHERE owner IS NULL` across 19
tables, assigning the first `is_admin: true` user from `auth.json`. It also rewrites
`memory.json` and restructures `user_prefs.json`.

This exists because data created while auth was disabled or localhost-bypassed would
otherwise stay world-visible. **Port it**, but run it once at migration time rather than
hourly — the hourly sweep only exists to catch data created during a bypassed window,
which is not a Rust-app concern. Record as a deliberate change.

**The 19 entries are not 19 swept tables.** Only 14 of them have an `owner` column, and 5
owner-bearing tables are missing from the list — see the `owner`-sweep row in §2.2 before
porting this verbatim.

`_migrate_backfill_document_owner_from_session` (`database.py:1520`) **must run before**
the blanket sweep. Preserve ordering.

---

## 4. `data/scheduled_emails.db` — 15 raw tables

Hand-managed with raw `sqlite3`, **not** in the ORM. Opened in
`mcp_servers/email_server.py:1400-1407` and `routes/` helpers.

```
scheduled_emails(id PK, to_addr NOT NULL, cc, bcc, subject, body NOT NULL,
  in_reply_to, references_hdr, attachments, send_at NOT NULL, created_at NOT NULL,
  status NOT NULL DEFAULT 'pending', error, owner DEFAULT '', account_id,
  odysseus_kind)
  IDX ix_scheduled_emails_owner_status(owner,status)

email_urgency_alerts(message_id, owner, uid, folder, subject, sender, urgency,
  reason, alerted DEFAULT 0, created_at, PK(message_id, owner))
email_calendar_extractions(message_id, owner, uid, event_uids DEFAULT '[]',
  events_created DEFAULT 0, created_at, PK(message_id, owner))
email_tags(message_id, owner, account_id DEFAULT '', uid, folder, subject, sender,
  tags, spam_verdict DEFAULT 0, spam_reason, moved_to, model_used, created_at,
  PK(message_id, owner, account_id))
email_translations(body_hash, owner, target_language DEFAULT 'English', uid, folder,
  subject, sender, translation, same_language DEFAULT 0, model_used, created_at,
  PK(body_hash, owner, target_language))
email_ai_replies(message_id, owner, uid, folder, reply NOT NULL, model_used,
  created_at, PK(message_id, owner))
email_summaries(message_id, owner, uid, folder, subject, sender, summary NOT NULL,
  model_used, created_at, PK(message_id, owner))
sender_signatures(from_address, owner, signature_text, sample_count,
  last_built_at NOT NULL, model_used, source, PK(from_address, owner))
email_boundaries(message_id PK, uid, folder, sig_start, quote_start, model_used,
  created_at, turns_json)
email_attachment_metadata_cache(owner, account_key, folder, uid, message_id,
  attachments_json NOT NULL, updated_at NOT NULL, PK(owner,account_key,folder,uid))
email_body_preview_cache(owner, account_key, folder, uid, message_id,
  payload_json NOT NULL, updated_at NOT NULL, PK(owner,account_key,folder,uid))
  IDX ix_email_body_preview_message_id(owner,account_key,message_id)
email_message_index(owner, account_key, folder, uid, message_id, subject, from_name,
  from_address, to_text, cc_text, date_iso, date_display, date_epoch REAL DEFAULT 0,
  size INTEGER DEFAULT 0, flags DEFAULT '', has_attachments DEFAULT 0, updated_at,
  PK(owner,account_key,folder,uid))
  IDX ix_email_message_index_message_id(owner,account_key,message_id)
  IDX ix_email_message_index_folder_date(owner,account_key,folder,date_epoch DESC)
email_event_seen(owner, account_key, folder, message_key, first_seen_at,
  PK(owner,account_key,folder,message_key))
```

A third path `data/email_cache.db` is declared (`constants.py:39`) and referenced but
**does not currently exist**. Do not create it.

### 4.1 Decision required at Phase 15

| Option | Pros | Cons |
|---|---|---|
| **Fold into `app.db`** | One file, one backup, one migration chain, cross-DB queries become possible | Larger primary DB; 15 more tables in the baseline |
| **Keep separate** | Isolates email churn; smaller primary DB | Two migrations, two backups, no cross-DB queries |

**Recommendation:** fold in. The "local-first single data directory" story is simpler
with one file, and `backup_routes.py` already exports user data across stores.

**Risk regardless:** none of these tables have schema tests. Add tests before touching
them.

---

## 5. Vector storage: ChromaDB → `sqlite-vec`

### 5.1 Four collections to migrate

| Collection | Purpose | Built by | Query time |
|---|---|---|---|
| tool_index | Tool selection RAG | `tool_index.py:175` | 1.5 s timeout |
| rag | Personal-document RAG | `rag_vector.py` | `similarity >= 0.35` |
| memory | Semantic memory | `memory_vector.py` | `0.55*vector + 0.40*keyword + 0.05*recency` |
| embeddings lanes | Per-lane embedding collections | `embedding_lanes.py` | priority merge |

### 5.2 This is a replacement, not a port

ChromaDB and sqlite-vec use **different distance metrics**. Every calibrated value
below was tuned against ChromaDB and will not hold:

| Calibrated value | Location |
|---|---|
| Personal-doc similarity `>= 0.35` | `chat_processor.py` |
| Memory mix `0.55 / 0.40 / 0.05` | `chat_processor.py:158` |
| Category boosts identity 1.4×, contact 1.3×, preference 1.2× | `chat_processor.py:158` |
| Relevance gate `vs<0.20 and kw<0.08 → skip` | `chat_processor.py` |
| Tool-selection timeout 1.5 s | `tool_index.py` |
| Skill relevance threshold 0.25 | `agent_loop.py` |
| `skill_autosave_min_confidence = 0.85` | `settings.py` |
| `test_skill` confidence nudges .95/.6/.4 | `skills_routes.py` |
| Lane priority merge order | `embedding_lanes.py` `retrieve` |

**This is a gated task with golden-set validation, never hand-tuning** (decision D4a).

### 5.3 Golden-set protocol (spike S11)

1. Before switching, capture a **golden set** from the live ChromaDB: 200 queries ×
   top-10 results with scores, across all 4 collections.
2. Run the same queries against sqlite-vec with candidate threshold sets.
3. Choose the metric + thresholds that maximise top-10 agreement.
4. Record the chosen values as **constants with a comment citing this document**.
5. Keep the golden set as a test fixture so future changes are measurable.

### 5.4 Reindex strategy

The user chose "no separate server". Options:

| Option | Assessment |
|---|---|
| **Reindex from source** | Documents, memories, and tool descriptions all still exist. Cheapest correct path. |
| Export + re-embed | ChromaDB stores vectors; if the metric differs, they must be re-embedded anyway. **No advantage.** |

**Recommendation: full reindex.** The corpora are small (tool descriptions, personal
docs, memories). Exported vectors are useless across a metric change.

**UX during reindex:** retrieval features (RAG, semantic memory, tool selection) show a
"reindexing" state. The keyword fallback in `tool_index.py:3938-3946` is **already the
designed degradation path** — on any error or timeout, `_relevant_tools` stays `None` and
the keyword fallback runs. That means tool selection degrades gracefully, and this is
worth stating explicitly rather than treating reindex as a hard outage.

### 5.5 Historical note

This would be the **third** vector backend (there is a `test_migrate_faiss_to_chroma.py`
in the test suite). Flagged as a risk signal, not an objection — the ChromaDB service
being removed genuinely serves the single-process goal.

---

## 6. Encryption at rest

| Column | Table | Format |
|---|---|---|
| `model_endpoints.api_key` | Model endpoints | Fernet, `enc:` prefix |
| `provider_auth_sessions.access_token` | Copilot / ChatGPT | Fernet |
| `provider_auth_sessions.refresh_token` | Copilot / ChatGPT | Fernet |
| `mcp_servers.oauth_tokens` | MCP servers | Fernet (incl. DCR) |
| `signatures.data_png` | Signatures | Fernet |
| `signatures.svg` | Signatures | Fernet |
| `email_accounts.imap_password`, `oauth_access_token`, `oauth_refresh_token` | Email | Fernet via explicit migration |

Plus non-DB: `integrations.json` `api_key`, `vault.json`, `contacts.json` passwords,
CalDAV account passwords.

**Target:** the `fernet` crate, or AES-256-GCM with a compatible envelope. **Requirement:
`data/.app_key` must remain readable by the Rust binary** so existing installs keep
their secrets. Changing the key format orphans every encrypted column.

`is_encrypted()` idempotence is used by three migration functions
(`database.py:2326-2413`) — port the check so re-running is safe.

---

## 7. Filesystem layout

```
data/
├── app.db                     PRIMARY (rusqlite)
├── .app_key                   Fernet key — DO NOT ROTATE
├── auth.json                  users
├── sessions.json              browser tokens
├── settings.json              ~60 settings
├── features.json              8 toggles
├── user_prefs.json            _users: {user: {...}}
├── presets.json
├── integrations.json          keys encrypted
├── contacts.json              passwords encrypted
├── vault.json
├── cookbook_state.json
├── bg_jobs.json
├── memory_tidy_state.json
├── memory.json
├── logs/app.log               5 MB × 3, rotating
├── cache/{search,content}/    1,000 entries each, LRU
├── chroma/                    ← REMOVED (sqlite-vec replaces it)
├── skills/<category>/<name>/SKILL.md
├── skills/_usage.json
├── uploads/
├── uploads/COMPOSE_UPLOADS/_compose/
├── mail-attachments/
├── deep_research/{sid}.json
├── tts_cache/
├── mcp_oauth/
└── hwfit/*.json
```

Paths come from `core/constants.py` → `src/constants.py`. **All path constants are
preserved** so existing installs work unmodified.

`DATA_DIR` resolution: `runtime_paths.py` + `config.py::set_data_paths`, which
**always** rebuilds paths from `_DATA_DIR_CONST`, ignoring incoming values.

---

## 8. Client-side state

| Store | Key | Disposition |
|---|---|---|
| `odysseus-theme` | Theme object | RETAIN |
| `odysseus-ui-scale` | Text scale | RETAIN |
| `odysseus-sidebar-mode` | `full`/`mini`/`off` | RETAIN |
| `odysseus-toggles` | Chat-mode toggle blob | RETAIN |
| `odysseus-models-collapsed` | bool | RETAIN |
| `odysseus-model-favorites` | Shared by `models.js` + `modelPicker.js` | RETAIN |
| `odysseus-model-usage` | usage counters | RETAIN |
| `odysseus-model-sort` | sort order | RETAIN |
| `odysseus-model-recent` | last 5 picks | RETAIN |
| `odysseus-session-date-section-collapsed` | bool | RETAIN |
| `odysseus-research-dismissed` | id set | RETAIN |
| `odysseus-sensitive-blur` | pref | RETAIN |
| `odysseus-cookbook-bg-leader` | cross-tab leader election | RETAIN |
| `cal-*` (6 keys) | Calendar prefs | RETAIN |
| `odysseus-calendar-badge-seen`, `odysseus-calendar-cache` | Calendar state | RETAIN |
| sessionStorage: incognito set | Distinguish from localStorage | RETAIN |
| sessionStorage: `ody-prefetch-settings` | Login→app handoff | RETAIN |
| Cache API | 112 precache entries | REPLACED |
| In-memory `localStorage`-backed module state | ~150 singletons | REPLACED by signals |

Every `localStorage` access is try/catch-guarded today (private-browsing safety). The
Rust wrapper must preserve that.

---

## 9. Server state (in-memory, non-persistent)

| Store | Lifetime | Disposition |
|---|---|---|
| `SessionManager.sessions` dict | Process | Becomes an LRU metadata cache over SQLite. **Not** a source of truth. |
| `mcp_manager._connections/_tools/_sessions/_stacks` | Process | `McpManager` struct. |
| `mcp_manager._generation` | Process | `AtomicU64`. |
| `_token_cache` | Process | `RwLock<HashMap<prefix, Vec<Candidate>>>`. |
| `ToolApprovalStore` | 10 min TTL, max 2048 | TTL map + `Semaphore(1)` for one-shot claim. |
| `PendingDeviceFlowStore` | TTL-pruned | TTL map. |
| `agent_runs._RUNS` | Process | Detached run registry. |
| `chat_routes._active_streams` | Process | Same registry. |
| `_LOCAL_MODEL_WAITING_FOREGROUND` | Process | `AtomicUsize` + `Notify`. |
| Dead-host cooldown map | 20 s | `RwLock<HashMap<host, Instant>>`. |
| LLM response cache | Process | Bounded LRU keyed on credential identity. |
| `interactive_gate` counters | Process | `AtomicUsize` + `Condvar`. |
| Email caches (4 layers) | 8 s / 60 s / pool / prefetch | `moka` or hand-rolled TTL cache. |
| Model/provider caches | 30 s / 60 s / 24 h | TTL caches. |
| `settings` cache | 2 s TTL | `RwLock<Arc<Settings>>` + timestamp. |

---

## 10. Migration risks

| Risk | Severity | Mitigation |
|---|---|---|
| Baseline generated from models instead of the live DB | **High** | Generate from `sqlite_master`. Verify column-by-column against a real DB copy. |
| FTS5 not compiled into `rusqlite` | **High** | Spike S7. Degrade to LIKE search if absent. |
| sqlite-vec threshold drift | **High** | Golden-set protocol (§5.3). Gated task. |
| Vector reindex downtime | Medium | The keyword fallback is the existing designed degradation path. |
| `.app_key` incompatibility | **High** | Non-negotiable. Round-trip test with a real key. |
| `chat_messages.metadata` JSON heuristic | Medium | Port `_parse_msg_content` exactly, with tests. |
| `email_accounts` partial unique index | Medium | `COALESCE(owner,'')` in a partial index. Round-trip test. |
| 15 sidecar email tables untested | Medium | Add schema tests before touching. |
| Naive-UTC vs timezone-aware conversion | Medium | Store naive UTC. Round-trip test on every datetime column. |
| Owner sweep semantics change | Medium | Run once at migration; document. |
| File permission regression | Medium | `chmod 0600` on DB + sidecars; test. |
| Concurrent access without `busy_timeout` | Medium | Required once `spawn_blocking` introduces real concurrency. |

---

## 11. Data migration acceptance criteria

A data migration is done when:

1. A copy of a real `app.db` opens in the Rust binary without error.
2. `PRAGMA table_info` matches for all 27 tables + `chat_messages_fts`.
3. `PRAGMA index_list` matches for every table.
4. `PRAGMA foreign_key_check` returns zero rows.
5. Existing sessions, messages, documents, memories, notes, tasks, and calendar events
   are readable and owner-scoped correctly.
6. Login works against the real `auth.json` + `sessions.json`.
7. A previously-encrypted column decrypts with the real `.app_key`.
8. `data/settings.json` keys are honoured.
9. The golden set from §5.3 passes at the chosen thresholds.
10. File permissions on the DB and sidecars are 0600.
11. A golden-set vector query set meets an agreed top-10 agreement threshold.
12. Every encrypted column round-trips with the existing `.app_key`.