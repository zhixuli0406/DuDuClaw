ALTER TABLE pending_placeholders ADD COLUMN state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','cleanup_only'));
