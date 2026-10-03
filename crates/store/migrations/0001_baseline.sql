-- 0001_baseline.sql
--
-- GENERATED from the live `data/app.db` sqlite_master (task T0043).
-- Decision D6: the live database is ground truth, not the Python
-- SQLAlchemy models, which have known drift.
--
-- Source: data/app.db (path varies per install)
-- Tables: 33
--
-- Regenerate with: cargo run -p dump-baseline -- <app.db> <out-dir>

-- ---- TABLE api_tokens ----
CREATE TABLE api_tokens (
	id VARCHAR NOT NULL, 
	owner VARCHAR, 
	name VARCHAR NOT NULL, 
	token_hash VARCHAR NOT NULL, 
	token_prefix VARCHAR NOT NULL, 
	scopes VARCHAR NOT NULL, 
	is_active BOOLEAN, 
	last_used_at DATETIME, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE caldav_deleted_events ----
CREATE TABLE caldav_deleted_events (
	uid VARCHAR NOT NULL, 
	owner VARCHAR, 
	calendar_id VARCHAR, 
	remote_href VARCHAR, 
	remote_etag VARCHAR, 
	caldav_base_url VARCHAR, 
	summary VARCHAR, 
	last_error TEXT, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (uid)
);

-- ---- TABLE calendar_events ----
CREATE TABLE calendar_events (
	uid VARCHAR NOT NULL, 
	calendar_id VARCHAR NOT NULL, 
	summary VARCHAR NOT NULL, 
	description TEXT, 
	location VARCHAR, 
	dtstart DATETIME NOT NULL, 
	dtend DATETIME NOT NULL, 
	all_day BOOLEAN, 
	is_utc BOOLEAN NOT NULL, 
	rrule VARCHAR, 
	recurrence_exdates TEXT, 
	color VARCHAR, 
	status VARCHAR, 
	importance VARCHAR, 
	event_type VARCHAR, 
	last_pinged DATETIME, 
	origin VARCHAR, 
	remote_href VARCHAR, 
	remote_etag VARCHAR, 
	caldav_sync_pending VARCHAR, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (uid), 
	FOREIGN KEY(calendar_id) REFERENCES calendars (id)
);

-- ---- TABLE calendars ----
CREATE TABLE calendars (
	id VARCHAR NOT NULL, 
	owner VARCHAR, 
	name VARCHAR NOT NULL, 
	color VARCHAR, 
	source VARCHAR, 
	account_id VARCHAR, 
	caldav_base_url VARCHAR, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE chat_messages ----
CREATE TABLE chat_messages (
	id VARCHAR NOT NULL, 
	session_id VARCHAR NOT NULL, 
	role VARCHAR NOT NULL, 
	content TEXT NOT NULL, 
	metadata TEXT, 
	timestamp DATETIME, 
	PRIMARY KEY (id), 
	FOREIGN KEY(session_id) REFERENCES sessions (id) ON DELETE CASCADE
);

-- ---- TABLE comparisons ----
CREATE TABLE comparisons (
	id VARCHAR NOT NULL, 
	session_id VARCHAR, 
	owner VARCHAR, 
	prompt TEXT NOT NULL, 
	model_a VARCHAR NOT NULL, 
	model_b VARCHAR NOT NULL, 
	endpoint_a VARCHAR NOT NULL, 
	endpoint_b VARCHAR NOT NULL, 
	response_a TEXT, 
	response_b TEXT, 
	metrics_a TEXT, 
	metrics_b TEXT, 
	winner VARCHAR, 
	is_blind BOOLEAN, 
	blind_mapping TEXT, 
	voted_at DATETIME, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE crew_members ----
CREATE TABLE crew_members (
	id VARCHAR NOT NULL, 
	owner VARCHAR, 
	name VARCHAR NOT NULL, 
	avatar VARCHAR, 
	user_name VARCHAR, 
	personality TEXT, 
	model VARCHAR, 
	endpoint_url VARCHAR, 
	greeting TEXT, 
	enabled_tools TEXT, 
	session_id VARCHAR, 
	is_active BOOLEAN, 
	sort_order INTEGER, 
	is_default_assistant BOOLEAN, 
	timezone VARCHAR, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(session_id) REFERENCES sessions (id) ON DELETE SET NULL
);

-- ---- TABLE document_versions ----
CREATE TABLE document_versions (
	id VARCHAR NOT NULL, 
	document_id VARCHAR NOT NULL, 
	version_number INTEGER NOT NULL, 
	content TEXT NOT NULL, 
	summary VARCHAR, 
	source VARCHAR, 
	created_at DATETIME, 
	PRIMARY KEY (id), 
	FOREIGN KEY(document_id) REFERENCES documents (id) ON DELETE CASCADE
);

-- ---- TABLE documents ----
CREATE TABLE documents (
	id VARCHAR NOT NULL, 
	session_id VARCHAR, 
	title VARCHAR NOT NULL, 
	language VARCHAR, 
	current_content TEXT NOT NULL, 
	version_count INTEGER, 
	is_active BOOLEAN, 
	archived BOOLEAN, 
	owner VARCHAR, 
	tidy_verdict VARCHAR, 
	source_email_uid VARCHAR, 
	source_email_folder VARCHAR, 
	source_email_account_id VARCHAR, 
	source_email_message_id VARCHAR, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(session_id) REFERENCES sessions (id) ON DELETE SET NULL
);

-- ---- TABLE editor_drafts ----
CREATE TABLE editor_drafts (
	id VARCHAR NOT NULL, 
	owner VARCHAR, 
	name VARCHAR NOT NULL, 
	source_image_id VARCHAR, 
	width INTEGER, 
	height INTEGER, 
	payload TEXT NOT NULL, 
	thumbnail TEXT, 
	is_active BOOLEAN, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE email_account_owner_locks ----
CREATE TABLE email_account_owner_locks (
	owner_key VARCHAR NOT NULL, 
	PRIMARY KEY (owner_key)
);

-- ---- TABLE email_accounts ----
CREATE TABLE email_accounts (
	id VARCHAR NOT NULL, 
	owner VARCHAR, 
	name VARCHAR NOT NULL, 
	is_default BOOLEAN NOT NULL, 
	enabled BOOLEAN NOT NULL, 
	imap_host VARCHAR, 
	imap_port INTEGER, 
	imap_user VARCHAR, 
	imap_password VARCHAR, 
	imap_starttls BOOLEAN, 
	smtp_host VARCHAR, 
	smtp_port INTEGER, 
	smtp_security VARCHAR, 
	smtp_user VARCHAR, 
	smtp_password VARCHAR, 
	from_address VARCHAR, 
	display_name VARCHAR, 
	oauth_provider VARCHAR, 
	oauth_access_token VARCHAR, 
	oauth_refresh_token VARCHAR, 
	oauth_token_expiry VARCHAR, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE gallery_albums ----
CREATE TABLE gallery_albums (
	id VARCHAR NOT NULL, 
	name VARCHAR NOT NULL, 
	description TEXT, 
	cover_id VARCHAR, 
	owner VARCHAR, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE gallery_images ----
CREATE TABLE gallery_images (
	id VARCHAR NOT NULL, 
	filename VARCHAR NOT NULL, 
	prompt TEXT NOT NULL, 
	caption TEXT, 
	model VARCHAR, 
	size VARCHAR, 
	quality VARCHAR, 
	tags VARCHAR, 
	ai_tags TEXT, 
	session_id VARCHAR, 
	album_id VARCHAR, 
	owner VARCHAR, 
	is_active BOOLEAN, 
	favorite BOOLEAN, 
	file_hash VARCHAR(64), 
	taken_at DATETIME, 
	camera_make VARCHAR, 
	camera_model VARCHAR, 
	gps_lat VARCHAR, 
	gps_lng VARCHAR, 
	width INTEGER, 
	height INTEGER, 
	file_size INTEGER, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	UNIQUE (filename), 
	FOREIGN KEY(session_id) REFERENCES sessions (id) ON DELETE SET NULL, 
	FOREIGN KEY(album_id) REFERENCES gallery_albums (id) ON DELETE SET NULL
);

-- ---- TABLE integrations ----
CREATE TABLE integrations (
	id VARCHAR NOT NULL, 
	owner VARCHAR, 
	name VARCHAR NOT NULL, 
	type VARCHAR NOT NULL, 
	config JSON, 
	enabled BOOLEAN, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE mcp_servers ----
CREATE TABLE mcp_servers (
	id VARCHAR NOT NULL, 
	name VARCHAR NOT NULL, 
	transport VARCHAR NOT NULL, 
	command VARCHAR, 
	args TEXT, 
	env TEXT, 
	url VARCHAR, 
	is_enabled BOOLEAN, 
	oauth_config TEXT, 
	disabled_tools TEXT, 
	oauth_tokens TEXT, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE memories ----
CREATE TABLE memories (
	id VARCHAR NOT NULL, 
	text TEXT NOT NULL, 
	category VARCHAR, 
	source VARCHAR, 
	owner VARCHAR, 
	session_id VARCHAR, 
	timestamp INTEGER, 
	PRIMARY KEY (id), 
	FOREIGN KEY(session_id) REFERENCES sessions (id) ON DELETE SET NULL
);

-- ---- TABLE model_endpoints ----
CREATE TABLE model_endpoints (
	id VARCHAR NOT NULL, 
	name VARCHAR NOT NULL, 
	base_url VARCHAR NOT NULL, 
	api_key TEXT, 
	is_enabled BOOLEAN, 
	hidden_models TEXT, 
	cached_models TEXT, 
	pinned_models TEXT, 
	model_type VARCHAR, 
	endpoint_kind VARCHAR, 
	model_refresh_mode VARCHAR, 
	model_refresh_interval INTEGER, 
	model_refresh_timeout INTEGER, 
	supports_tools BOOLEAN, 
	owner VARCHAR, 
	provider_auth_id VARCHAR, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE notes ----
CREATE TABLE notes (
	id VARCHAR NOT NULL, 
	owner VARCHAR, 
	title VARCHAR, 
	content TEXT, 
	items TEXT, 
	note_type VARCHAR, 
	color VARCHAR, 
	label VARCHAR, 
	pinned BOOLEAN, 
	archived BOOLEAN, 
	due_date VARCHAR, 
	source VARCHAR, 
	session_id VARCHAR, 
	sort_order INTEGER, 
	image_url VARCHAR, 
	repeat VARCHAR, 
	ai_classification TEXT, 
	ai_content_hash VARCHAR, 
	agent_session_id VARCHAR, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE provider_auth_sessions ----
CREATE TABLE provider_auth_sessions (
	id VARCHAR NOT NULL, 
	provider VARCHAR NOT NULL, 
	owner VARCHAR, 
	label VARCHAR, 
	base_url VARCHAR NOT NULL, 
	access_token TEXT, 
	refresh_token TEXT, 
	last_refresh DATETIME, 
	auth_mode VARCHAR, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE scheduled_tasks ----
CREATE TABLE scheduled_tasks (
	id VARCHAR NOT NULL, 
	owner VARCHAR, 
	name VARCHAR NOT NULL, 
	prompt TEXT, 
	task_type VARCHAR, 
	action VARCHAR, 
	schedule VARCHAR, 
	scheduled_time VARCHAR, 
	scheduled_day INTEGER, 
	scheduled_date DATETIME, 
	trigger_type VARCHAR, 
	trigger_event VARCHAR, 
	trigger_count INTEGER, 
	trigger_counter INTEGER, 
	next_run DATETIME, 
	last_run DATETIME, 
	status VARCHAR, 
	output_target VARCHAR, 
	session_id VARCHAR, 
	model VARCHAR, 
	endpoint_url VARCHAR, 
	run_count INTEGER, 
	cron_expression VARCHAR, 
	then_task_id VARCHAR, 
	webhook_token VARCHAR, 
	crew_member_id VARCHAR, 
	character_id VARCHAR, 
	max_steps INTEGER, 
	email_results BOOLEAN, 
	notifications_enabled BOOLEAN, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(session_id) REFERENCES sessions (id) ON DELETE SET NULL, 
	FOREIGN KEY(then_task_id) REFERENCES scheduled_tasks (id) ON DELETE SET NULL, 
	UNIQUE (webhook_token)
);

-- ---- TABLE sessions ----
CREATE TABLE sessions (
	id VARCHAR NOT NULL, 
	name VARCHAR NOT NULL, 
	endpoint_url VARCHAR NOT NULL, 
	model VARCHAR NOT NULL, 
	owner VARCHAR, 
	rag BOOLEAN, 
	archived BOOLEAN, 
	folder VARCHAR, 
	headers JSON, 
	last_accessed DATETIME, 
	last_message_at DATETIME, 
	is_important BOOLEAN, 
	message_count INTEGER, 
	total_input_tokens INTEGER, 
	total_output_tokens INTEGER, 
	mode VARCHAR, 
	crew_member_id VARCHAR, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE signatures ----
CREATE TABLE signatures (
	id VARCHAR NOT NULL, 
	owner VARCHAR, 
	name VARCHAR NOT NULL, 
	data_png TEXT NOT NULL, 
	width INTEGER, 
	height INTEGER, 
	svg TEXT, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE task_runs ----
CREATE TABLE task_runs (
	id VARCHAR NOT NULL, 
	task_id VARCHAR NOT NULL, 
	started_at DATETIME NOT NULL, 
	finished_at DATETIME, 
	status VARCHAR, 
	result TEXT, 
	error TEXT, 
	tokens_used INTEGER, 
	steps TEXT, 
	model VARCHAR, 
	PRIMARY KEY (id), 
	FOREIGN KEY(task_id) REFERENCES scheduled_tasks (id) ON DELETE CASCADE
);

-- ---- TABLE user_tool_data ----
CREATE TABLE user_tool_data (
	id INTEGER NOT NULL, 
	tool_id VARCHAR NOT NULL, 
	"key" VARCHAR NOT NULL, 
	value TEXT, 
	created_at DATETIME, 
	updated_at DATETIME, 
	PRIMARY KEY (id), 
	FOREIGN KEY(tool_id) REFERENCES user_tools (id) ON DELETE CASCADE
);

-- ---- TABLE user_tools ----
CREATE TABLE user_tools (
	id VARCHAR NOT NULL, 
	name VARCHAR NOT NULL, 
	description TEXT, 
	icon VARCHAR, 
	html_content TEXT NOT NULL, 
	scope VARCHAR NOT NULL, 
	session_id VARCHAR, 
	owner VARCHAR, 
	is_pinned BOOLEAN, 
	is_active BOOLEAN, 
	version INTEGER, 
	author VARCHAR, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(session_id) REFERENCES sessions (id) ON DELETE SET NULL
);

-- ---- TABLE webhooks ----
CREATE TABLE webhooks (
	id VARCHAR NOT NULL, 
	name VARCHAR NOT NULL, 
	url VARCHAR NOT NULL, 
	secret VARCHAR, 
	events VARCHAR NOT NULL, 
	is_active BOOLEAN, 
	last_triggered_at DATETIME, 
	last_status_code INTEGER, 
	last_error VARCHAR, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id)
);

-- ---- TABLE chat_messages_fts ----
CREATE VIRTUAL TABLE chat_messages_fts USING fts5(
                content,
                message_id UNINDEXED,
                session_id UNINDEXED,
                role UNINDEXED
            );

-- ---- INDEX ix_api_tokens_id ----
CREATE INDEX ix_api_tokens_id ON api_tokens (id);

-- ---- INDEX ix_api_tokens_owner ----
CREATE INDEX ix_api_tokens_owner ON api_tokens (owner);

-- ---- INDEX ix_caldav_deleted_events_calendar_id ----
CREATE INDEX ix_caldav_deleted_events_calendar_id ON caldav_deleted_events (calendar_id);

-- ---- INDEX ix_caldav_deleted_events_owner ----
CREATE INDEX ix_caldav_deleted_events_owner ON caldav_deleted_events (owner);

-- ---- INDEX ix_caldav_deleted_events_uid ----
CREATE INDEX ix_caldav_deleted_events_uid ON caldav_deleted_events (uid);

-- ---- INDEX ix_calendar_events_calendar_id ----
CREATE INDEX ix_calendar_events_calendar_id ON calendar_events (calendar_id);

-- ---- INDEX ix_calendar_events_dtstart ----
CREATE INDEX ix_calendar_events_dtstart ON calendar_events (dtstart);

-- ---- INDEX ix_calendar_events_origin ----
CREATE INDEX ix_calendar_events_origin ON calendar_events (origin);

-- ---- INDEX ix_calendar_events_uid ----
CREATE INDEX ix_calendar_events_uid ON calendar_events (uid);

-- ---- INDEX ix_calendars_account_id ----
CREATE INDEX ix_calendars_account_id ON calendars (account_id);

-- ---- INDEX ix_calendars_id ----
CREATE INDEX ix_calendars_id ON calendars (id);

-- ---- INDEX ix_calendars_owner ----
CREATE INDEX ix_calendars_owner ON calendars (owner);

-- ---- INDEX ix_chat_messages_id ----
CREATE INDEX ix_chat_messages_id ON chat_messages (id);

-- ---- INDEX ix_chat_messages_session_id ----
CREATE INDEX ix_chat_messages_session_id ON chat_messages (session_id);

-- ---- INDEX ix_comparisons_id ----
CREATE INDEX ix_comparisons_id ON comparisons (id);

-- ---- INDEX ix_comparisons_owner ----
CREATE INDEX ix_comparisons_owner ON comparisons (owner);

-- ---- INDEX ix_comparisons_voted_at ----
CREATE INDEX ix_comparisons_voted_at ON comparisons (voted_at);

-- ---- INDEX ix_crew_members_id ----
CREATE INDEX ix_crew_members_id ON crew_members (id);

-- ---- INDEX ix_crew_members_owner ----
CREATE INDEX ix_crew_members_owner ON crew_members (owner);

-- ---- INDEX ix_document_versions_document_id ----
CREATE INDEX ix_document_versions_document_id ON document_versions (document_id);

-- ---- INDEX ix_document_versions_id ----
CREATE INDEX ix_document_versions_id ON document_versions (id);

-- ---- INDEX ix_documents_id ----
CREATE INDEX ix_documents_id ON documents (id);

-- ---- INDEX ix_documents_owner ----
CREATE INDEX ix_documents_owner ON documents (owner);

-- ---- INDEX ix_documents_session_id ----
CREATE INDEX ix_documents_session_id ON documents (session_id);

-- ---- INDEX ix_documents_source_email_message_id ----
CREATE INDEX ix_documents_source_email_message_id ON documents (source_email_message_id);

-- ---- INDEX ix_editor_drafts_id ----
CREATE INDEX ix_editor_drafts_id ON editor_drafts (id);

-- ---- INDEX ix_editor_drafts_owner ----
CREATE INDEX ix_editor_drafts_owner ON editor_drafts (owner);

-- ---- INDEX ix_editor_drafts_owner_updated ----
CREATE INDEX ix_editor_drafts_owner_updated ON editor_drafts (owner, is_active, updated_at);

-- ---- INDEX ix_editor_drafts_source_image_id ----
CREATE INDEX ix_editor_drafts_source_image_id ON editor_drafts (source_image_id);

-- ---- INDEX ix_email_accounts_id ----
CREATE INDEX ix_email_accounts_id ON email_accounts (id);

-- ---- INDEX ix_email_accounts_owner ----
CREATE INDEX ix_email_accounts_owner ON email_accounts (owner);

-- ---- INDEX ix_email_accounts_owner_default ----
CREATE INDEX ix_email_accounts_owner_default ON email_accounts (owner, is_default);

-- ---- INDEX ix_gallery_albums_id ----
CREATE INDEX ix_gallery_albums_id ON gallery_albums (id);

-- ---- INDEX ix_gallery_albums_owner ----
CREATE INDEX ix_gallery_albums_owner ON gallery_albums (owner);

-- ---- INDEX ix_gallery_images_active ----
CREATE INDEX ix_gallery_images_active ON gallery_images (is_active, created_at);

-- ---- INDEX ix_gallery_images_album_id ----
CREATE INDEX ix_gallery_images_album_id ON gallery_images (album_id);

-- ---- INDEX ix_gallery_images_file_hash ----
CREATE INDEX ix_gallery_images_file_hash ON gallery_images (file_hash);

-- ---- INDEX ix_gallery_images_id ----
CREATE INDEX ix_gallery_images_id ON gallery_images (id);

-- ---- INDEX ix_gallery_images_model ----
CREATE INDEX ix_gallery_images_model ON gallery_images (model);

-- ---- INDEX ix_gallery_images_owner ----
CREATE INDEX ix_gallery_images_owner ON gallery_images (owner);

-- ---- INDEX ix_gallery_images_session_id ----
CREATE INDEX ix_gallery_images_session_id ON gallery_images (session_id);

-- ---- INDEX ix_gallery_images_tags ----
CREATE INDEX ix_gallery_images_tags ON gallery_images (tags);

-- ---- INDEX ix_gallery_images_taken_at ----
CREATE INDEX ix_gallery_images_taken_at ON gallery_images (taken_at);

-- ---- INDEX ix_integrations_id ----
CREATE INDEX ix_integrations_id ON integrations (id);

-- ---- INDEX ix_integrations_owner ----
CREATE INDEX ix_integrations_owner ON integrations (owner);

-- ---- INDEX ix_mcp_servers_id ----
CREATE INDEX ix_mcp_servers_id ON mcp_servers (id);

-- ---- INDEX ix_memories_id ----
CREATE INDEX ix_memories_id ON memories (id);

-- ---- INDEX ix_memories_lookup ----
CREATE INDEX ix_memories_lookup ON memories (category, timestamp);

-- ---- INDEX ix_memories_owner ----
CREATE INDEX ix_memories_owner ON memories (owner);

-- ---- INDEX ix_memories_session ----
CREATE INDEX ix_memories_session ON memories (session_id, timestamp);

-- ---- INDEX ix_memories_session_id ----
CREATE INDEX ix_memories_session_id ON memories (session_id);

-- ---- INDEX ix_messages_session_time ----
CREATE INDEX ix_messages_session_time ON chat_messages (session_id, timestamp);

-- ---- INDEX ix_model_endpoints_id ----
CREATE INDEX ix_model_endpoints_id ON model_endpoints (id);

-- ---- INDEX ix_model_endpoints_owner ----
CREATE INDEX ix_model_endpoints_owner ON model_endpoints (owner);

-- ---- INDEX ix_model_endpoints_provider_auth_id ----
CREATE INDEX ix_model_endpoints_provider_auth_id ON model_endpoints (provider_auth_id);

-- ---- INDEX ix_notes_id ----
CREATE INDEX ix_notes_id ON notes (id);

-- ---- INDEX ix_notes_owner ----
CREATE INDEX ix_notes_owner ON notes (owner);

-- ---- INDEX ix_provider_auth_sessions_id ----
CREATE INDEX ix_provider_auth_sessions_id ON provider_auth_sessions (id);

-- ---- INDEX ix_provider_auth_sessions_owner ----
CREATE INDEX ix_provider_auth_sessions_owner ON provider_auth_sessions (owner);

-- ---- INDEX ix_provider_auth_sessions_provider ----
CREATE INDEX ix_provider_auth_sessions_provider ON provider_auth_sessions (provider);

-- ---- INDEX ix_scheduled_tasks_due ----
CREATE INDEX ix_scheduled_tasks_due ON scheduled_tasks (status, next_run);

-- ---- INDEX ix_scheduled_tasks_event ----
CREATE INDEX ix_scheduled_tasks_event ON scheduled_tasks (trigger_type, trigger_event, status);

-- ---- INDEX ix_scheduled_tasks_id ----
CREATE INDEX ix_scheduled_tasks_id ON scheduled_tasks (id);

-- ---- INDEX ix_scheduled_tasks_next_run ----
CREATE INDEX ix_scheduled_tasks_next_run ON scheduled_tasks (next_run);

-- ---- INDEX ix_scheduled_tasks_owner ----
CREATE INDEX ix_scheduled_tasks_owner ON scheduled_tasks (owner);

-- ---- INDEX ix_sessions_active ----
CREATE INDEX ix_sessions_active ON sessions (archived, last_accessed);

-- ---- INDEX ix_sessions_id ----
CREATE INDEX ix_sessions_id ON sessions (id);

-- ---- INDEX ix_sessions_last_message_at ----
CREATE INDEX ix_sessions_last_message_at ON sessions(archived, last_message_at);

-- ---- INDEX ix_sessions_owner ----
CREATE INDEX ix_sessions_owner ON sessions (owner);

-- ---- INDEX ix_sessions_search ----
CREATE INDEX ix_sessions_search ON sessions (name, archived);

-- ---- INDEX ix_signatures_id ----
CREATE INDEX ix_signatures_id ON signatures (id);

-- ---- INDEX ix_signatures_owner ----
CREATE INDEX ix_signatures_owner ON signatures (owner);

-- ---- INDEX ix_task_runs_id ----
CREATE INDEX ix_task_runs_id ON task_runs (id);

-- ---- INDEX ix_task_runs_task ----
CREATE INDEX ix_task_runs_task ON task_runs (task_id, started_at);

-- ---- INDEX ix_user_tool_data_tool_key ----
CREATE UNIQUE INDEX ix_user_tool_data_tool_key ON user_tool_data (tool_id, "key");

-- ---- INDEX ix_user_tools_active ----
CREATE INDEX ix_user_tools_active ON user_tools (is_active);

-- ---- INDEX ix_user_tools_id ----
CREATE INDEX ix_user_tools_id ON user_tools (id);

-- ---- INDEX ix_user_tools_owner ----
CREATE INDEX ix_user_tools_owner ON user_tools (owner);

-- ---- INDEX ix_user_tools_scope ----
CREATE INDEX ix_user_tools_scope ON user_tools (scope);

-- ---- INDEX ix_webhooks_id ----
CREATE INDEX ix_webhooks_id ON webhooks (id);

-- ---- INDEX ux_email_accounts_one_default_per_owner ----
CREATE UNIQUE INDEX ux_email_accounts_one_default_per_owner ON email_accounts (COALESCE(owner, '')) WHERE is_default = 1;

-- ---- TRIGGER chat_messages_fts_ad ----
CREATE TRIGGER chat_messages_fts_ad
            AFTER DELETE ON chat_messages BEGIN
                DELETE FROM chat_messages_fts WHERE message_id = old.id;
            END;

-- ---- TRIGGER chat_messages_fts_ai ----
CREATE TRIGGER chat_messages_fts_ai
            AFTER INSERT ON chat_messages BEGIN
                INSERT INTO chat_messages_fts(content, message_id, session_id, role)
                VALUES (CASE WHEN instr(COALESCE(new.content, ''), ';base64,') > 0 OR instr(COALESCE(new.content, ''), 'data:image/') > 0 OR instr(COALESCE(new.content, ''), 'data:audio/') > 0 THEN '[inline media omitted from search index]' ELSE COALESCE(new.content, '') END, new.id, new.session_id, new.role);
            END;

-- ---- TRIGGER chat_messages_fts_au ----
CREATE TRIGGER chat_messages_fts_au
            AFTER UPDATE ON chat_messages BEGIN
                DELETE FROM chat_messages_fts WHERE message_id = old.id;
                INSERT INTO chat_messages_fts(content, message_id, session_id, role)
                VALUES (CASE WHEN instr(COALESCE(new.content, ''), ';base64,') > 0 OR instr(COALESCE(new.content, ''), 'data:image/') > 0 OR instr(COALESCE(new.content, ''), 'data:audio/') > 0 THEN '[inline media omitted from search index]' ELSE COALESCE(new.content, '') END, new.id, new.session_id, new.role);
            END;

