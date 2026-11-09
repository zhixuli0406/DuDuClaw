PRAGMA journal_mode=WAL;
            PRAGMA busy_timeout=5000;
            PRAGMA foreign_keys=ON;

            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                summary TEXT DEFAULT '',
                total_tokens INTEGER DEFAULT 0,
                last_active TEXT NOT NULL,
                model TEXT DEFAULT 'auto',
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS session_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                tokens INTEGER DEFAULT 0,
                timestamp TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session
                ON session_messages(session_id);

            CREATE TABLE IF NOT EXISTS webchat_turn_cancellations (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                turn_id TEXT NOT NULL,
                msg_id TEXT NOT NULL,
                cancelled_at TEXT NOT NULL,
                PRIMARY KEY(session_id, turn_id)
            );

            CREATE INDEX IF NOT EXISTS idx_webchat_cancellations_age
                ON webchat_turn_cancellations(cancelled_at);
            CREATE INDEX IF NOT EXISTS idx_webchat_cancellations_session_age
                ON webchat_turn_cancellations(session_id, cancelled_at DESC, turn_id DESC);

            CREATE TABLE IF NOT EXISTS session_ccr_references (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                user_message_id INTEGER NOT NULL,
                ccr_id TEXT NOT NULL,
                record_json TEXT NOT NULL,
                expires_at INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE(session_id, user_message_id, ccr_id)
            );
            CREATE INDEX IF NOT EXISTS idx_session_ccr_references_session
                ON session_ccr_references(session_id, id);;

ALTER TABLE session_messages ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;

ALTER TABLE session_messages ADD COLUMN notice_kind TEXT NULL;

ALTER TABLE sessions ADD COLUMN pinned_instructions TEXT DEFAULT '';

ALTER TABLE sessions ADD COLUMN summary_of_prior TEXT;

ALTER TABLE sessions ADD COLUMN summarized_through_turn INTEGER NOT NULL DEFAULT 0;

ALTER TABLE sessions ADD COLUMN last_summarized_at TEXT;

ALTER TABLE sessions ADD COLUMN archived_at TEXT;

ALTER TABLE session_messages ADD COLUMN undone_at TEXT;

ALTER TABLE sessions ADD COLUMN lineage INTEGER NOT NULL DEFAULT 1;

ALTER TABLE sessions ADD COLUMN checkpoint_message_id INTEGER NOT NULL DEFAULT 0;

ALTER TABLE sessions ADD COLUMN title TEXT;

ALTER TABLE sessions ADD COLUMN titled_through_turn INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_sessions_last_active
                ON sessions(agent_id, last_active);

CREATE TABLE IF NOT EXISTS message_session_map (
                channel TEXT NOT NULL,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (channel, message_id)
            );;

ALTER TABLE sessions ADD COLUMN origin_kind TEXT;

ALTER TABLE sessions ADD COLUMN owner_user_id TEXT;

ALTER TABLE sessions ADD COLUMN origin_at TEXT;

CREATE INDEX IF NOT EXISTS idx_sessions_agent_origin
            ON sessions(agent_id, origin_kind, owner_user_id, last_active);
