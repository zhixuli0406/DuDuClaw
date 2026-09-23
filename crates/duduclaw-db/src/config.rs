//! `config.toml [db_sources.<name>]` — parsing, validation, and credential
//! resolution.
//!
//! ```toml
//! [db_sources.crm_pg]
//! label = "客戶 CRM 資料庫"
//! driver = "postgres"                       # postgres | mysql | sqlite
//! url = "secret://vault/crm-dsn"            # or url_enc = "<ciphertext>"
//! allowed_tables = ["customers", "orders"]  # required, non-empty; ["*"] = all
//! max_rows = 200                            # default 200, hard cap 1000
//! timeout_ms = 10000                        # default 10s
//! ```
//!
//! The `url` field goes through the project's credentials doctrine
//! ([`duduclaw_security::secret_ref`]), so `secret://…` references and the
//! `url_enc` encrypted twin both resolve. A **plaintext** `url` is accepted
//! only for `driver = "sqlite"`, where the value is a filesystem path and not
//! a credential; a plaintext PostgreSQL / MySQL DSN is refused at load with a
//! message pointing at `secret://` or the dashboard, because a DSN carries a
//! password and config.toml is not a secret store.
//!
//! Validation is per source and fail-closed: a source that does not validate
//! is **excluded** (never silently repaired) and its reason is reported
//! alongside the ones that loaded, so the dashboard can show the operator what
//! is broken without taking the working sources down with it.

use std::path::Path;

use duduclaw_security::secret_ref::{Secret, SecretRef, SecretStatus};

use crate::{
    DbError, DbSource, DbSourceConfig, Driver, clamp_max_rows, clamp_timeout_ms, ident,
};

/// Longest `[db_sources.<name>]` key accepted.
pub const MAX_SOURCE_NAME_LEN: usize = 64;
/// Most sources one config may declare.
pub const MAX_SOURCES: usize = 64;
/// Most entries in one `allowed_tables` list.
pub const MAX_ALLOWED_TABLES: usize = 500;

/// One declared source, with its credential kept as a reference (never a
/// resolved value).
#[derive(Clone)]
pub struct DbSourceEntry {
    pub name: String,
    pub label: String,
    pub driver: Driver,
    pub allowed_tables: Vec<String>,
    pub max_rows: usize,
    pub timeout_ms: u64,
    url_ref: SecretRef,
}

impl std::fmt::Debug for DbSourceEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `SecretRef`'s own Debug is safe for references and ciphertext, but a
        // legacy plaintext SQLite path would still print. Keep the whole field
        // out and render the non-secret status instead.
        f.debug_struct("DbSourceEntry")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("driver", &self.driver)
            .field("allowed_tables", &self.allowed_tables)
            .field("max_rows", &self.max_rows)
            .field("timeout_ms", &self.timeout_ms)
            .field("url_status", &self.url_status())
            .finish()
    }
}

impl DbSourceEntry {
    /// Does this source permit free-form SQL (`db_query`)?
    ///
    /// Only a wildcard `allowed_tables = ["*"]` does. Exposed on the entry so
    /// the MCP handler can refuse **before** opening a connection — see
    /// [`crate::DbError::FreeSqlNotAllowed`].
    pub fn allows_all_tables(&self) -> bool {
        self.allowed_tables
            .iter()
            .any(|t| t.trim() == crate::ALLOW_ALL_TABLES)
    }

    /// Non-secret description of where the connection string comes from —
    /// what `db_sources.list` renders. Never resolves, never calls a backend.
    pub fn url_status(&self) -> SecretStatus {
        self.url_ref.describe()
    }

    /// Resolve the connection string. Async because a `secret://vault/…`
    /// reference needs a backend round-trip.
    pub async fn resolve_url(&self, home_dir: &Path) -> Result<Secret, DbError> {
        let sm_cfg =
            duduclaw_security::secret_manager::SecretManagerConfig::load_from_home(home_dir).await;
        self.url_ref
            .resolve(&sm_cfg, home_dir)
            .await
            .ok_or_else(|| {
                DbError::Config(format!(
                    "資料來源「{}」的連線字串無法解析（未設定、金鑰檔遺失，或 secret:// 後端取不到值）",
                    self.name
                ))
            })
    }
}

