PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS credentials(
 id TEXT PRIMARY KEY, fingerprint TEXT UNIQUE, kind TEXT NOT NULL,
 account_hint TEXT, service TEXT, service_source TEXT NOT NULL,
 source_kind TEXT NOT NULL, source_ref TEXT NOT NULL, provided_by TEXT NOT NULL,
 status TEXT NOT NULL, first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL,
 last_used_at TEXT, expires_at TEXT, rotation_due_at TEXT, note TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS credential_source_kind ON credentials(source_ref,kind) WHERE fingerprint IS NULL;
CREATE TABLE IF NOT EXISTS credential_holders(
 credential_id TEXT NOT NULL REFERENCES credentials(id), holder_kind TEXT NOT NULL,
 holder_id TEXT NOT NULL, granted_at TEXT NOT NULL, granted_by TEXT NOT NULL,
 revoked_at TEXT, PRIMARY KEY(credential_id,holder_kind,holder_id)
);
CREATE TABLE IF NOT EXISTS detection_quota(sender TEXT NOT NULL, at INTEGER NOT NULL, count INTEGER NOT NULL, PRIMARY KEY(sender,at));
CREATE INDEX IF NOT EXISTS detection_quota_expiry ON detection_quota(at);
