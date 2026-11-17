            CREATE TABLE IF NOT EXISTS pending_placeholders (
                message_name TEXT PRIMARY KEY NOT NULL, space TEXT NOT NULL,
                app_agent TEXT, app_email TEXT NOT NULL, employee_id TEXT NOT NULL,
                session_id TEXT NOT NULL, thread_name TEXT, created_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS pending_placeholders_age ON pending_placeholders(created_at);