/// Why one `[db_sources.<name>]` block was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbSourceLoadError {
    /// The source key, or `config.toml` when the whole file failed to parse.
    pub name: String,
    pub message: String,
}

/// Outcome of reading `[db_sources]`: what loaded, and what did not.
#[derive(Debug, Clone, Default)]
pub struct LoadedDbSources {
    pub sources: Vec<DbSourceEntry>,
    pub errors: Vec<DbSourceLoadError>,
}

impl LoadedDbSources {
    /// Look one source up by name (exact, trimmed, ASCII-case-insensitive —
    /// never a substring test).
    pub fn get(&self, name: &str) -> Option<&DbSourceEntry> {
        let want = name.trim();
        self.sources
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(want))
    }

    pub fn names(&self) -> Vec<String> {
        self.sources.iter().map(|s| s.name.clone()).collect()
    }
}

/// `^[a-z][a-z0-9_]*$` — the `[db_sources.<name>]` key contract.
pub fn is_valid_source_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_SOURCE_NAME_LEN {
        return false;
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Parse `[db_sources]` out of an already-parsed `config.toml` table.
///
/// Pure: no file I/O, no credential resolution. This is the function the unit
/// tests exercise and the one the gateway's upsert path reuses to re-validate
/// what it is about to write.
pub fn parse_db_sources(config: &toml::Table) -> LoadedDbSources {
    let mut out = LoadedDbSources::default();
    let Some(section) = config.get("db_sources") else {
        return out;
    };
    let Some(table) = section.as_table() else {
        out.errors.push(DbSourceLoadError {
            name: "db_sources".into(),
            message: "[db_sources] 必須是一個表格（table）".into(),
        });
        return out;
    };

    for (name, value) in table {
        if out.sources.len() + out.errors.len() >= MAX_SOURCES {
            out.errors.push(DbSourceLoadError {
                name: name.clone(),
                message: format!("資料來源數量超過上限（{MAX_SOURCES}）"),
            });
            break;
        }
        match parse_one(name, value) {
            Ok(entry) => out.sources.push(entry),
            Err(message) => out.errors.push(DbSourceLoadError {
                name: name.clone(),
                message,
            }),
        }
    }
    out.sources.sort_by(|a, b| a.name.cmp(&b.name));
    out.errors.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn parse_one(name: &str, value: &toml::Value) -> Result<DbSourceEntry, String> {
    if !is_valid_source_name(name) {
        return Err(format!(
            "資料來源名稱「{name}」不合法（只允許小寫英文字母開頭，之後是小寫字母、數字或底線，最長 {MAX_SOURCE_NAME_LEN} 字元）"
        ));
    }
    let t = value
        .as_table()
        .ok_or_else(|| format!("[db_sources.{name}] 必須是一個表格（table）"))?;

    let driver_raw = t
        .get("driver")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("[db_sources.{name}] 缺少 driver（postgres / mysql / sqlite）"))?;
    let driver = Driver::parse(driver_raw).ok_or_else(|| {
        format!("[db_sources.{name}] driver「{driver_raw}」不支援，只接受 postgres / mysql / sqlite")
    })?;

    let url_enc = t.get("url_enc").and_then(|v| v.as_str());
    let url_plain = t.get("url").and_then(|v| v.as_str());

    // Plaintext DSN rule: only SQLite (a path, not a credential) may carry one.
    if let Some(u) = url_plain.map(str::trim).filter(|s| !s.is_empty())
        && !u.starts_with("secret://")
        && driver != Driver::Sqlite
    {
        return Err(format!(
            "[db_sources.{name}] 的 url 是明文連線字串。{} 的連線字串含密碼，請改用 secret://<backend>/<name> 參照，或在儀表板「資料來源」表單儲存（會加密成 url_enc）。",
            driver.as_str()
        ));
    }

    let url_ref = SecretRef::classify(url_enc, url_plain);
    if !url_ref.describe().configured {
        return Err(format!(
            "[db_sources.{name}] 沒有可用的連線字串（url 或 url_enc 至少要有一個）"
        ));
    }

    let allowed_tables = parse_allowed_tables(name, t)?;

    let label = t
        .get("label")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(name)
        .to_string();

    Ok(DbSourceEntry {
        name: name.to_string(),
        label,
        driver,
        allowed_tables,
        max_rows: clamp_max_rows(t.get("max_rows").and_then(|v| v.as_integer())),
        timeout_ms: clamp_timeout_ms(t.get("timeout_ms").and_then(|v| v.as_integer())),
        url_ref,
    })
}

fn parse_allowed_tables(name: &str, t: &toml::Table) -> Result<Vec<String>, String> {
    let arr = t
        .get("allowed_tables")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            format!(
                "[db_sources.{name}] 缺少 allowed_tables（必填，例如 [\"customers\"]；[\"*\"] 代表全部）"
            )
        })?;
    if arr.is_empty() {
        return Err(format!(
            "[db_sources.{name}] 的 allowed_tables 不可為空陣列（空白代表沒有任何資料表可讀，請明確列出或使用 [\"*\"]）"
        ));
    }
    if arr.len() > MAX_ALLOWED_TABLES {
        return Err(format!(
            "[db_sources.{name}] 的 allowed_tables 過長（{} 項，上限 {MAX_ALLOWED_TABLES} 項）",
            arr.len()
        ));
    }
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item.as_str().map(str::trim).ok_or_else(|| {
            format!("[db_sources.{name}] 的 allowed_tables 只能包含字串")
        })?;
        if s == crate::ALLOW_ALL_TABLES {
            out.push(s.to_string());
            continue;
        }
        if !ident::is_valid_identifier(s) {
            return Err(format!(
                "[db_sources.{name}] 的 allowed_tables 含不合法的資料表名稱「{s}」（僅允許英數字與底線，且不可數字開頭；或使用 \"*\"）"
            ));
        }
        out.push(s.to_string());
    }
    Ok(out)
}

