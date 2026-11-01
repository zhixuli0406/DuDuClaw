PRAGMA foreign_keys=ON;
PRAGMA secure_delete=ON;
CREATE TABLE IF NOT EXISTS credentials(
 id TEXT PRIMARY KEY, fingerprint TEXT UNIQUE, kind TEXT NOT NULL,
 account_hint TEXT, service TEXT, service_source TEXT NOT NULL,
 source_kind TEXT NOT NULL, source_ref TEXT NOT NULL, provided_by TEXT NOT NULL,
 status TEXT NOT NULL, first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL,
 last_used_at TEXT, expires_at TEXT, rotation_due_at TEXT, note TEXT, note_source TEXT NOT NULL DEFAULT 'detected', source_ordinal INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS credential_source_kind ON credentials(source_ref,kind) WHERE fingerprint IS NULL;
CREATE TABLE IF NOT EXISTS credential_holders(
 credential_id TEXT NOT NULL REFERENCES credentials(id), holder_kind TEXT NOT NULL,
 holder_id TEXT NOT NULL, granted_at TEXT NOT NULL, granted_by TEXT NOT NULL,
 revoked_at TEXT, PRIMARY KEY(credential_id,holder_kind,holder_id)
);
CREATE TABLE IF NOT EXISTS detection_quota(sender TEXT NOT NULL, at INTEGER NOT NULL, count INTEGER NOT NULL, PRIMARY KEY(sender,at));
CREATE INDEX IF NOT EXISTS detection_quota_expiry ON detection_quota(at);
CREATE TRIGGER IF NOT EXISTS credential_holder_limit BEFORE INSERT ON credential_holders
WHEN NOT EXISTS(SELECT 1 FROM credential_holders WHERE credential_id=NEW.credential_id AND holder_kind=NEW.holder_kind AND holder_id=NEW.holder_id)
AND ((SELECT count(*) FROM credential_holders WHERE credential_id=NEW.credential_id)>=32
 OR (SELECT count(*) FROM credential_holders)>=200000)
BEGIN
 SELECT RAISE(ABORT, 'credential holder limit reached');
END;
CREATE TABLE IF NOT EXISTS credential_ingress_links(
 source_ref TEXT NOT NULL, credential_id TEXT NOT NULL REFERENCES credentials(id),
 at INTEGER NOT NULL, PRIMARY KEY(source_ref,credential_id)
);
CREATE INDEX IF NOT EXISTS credential_ingress_expiry ON credential_ingress_links(at);
