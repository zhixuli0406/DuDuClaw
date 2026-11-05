-- Exact ChannelSettingsManager initializer, colocated with sessions.db.
CREATE TABLE IF NOT EXISTS channel_settings (
                channel_type TEXT NOT NULL,
                scope_id TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (channel_type, scope_id, key)
            );

            CREATE INDEX IF NOT EXISTS idx_channel_settings_scope
                ON channel_settings(channel_type, scope_id);