/// Read `<home>/config.toml` and parse `[db_sources]`.
///
/// A missing file is not an error (no sources configured). A file that exists
/// but does not parse **is** one, reported rather than degraded to "no
/// sources" — silently losing every source because of an unrelated typo
/// elsewhere in config.toml would look identical to "the operator removed
/// them".
pub async fn load_db_sources(home_dir: &Path) -> LoadedDbSources {
    let path = home_dir.join("config.toml");
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LoadedDbSources::default(),
        Err(e) => {
            return LoadedDbSources {
                sources: Vec::new(),
                errors: vec![DbSourceLoadError {
                    name: "config.toml".into(),
                    message: format!("設定檔讀取失敗：{e}"),
                }],
            };
        }
    };
    match content.parse::<toml::Table>() {
        Ok(table) => parse_db_sources(&table),
        Err(e) => LoadedDbSources {
            sources: Vec::new(),
            errors: vec![DbSourceLoadError {
                name: "config.toml".into(),
                message: format!("設定檔解析失敗：{e}"),
            }],
        },
    }
}

/// Resolve `entry`'s connection string. Thin wrapper kept for symmetry with
/// [`open_source`]; prefer [`DbSourceEntry::resolve_url`] at call sites that
/// already hold the entry.
pub async fn resolve_source_url(
    entry: &DbSourceEntry,
    home_dir: &Path,
) -> Result<Secret, DbError> {
    entry.resolve_url(home_dir).await
}

