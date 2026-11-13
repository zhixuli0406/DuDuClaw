-- Reviewed v1.82 MCP key store schema (mcp_keys.db, RM-134): a byte copy of duduclaw_core::mcp_key_store::SCHEMA_SQL.
-- duduclaw-auth's importer_catalog_matches_a_fresh_store test compares it with a fresh McpKeyStore.
CREATE TABLE IF NOT EXISTS mcp_api_keys (
    id TEXT PRIMARY KEY,
    key_hash TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    client_id TEXT NOT NULL,
    is_external INTEGER NOT NULL CHECK (is_external IN (0, 1)),
    env TEXT NOT NULL CHECK (env IN ('prod', 'staging', 'dev')),
    hint TEXT NOT NULL,
    scopes TEXT NOT NULL,
    created_by TEXT,
    created_at TEXT NOT NULL,
    expires_at TEXT,
    disabled_at TEXT,
    disabled_by TEXT,
    deleted_at TEXT,
    deleted_by TEXT,
    migrated INTEGER NOT NULL DEFAULT 0 CHECK (migrated IN (0, 1)),
    expiry_reviewed INTEGER NOT NULL DEFAULT 1 CHECK (expiry_reviewed IN (0, 1)),
    legacy_entry_hash TEXT,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    agent_binding TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_mcp_api_keys_name
    ON mcp_api_keys(name COLLATE NOCASE) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_mcp_api_keys_expiry
    ON mcp_api_keys(expires_at) WHERE deleted_at IS NULL AND disabled_at IS NULL;
CREATE TABLE IF NOT EXISTS mcp_api_key_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_internal_keys (
    key_hash TEXT PRIMARY KEY,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_legacy_pending (
    key_hash TEXT PRIMARY KEY,
    recorded_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_legacy_ignored (
    key_hash TEXT PRIMARY KEY,
    hint TEXT NOT NULL,
    first_seen_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_api_key_notices (
    key_id TEXT NOT NULL,
    threshold TEXT NOT NULL CHECK (threshold IN ('7d', '1d')),
    expires_at TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'delivered', 'closed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    claimed_until TEXT,
    next_attempt_at TEXT,
    finished_at TEXT,
    claim_token TEXT,
    PRIMARY KEY (key_id, threshold, expires_at)
);
PRAGMA user_version = 1;
