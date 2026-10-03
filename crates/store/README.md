# odysseus-store

## Concern

SQLite persistence, plus the two JSON side-files that hold credentials. It owns
everything that touches the `data/` directory on disk:

| Module | Owns |
|---|---|
| `schema` | the 27-table baseline migration, generated from the **live** `data/app.db` `sqlite_master` rather than the Python ORM models (decision D6), and the idempotent migration chain |
| `pool` | per-connection pragmas, applied on every connection as the Python `Engine` listener did |
| `blocking` | the only sanctioned path from async code to `rusqlite`, which is synchronous (AGENTS.md D11) |
| `crypto` | Fernet encryption at rest for the six columns Python wrapped in a `TypeDecorator`; `data/.app_key` must stay readable |
| `atomic` | crash-safe JSON writes (temp file, `fsync`, `rename`, mode 0600) |
| `auth` | users, signup flag, privileges, TOTP state, and browser sessions — these live in `data/auth.json` and `data/sessions.json`, **not** in SQLite, because the Python schema has no `users` table |
| `api_tokens` | bearer tokens for external integrations, bcrypt-hashed with a prefix-indexed cache |
| `totp` | the second-factor logic, pinned to pyotp's SHA1/6-digit/30s parameters |

Integration tests live in `crates/store/tests/` (`baseline`, `auth_compat`,
`totp_flows`) and run against a real temporary SQLite database, never a mock.

## Must not import

- `axum`
- `leptos`
- `reqwest`

Source of truth for this list: the architecture table in `AGENTS.md` §8.