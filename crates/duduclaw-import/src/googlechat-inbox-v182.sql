            CREATE TABLE IF NOT EXISTS notifications (
                stream TEXT NOT NULL, event_key TEXT NOT NULL, payload TEXT,
                status TEXT NOT NULL, received_at INTEGER NOT NULL,
                PRIMARY KEY(stream,event_key));
            CREATE INDEX IF NOT EXISTS notifications_pending ON notifications(stream,status,received_at);
            CREATE INDEX IF NOT EXISTS notifications_age ON notifications(received_at);
