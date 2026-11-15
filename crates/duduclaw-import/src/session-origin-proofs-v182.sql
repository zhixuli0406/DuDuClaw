
-- v1.82 sessions.db addition: the gateway-owned turn-origin proof records of
-- schedules, reminders and goals (`recipient_guard::origin::SCHEMA`, run by
-- `SessionManager::init_tables` on every start of a fresh or upgraded store,
-- the text below must stay identical to it). Jobs store only the opaque
-- record id. The session reader never selects this table (its authorizer
-- denies everything it does not list), so v1.82 stores map as V181.
CREATE TABLE IF NOT EXISTS schedule_origin_proofs (id TEXT PRIMARY KEY NOT NULL, employee TEXT NOT NULL, recipient TEXT NOT NULL, creator_user_id TEXT NOT NULL, identity_channel TEXT NOT NULL, identity_user_id TEXT NOT NULL, conversation TEXT NOT NULL, generation TEXT NOT NULL, minted_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS idx_schedule_origin_proofs_employee ON schedule_origin_proofs(employee, minted_at);
