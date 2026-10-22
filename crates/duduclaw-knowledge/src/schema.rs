//! `knowledge.db` schema and connection setup.
//!
//! Opened with `foreign_keys=ON` (deleting a source cascades to its
//! segments, chunks, embeddings and jobs), `secure_delete=ON` (deleted text
//! does not linger in free pages), WAL and a 5 s busy timeout. The schema is
//! created idempotently and versioned in `kb_meta.schema_version`.
//!
//! `chunks.text` is the display text; the FTS index is built over the
//! matching form (`match_text`, `match_heading`, see [`crate::normalize`]).

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::KbResult;

/// Current schema version.
pub const SCHEMA_VERSION: i64 = 1;

/// `<home>/knowledge`.
pub fn knowledge_dir(home: &Path) -> PathBuf {
    home.join("knowledge")
}

/// `<home>/knowledge/knowledge.db`.
pub fn db_path(home: &Path) -> PathBuf {
    knowledge_dir(home).join("knowledge.db")
}

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS kb_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS collections (
  id            TEXT PRIMARY KEY,
  slug          TEXT NOT NULL UNIQUE,
  name          TEXT NOT NULL,
  description   TEXT NOT NULL DEFAULT '',
  department    TEXT,
  sensitivity   TEXT NOT NULL DEFAULT 'internal'
                CHECK (sensitivity IN ('public','internal','personal','restricted')),
  embedding_policy TEXT NOT NULL DEFAULT 'inherit'
                CHECK (embedding_policy IN ('inherit','local_only')),
  acl_revision  INTEGER NOT NULL DEFAULT 0,
  created_by    TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS collection_acl (
  collection_id  TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
  principal_kind TEXT NOT NULL CHECK (principal_kind IN ('user','agent','department','channel')),
  principal_id   TEXT NOT NULL,
  role           TEXT NOT NULL CHECK (role IN ('viewer','editor','owner')),
  granted_by     TEXT NOT NULL,
  granted_at     TEXT NOT NULL,
  PRIMARY KEY (collection_id, principal_kind, principal_id),
  CHECK (principal_kind NOT IN ('agent','channel') OR role = 'viewer'),
  CHECK (principal_kind <> 'department' OR role IN ('viewer','editor'))
);
CREATE INDEX IF NOT EXISTS idx_acl_principal ON collection_acl(principal_kind, principal_id);

CREATE TABLE IF NOT EXISTS sources (
  id            TEXT PRIMARY KEY,
  collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
  kind          TEXT NOT NULL CHECK (kind IN ('file','paste','url','attachment')),
  title         TEXT NOT NULL,
  original_name TEXT,
  url           TEXT,
  format        TEXT NOT NULL,
  blob_sha256   TEXT NOT NULL,
  content_sha256 TEXT,
  bytes         INTEGER NOT NULL DEFAULT 0,
  chars         INTEGER NOT NULL DEFAULT 0,
  pages         INTEGER,
  chunk_count   INTEGER NOT NULL DEFAULT 0,
  status        TEXT NOT NULL CHECK (status IN
                ('awaiting_approval','queued','extracting','indexing','embedding','ready','failed','deleting')),
  status_detail TEXT,
  error_code    TEXT,
  error_message TEXT,
  warnings_json TEXT NOT NULL DEFAULT '[]',
  origin_json   TEXT NOT NULL DEFAULT '{}',
  extractor_version TEXT,
  chunker_version TEXT,
  created_by    TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL,
  indexed_at    TEXT,
  UNIQUE (collection_id, blob_sha256)
);
CREATE INDEX IF NOT EXISTS idx_sources_collection ON sources(collection_id, status);

CREATE TABLE IF NOT EXISTS source_segments (
  source_id  TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  ord        INTEGER NOT NULL,
  kind       TEXT NOT NULL CHECK (kind IN ('page','slide','sheet','section','text')),
  label      TEXT,
  page       INTEGER,
  sheet      TEXT,
  row_start  INTEGER,
  row_end    INTEGER,
  char_start INTEGER NOT NULL,
  char_end   INTEGER NOT NULL,
  text       TEXT NOT NULL,
  PRIMARY KEY (source_id, ord)
);

CREATE TABLE IF NOT EXISTS chunks (
  id            INTEGER PRIMARY KEY,
  source_id     TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
  ord           INTEGER NOT NULL,
  text          TEXT NOT NULL,
  heading_path  TEXT NOT NULL DEFAULT '',
  locator_kind  TEXT NOT NULL CHECK (locator_kind IN ('page','slide','sheet_rows','section','paragraph','url')),
  page_start    INTEGER,
  page_end      INTEGER,
  sheet         TEXT,
  row_start     INTEGER,
  row_end       INTEGER,
  para_index    INTEGER,
  char_start    INTEGER NOT NULL,
  char_end      INTEGER NOT NULL,
  segment_ord   INTEGER NOT NULL,
  text_sha256   TEXT NOT NULL,
  match_text    TEXT NOT NULL,
  match_heading TEXT NOT NULL DEFAULT '',
  flags         INTEGER NOT NULL DEFAULT 0,
  UNIQUE (source_id, ord)
);
CREATE INDEX IF NOT EXISTS idx_chunks_collection ON chunks(collection_id);
CREATE INDEX IF NOT EXISTS idx_chunks_textsha ON chunks(text_sha256);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
  match_text, match_heading,
  content='chunks', content_rowid='id',
  tokenize='trigram remove_diacritics 1'
);

CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
  INSERT INTO chunks_fts(rowid, match_text, match_heading) VALUES (new.id, new.match_text, new.match_heading);
END;
CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
  INSERT INTO chunks_fts(chunks_fts, rowid, match_text, match_heading)
    VALUES ('delete', old.id, old.match_text, old.match_heading);
END;
CREATE TRIGGER IF NOT EXISTS chunks_au AFTER UPDATE ON chunks BEGIN
  INSERT INTO chunks_fts(chunks_fts, rowid, match_text, match_heading)
    VALUES ('delete', old.id, old.match_text, old.match_heading);
  INSERT INTO chunks_fts(rowid, match_text, match_heading) VALUES (new.id, new.match_text, new.match_heading);
END;

CREATE TABLE IF NOT EXISTS embeddings (
  chunk_id   INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
  model_id   TEXT NOT NULL,
  dim        INTEGER NOT NULL,
  vec        BLOB NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (chunk_id, model_id)
);
CREATE INDEX IF NOT EXISTS idx_emb_model ON embeddings(model_id);

CREATE TABLE IF NOT EXISTS ingest_jobs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  kind  TEXT NOT NULL CHECK (kind IN ('ingest','reindex','embed')),
  state TEXT NOT NULL CHECK (state IN ('queued','running','done','failed','cancelled')),
  model_id TEXT,
  attempts INTEGER NOT NULL DEFAULT 0,
  progress_done INTEGER NOT NULL DEFAULT 0,
  progress_total INTEGER NOT NULL DEFAULT 0,
  lease_until TEXT,
  error_code TEXT,
  error_message TEXT,
  created_at TEXT NOT NULL,
  started_at TEXT,
  finished_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_jobs_state ON ingest_jobs(state, id);

CREATE TABLE IF NOT EXISTS access_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at TEXT NOT NULL,
  op TEXT NOT NULL,
  collection_id TEXT,
  agent_id TEXT,
  requester_kind TEXT NOT NULL CHECK (requester_kind IN
    ('dashboard_user','channel_user','channel_unbound','system','unknown')),
  requester_id TEXT,
  channel TEXT,
  session_digest TEXT,
  turn_id TEXT,
  query TEXT,
  allowed_json TEXT NOT NULL DEFAULT '[]',
  results_json TEXT NOT NULL DEFAULT '[]',
  result_count INTEGER NOT NULL DEFAULT 0,
  mode TEXT,
  outcome TEXT NOT NULL CHECK (outcome IN ('ok','denied','error','no_match'))
);
CREATE INDEX IF NOT EXISTS idx_access_at ON access_log(at);
CREATE INDEX IF NOT EXISTS idx_access_collection ON access_log(collection_id, id);
"#;

/// Open (creating if needed) the database at `path` and bring the schema up
/// to [`SCHEMA_VERSION`].
pub fn open(path: &Path) -> KbResult<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Open the database under `<home>`.
pub fn open_home(home: &Path) -> KbResult<Connection> {
    open(&db_path(home))
}