/// Resolve `entry`'s credential and open a pool.
pub async fn open_source(entry: &DbSourceEntry, home_dir: &Path) -> Result<DbSource, DbError> {
    let url = entry.resolve_url(home_dir).await?;
    let cfg = DbSourceConfig {
        name: entry.name.clone(),
        label: entry.label.clone(),
        driver: entry.driver,
        url: url.expose_owned(),
        allowed_tables: entry.allowed_tables.clone(),
        max_rows: entry.max_rows,
        timeout_ms: entry.timeout_ms,
    };
    DbSource::connect(cfg).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_str: &str) -> LoadedDbSources {
        let table: toml::Table = toml_str.parse().expect("test toml must parse");
        parse_db_sources(&table)
    }

    #[test]
    fn no_section_is_not_an_error() {
        let loaded = parse("[settings]\nfoo = 1\n");
        assert!(loaded.sources.is_empty());
        assert!(loaded.errors.is_empty());
    }

    #[test]
    fn parses_sqlite_with_plaintext_path() {
        let loaded = parse(
            r#"
[db_sources.demo]
label = "示範"
driver = "sqlite"
url = "/tmp/demo.sqlite"
allowed_tables = ["customers"]
"#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let s = loaded.get("demo").expect("demo source");
        assert_eq!(s.driver, Driver::Sqlite);
        assert_eq!(s.label, "示範");
        assert_eq!(s.allowed_tables, vec!["customers".to_string()]);
        assert_eq!(s.max_rows, crate::DEFAULT_MAX_ROWS);
        assert_eq!(s.timeout_ms, crate::DEFAULT_TIMEOUT_MS);
        assert!(s.url_status().configured);
    }

    #[test]
    fn refuses_plaintext_dsn_for_postgres_and_mysql() {
        for driver in ["postgres", "mysql"] {
            let loaded = parse(&format!(
                r#"
[db_sources.crm]
driver = "{driver}"
url = "{driver}://user:pw@db.internal/app"
allowed_tables = ["customers"]
"#
            ));
            assert!(loaded.sources.is_empty(), "{driver} must not load");
            let err = &loaded.errors[0];
            assert_eq!(err.name, "crm");
            assert!(err.message.contains("secret://"), "{}", err.message);
        }
    }

    #[test]
    fn accepts_secret_reference_for_postgres() {
        let loaded = parse(
            r#"
[db_sources.crm]
driver = "postgres"
url = "secret://env/CRM_DSN"
allowed_tables = ["*"]
"#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let s = loaded.get("crm").unwrap();
        assert!(s.url_status().configured);
        assert_eq!(
            s.url_status().source,
            duduclaw_security::secret_ref::SourceKind::Env
        );
    }

    #[test]
    fn accepts_url_enc_for_postgres() {
        let loaded = parse(
            r#"
[db_sources.crm]
driver = "postgres"
url_enc = "c29tZS1jaXBoZXJ0ZXh0"
allowed_tables = ["customers"]
"#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(
            loaded.get("crm").unwrap().url_status().source,
            duduclaw_security::secret_ref::SourceKind::Inline
        );
    }

    #[test]
    fn rejects_missing_url_driver_and_allowed_tables() {
        let loaded = parse(
            r#"
[db_sources.a]
driver = "sqlite"
allowed_tables = ["t"]

[db_sources.b]
url = "/tmp/x.sqlite"
allowed_tables = ["t"]

[db_sources.c]
driver = "sqlite"
url = "/tmp/x.sqlite"
"#,
        );
        assert!(loaded.sources.is_empty(), "{:?}", loaded.sources);
        assert_eq!(loaded.errors.len(), 3);
        assert!(loaded.errors[0].message.contains("連線字串"));
        assert!(loaded.errors[1].message.contains("driver"));
        assert!(loaded.errors[2].message.contains("allowed_tables"));
    }

    #[test]
    fn rejects_empty_allowed_tables() {
        let loaded = parse(
            r#"
[db_sources.a]
driver = "sqlite"
url = "/tmp/x.sqlite"
allowed_tables = []
"#,
        );
        assert!(loaded.sources.is_empty());
        assert!(loaded.errors[0].message.contains("空陣列"));
    }

    #[test]
    fn rejects_injection_shaped_table_names() {
        let loaded = parse(
            r#"
[db_sources.a]
driver = "sqlite"
url = "/tmp/x.sqlite"
allowed_tables = ["customers; DROP TABLE x"]
"#,
        );
        assert!(loaded.sources.is_empty());
        assert!(loaded.errors[0].message.contains("不合法"));
    }

    #[test]
    fn rejects_bad_source_name_and_unknown_driver() {
        let loaded = parse(
            r#"
[db_sources."Bad-Name"]
driver = "sqlite"
url = "/tmp/x.sqlite"
allowed_tables = ["t"]

[db_sources.ora]
driver = "oracle"
url = "secret://env/X"
allowed_tables = ["t"]
"#,
        );
        assert!(loaded.sources.is_empty());
        assert_eq!(loaded.errors.len(), 2);
        assert!(loaded.errors.iter().any(|e| e.name == "Bad-Name"));
        assert!(loaded.errors.iter().any(|e| e.message.contains("oracle")));
    }

    #[test]
    fn caps_max_rows_and_timeout() {
        let loaded = parse(
            r#"
[db_sources.a]
driver = "sqlite"
url = "/tmp/x.sqlite"
allowed_tables = ["*"]
max_rows = 99999
timeout_ms = 1
"#,
        );
        let s = loaded.get("a").unwrap();
        assert_eq!(s.max_rows, crate::MAX_ROWS_CAP);
        assert_eq!(s.timeout_ms, crate::MIN_TIMEOUT_MS);
    }

    #[test]
    fn one_broken_source_does_not_take_down_the_good_ones() {
        let loaded = parse(
            r#"
[db_sources.good]
driver = "sqlite"
url = "/tmp/x.sqlite"
allowed_tables = ["t"]

[db_sources.bad]
driver = "postgres"
url = "postgres://u:p@h/db"
allowed_tables = ["t"]
"#,
        );
        assert_eq!(loaded.names(), vec!["good".to_string()]);
        assert_eq!(loaded.errors.len(), 1);
        assert_eq!(loaded.errors[0].name, "bad");
    }

    #[test]
    fn lookup_is_exact_not_substring() {
        let loaded = parse(
            r#"
[db_sources.crm]
driver = "sqlite"
url = "/tmp/x.sqlite"
allowed_tables = ["t"]
"#,
        );
        assert!(loaded.get("crm").is_some());
        assert!(loaded.get("CRM").is_some(), "ASCII-case-insensitive exact");
        assert!(loaded.get("cr").is_none());
        assert!(loaded.get("crm_prod").is_none());
    }

    #[test]
    fn debug_never_prints_the_connection_string() {
        let loaded = parse(
            r#"
[db_sources.demo]
driver = "sqlite"
url = "/very/secret/path/demo.sqlite"
allowed_tables = ["t"]
"#,
        );
        let rendered = format!("{:?}", loaded.get("demo").unwrap());
        assert!(!rendered.contains("/very/secret/path"), "{rendered}");
    }

    #[test]
    fn source_name_rules() {
        assert!(is_valid_source_name("crm_pg"));
        assert!(is_valid_source_name("a1"));
        assert!(!is_valid_source_name("A"));
        assert!(!is_valid_source_name("1a"));
        assert!(!is_valid_source_name("a-b"));
        assert!(!is_valid_source_name(""));
        assert!(!is_valid_source_name(&"a".repeat(MAX_SOURCE_NAME_LEN + 1)));
    }

    #[tokio::test]
    async fn missing_config_file_loads_nothing_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_db_sources(dir.path()).await;
        assert!(loaded.sources.is_empty());
        assert!(loaded.errors.is_empty());
    }

    #[tokio::test]
    async fn malformed_config_file_is_reported_not_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "this is not = = toml").unwrap();
        let loaded = load_db_sources(dir.path()).await;
        assert!(loaded.sources.is_empty());
        assert_eq!(loaded.errors[0].name, "config.toml");
    }

    #[tokio::test]
    async fn loads_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[db_sources.demo]\ndriver = \"sqlite\"\nurl = \"/tmp/demo.sqlite\"\nallowed_tables = [\"customers\"]\n",
        )
        .unwrap();
        let loaded = load_db_sources(dir.path()).await;
        assert_eq!(loaded.names(), vec!["demo".to_string()]);
    }
}
