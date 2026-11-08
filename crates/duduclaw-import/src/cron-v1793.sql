-- Exact CronStore initializer and ordered additive columns, v1.79.3.
PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS cron_tasks (
                 id              TEXT PRIMARY KEY,
                 name            TEXT NOT NULL,
                 agent_id        TEXT NOT NULL DEFAULT 'default',
                 cron            TEXT NOT NULL,
                 task            TEXT NOT NULL,
                 enabled         INTEGER NOT NULL DEFAULT 1,
                 created_at      TEXT NOT NULL,
                 updated_at      TEXT NOT NULL,
                 last_run_at     TEXT,
                 last_status     TEXT,
                 last_error      TEXT,
                 run_count       INTEGER NOT NULL DEFAULT 0,
                 failure_count   INTEGER NOT NULL DEFAULT 0
             );

             CREATE INDEX IF NOT EXISTS idx_cron_tasks_enabled ON cron_tasks(enabled);
             CREATE INDEX IF NOT EXISTS idx_cron_tasks_name    ON cron_tasks(name);

CREATE TABLE IF NOT EXISTS cron_workflow_bindings(cron_id TEXT PRIMARY KEY,
            activation_id TEXT NOT NULL,
            material_hash TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS cron_routine_cursors(cron_id TEXT NOT NULL,
            material_hash TEXT NOT NULL,cursor_unix INTEGER NOT NULL,PRIMARY KEY(cron_id,material_hash));
            CREATE TABLE IF NOT EXISTS cron_routine_skips(cron_id TEXT NOT NULL,material_hash TEXT NOT NULL,
            from_unix INTEGER NOT NULL,to_unix INTEGER NOT NULL,slots INTEGER NOT NULL,recorded_at TEXT NOT NULL,
            PRIMARY KEY(cron_id,material_hash,from_unix));
            -- Enabling a routine starts it from now: occurrences while it was
            -- disabled are not run late. Covers every writer of `enabled`.
            CREATE TRIGGER IF NOT EXISTS cron_routine_cursor_on_enable AFTER UPDATE OF enabled ON cron_tasks
            WHEN OLD.enabled=0 AND NEW.enabled=1 BEGIN
            UPDATE cron_routine_cursors SET cursor_unix=MAX(cursor_unix,CAST(strftime('%s','now') AS INTEGER))
            WHERE cron_id=NEW.id; END;
ALTER TABLE cron_tasks ADD COLUMN notify_channel TEXT;
ALTER TABLE cron_tasks ADD COLUMN notify_chat_id TEXT;
ALTER TABLE cron_tasks ADD COLUMN notify_thread_id TEXT;
ALTER TABLE cron_tasks ADD COLUMN cron_timezone TEXT;
ALTER TABLE cron_tasks ADD COLUMN trigger_kind TEXT;
ALTER TABLE cron_tasks ADD COLUMN condition_script TEXT;
ALTER TABLE cron_tasks ADD COLUMN condition_state TEXT;
ALTER TABLE cron_tasks ADD COLUMN watch_command TEXT;
ALTER TABLE cron_tasks ADD COLUMN on_behalf_of TEXT;