/// Open the database under `<home>` read-only, without creating or
/// migrating anything. `Ok(None)` when it does not exist yet. For processes
/// that only decide visibility (the MCP server's tool listing).
pub fn open_read_only(home: &Path) -> KbResult<Option<Connection>> {
    let path = db_path(home);
    if !path.exists() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    Ok(Some(conn))
}

/// An in-memory database with the full schema (tests and the eval tool).
pub fn open_in_memory() -> KbResult<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> KbResult<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // `journal_mode` answers with a row; in-memory databases report "memory".
    let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    conn.execute_batch(
        "PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON; PRAGMA synchronous=NORMAL;",
    )?;
    Ok(())
}

/// Idempotent: creates whatever is missing and records the version.
pub fn migrate(conn: &Connection) -> KbResult<()> {
    conn.execute_batch(DDL)?;
    conn.execute_batch(crate::citations::DDL)?;
    let current: Option<String> = conn
        .query_row(
            "SELECT value FROM kb_meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match current.and_then(|v| v.parse::<i64>().ok()) {
        Some(v) if v > SCHEMA_VERSION => {
            return Err(crate::KbError::Invalid(format!(
                "知識庫資料庫版本 {v} 比這個 DuDuClaw 支援的版本新，請更新 DuDuClaw。"
            )));
        }
        Some(_) => {}
        None => {
            conn.execute(
                "INSERT INTO kb_meta(key, value) VALUES ('schema_version', ?1)",
                params![SCHEMA_VERSION.to_string()],
            )?;
            conn.execute(
                "INSERT OR IGNORE INTO kb_meta(key, value) VALUES ('generation', '0')",
                [],
            )?;
        }
    }
    Ok(())
}

/// Bump `kb_meta.generation` — any chunk or embedding change. Vector caches
/// (later releases) key on it.
pub fn bump_generation(conn: &Connection) -> KbResult<()> {
    conn.execute(
        "UPDATE kb_meta SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT) WHERE key='generation'",
        [],
    )?;
    Ok(())
}

/// Current generation counter.
pub fn generation(conn: &Connection) -> KbResult<i64> {
    let v: String = conn.query_row(
        "SELECT value FROM kb_meta WHERE key='generation'",
        [],
        |r| r.get(0),
    )?;
    Ok(v.parse().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_idempotent_and_versioned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k").join("knowledge.db");
        drop(open(&path).unwrap());
        let conn = open(&path).unwrap();
        migrate(&conn).unwrap();
        let v: String = conn
            .query_row(
                "SELECT value FROM kb_meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION.to_string());
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
        let sd: i64 = conn
            .query_row("PRAGMA secure_delete", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sd, 1);
    }

    #[test]
    fn bundled_sqlite_supports_trigram_with_remove_diacritics() {
        let conn = open_in_memory().unwrap();
        let version: String = conn
            .query_row("SELECT sqlite_version()", [], |r| r.get(0))
            .unwrap();
        let parts: Vec<u32> = version.split('.').filter_map(|p| p.parse().ok()).collect();
        assert!(
            parts.len() >= 2 && (parts[0], parts[1]) >= (3, 45),
            "sqlite {version}"
        );
        // The virtual table above was created with the tokenizer options, so
        // reaching here proves they parse; a match proves trigram works on CJK.
        conn.execute_batch(
            "INSERT INTO kb_meta(key,value) VALUES('x','y');
             CREATE VIRTUAL TABLE t USING fts5(x, tokenize='trigram remove_diacritics 1');
             INSERT INTO t(x) VALUES ('員工請假規定與特休天數');",
        )
        .unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM t WHERE t MATCH '\"特休天\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn newer_schema_is_refused() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "UPDATE kb_meta SET value='99' WHERE key='schema_version'",
            [],
        )
        .unwrap();
        assert!(migrate(&conn).is_err());
    }

    #[test]
    fn generation_bumps() {
        let conn = open_in_memory().unwrap();
        assert_eq!(generation(&conn).unwrap(), 0);
        bump_generation(&conn).unwrap();
        assert_eq!(generation(&conn).unwrap(), 1);
    }
}
