-- 0002_pending_device_flows.sql
--
-- Pending OAuth device-flow sessions (T0308).
--
-- Why this is a table rather than an in-memory map: a device flow can sit
-- pending for up to `expires_in` (900s upstream), and the user may reload the
-- settings page or the server may be restarted while the browser is polling.
-- An in-memory store loses the flow on restart, which reads to the user as
-- "GitHub said my code was wrong" when in fact the server forgot it.
--
-- The `payload` column holds provider-side secrets -- the GitHub `device_code`,
-- the ChatGPT `device_auth_id`, and a PKCE `code_verifier` where one is used.
-- Any of those lets the holder complete the flow and obtain a token for the
-- user's provider account, so:
--
--   * the column is TEXT and the values are opaque to every reader except the
--     poll path, which is the only code that selects it;
--   * nothing here is ever serialized toward the browser. The browser-facing
--     response type carries `poll_id` plus the `user_code` the user must type,
--     and that type has no field capable of holding a `device_code`. See
--     `oauth_device_flow::BrowserDeviceStart` and the structural test that
--     asserts it.
--
-- `owner` follows the same convention as `model_endpoints`: NULL means the flow
-- is not scoped to a single user, which is how a system-wide admin login is
-- recorded. It is carried so a poll can be attributed, not used to widen access --
-- the poll route is admin-gated at the web layer regardless.

CREATE TABLE pending_device_flows (
	id VARCHAR NOT NULL,
	provider VARCHAR NOT NULL,
	owner VARCHAR,
	payload TEXT NOT NULL,
	interval_seconds INTEGER NOT NULL,
	expires_at DATETIME NOT NULL,
	next_poll_at DATETIME NOT NULL,
	created_at DATETIME NOT NULL,
	PRIMARY KEY (id)
);

-- The poll path looks an entry up by primary key and prunes by expiry, so the
-- only index worth having is on expiry: pruning scans for expired rows on every
-- add, and without this it is a full table scan.
CREATE INDEX ix_pending_device_flows_expires_at ON pending_device_flows (expires_at);