//! WP22 T1 — the **authoritative** organisational record (`<home>/org.toml`).
//!
//! # Why this file exists
//!
//! WP21 put every delegation decision behind one predicate
//! ([`crate::delegation_policy::can_delegate`]) whose two inputs are the org
//! tree (`reports_to`) and the department. Both lived in
//! `<home>/agents/<id>/agent.toml` — a file **inside the agent's own working
//! directory**. [`crate::org_field_guard`] closed the Claude-runtime hole
//! (`PreToolUse` compares the write field-wise and denies org-field changes),
//! but that guard is:
//!
//! - a *speed bump* for `Bash` (a shell redirect is matched heuristically, not
//!   parsed), and
//! - **absent entirely** for the codex runtime, which
//!   have no `PreToolUse` mechanism at all.
//!
//! The judged party still owned the evidence. This module moves the evidence
//! out of reach: the authority lives at `<home>/org.toml`, which is *outside*
//! every agent's cwd — a `workspace-write` sandbox cannot reach it at all, and
//! the Claude runtime is additionally denied by
//! [`crate::org_field_guard::classify_identity_surface`].
//!
//! `agent.toml`'s `reports_to` / `department` become a **display mirror**:
//! every gated writer keeps them in sync so the dashboard, `list_agents` and
//! hand inspection all still show the right thing, but tampering with the
//! mirror no longer changes any decision.
//!
//! # Authority rule (the one thing readers must get right)
//!
//! > **Store has an entry for this agent → the store is the only truth.
//! > Store has *no* entry → fall back to that agent's `agent.toml`.**
//!
//! The fallback is what keeps the migration free: an install that predates
//! this file, an agent hand-created after seeding, and every existing test
//! fixture that builds an org tree out of `agent.toml` files all keep working
//! byte-identically, because none of them have store entries.
//!
//! # Seeding
//!
//! The gateway seeds the file **once**, at boot, only when it does not exist
//! (see `server.rs`). Re-importing on every boot would hand the attacker the
//! very loop this module closes: tamper with `agent.toml`, wait for a restart,
//! watch the tampered value get promoted to authority. Operators who edit
//! `agent.toml` by hand adopt their change explicitly with `duduclaw org sync`;
//! `duduclaw doctor` reports the drift until they do.
//!
//! # Concurrency
//!
//! Reads are lock-free (writes commit by `rename`, so a reader sees either the
//! old or the new file, never a torn one). Every write first needs the global
//! org mutation lock ([`OrgMutationGuard`] — [`upsert`], [`upsert_many`] and
//! [`remove`] take it as an argument), then takes the `org.toml` sidecar lock
//! ([`crate::with_file_lock`]) around a full, strictly parsed
//! read-modify-write, so no two writers — in any process — can interleave a
//! validate-then-commit or lose each other's entries. Lock order: the org
//! lock first, then any `agent.toml` lock / the autonomy barrier; the
//! `org.toml` lock is only taken under the org lock.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::fs_lock::with_file_lock;

/// Basename of the store under `<home>`.
pub const ORG_STORE_FILE: &str = "org.toml";

/// Basename of the "this home has already been bootstrapped" marker.
///
/// # Why a second file (WP22 T5)
///
/// Before this marker existed, [`mutate`] treated "`org.toml` is missing" as
/// "this install has never been bootstrapped" and re-imported every
/// `agent.toml` mirror. That handed back the exact loop [`seed_if_absent`]
/// closes, one `rm` away: an agent tampers with its own mirror, deletes
/// `<home>/org.toml`, then triggers any gated write (creating one sub-agent is
/// enough) — and the tampered mirror is re-imported *as authority*.
///
/// The marker splits the two cases the missing file used to conflate:
///
/// | `org.toml` | marker | meaning | bootstrap? |
/// |---|---|---|---|
/// | absent | absent | genuinely first run | **yes** |
/// | absent | present | the store was **lost or deleted** | **no** — write only the caller's entry, warn, let `duduclaw doctor` tell the operator to rebuild with `org sync` |
/// | present | either | normal operation | n/a |
///
/// Refusing to bootstrap degrades to the pre-WP22 fallback (each agent's own
/// `agent.toml`) rather than to an outage — the same fail-safe direction as
/// [`load`] — but it never *promotes* a mirror to authority behind the
/// operator's back. Adoption stays a typed human action.
///
/// The marker is itself part of the guarded surface
/// ([`crate::org_field_guard::ProtectedSurface::OrgStore`]), so deleting it is
/// no easier than deleting the store.
pub const ORG_SEEDED_FILE: &str = ".org-seeded";

/// Schema version written into the file. A file carrying a *higher* version
/// was written by a newer DuDuClaw; this build refuses to interpret it
/// (fail-safe: every reader falls back to `agent.toml`) rather than guess.
pub const ORG_STORE_SCHEMA: i64 = 1;

/// One agent's authoritative organisational placement.
///
/// Both fields are stored canonicalised: trimmed, and `reports_to = "none"`
/// (the historical spelling for "root") normalised to the empty string, so a
/// reader never has to re-normalise before comparing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrgEntry {
    /// Direct supervisor's agent id (directory name); empty = root.
    pub reports_to: String,
    /// Department name (zh-TW allowed); empty = department-less.
    pub department: String,
}

impl OrgEntry {
    /// Build a canonicalised entry from raw `agent.toml`-shaped values.
    pub fn new(reports_to: impl AsRef<str>, department: impl AsRef<str>) -> Self {
        Self {
            reports_to: normalize_parent(reports_to.as_ref()),
            department: department.as_ref().trim().to_string(),
        }
    }
}

/// Normalise a `reports_to` value: trim, and treat the literal `none` as root.
fn normalize_parent(value: &str) -> String {
    let v = value.trim();
    if v == "none" { String::new() } else { v.to_string() }
}

/// The whole store, keyed by **agent directory name**.
///
/// Directory name — not `[agent] name` — because that is the namespace the
/// delegation predicate already works in (`org_snapshot`, `DispatchOrgView`
/// and `list_agents` all key by directory), and WP21 欠帳④ settled on keeping
/// the two aligned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrgStore {
    entries: BTreeMap<String, OrgEntry>,
}

impl OrgStore {
    /// Empty store — what a missing or unreadable file resolves to.
    pub fn new() -> Self {
        Self::default()
    }

    /// This agent's authoritative placement, or `None` when the store has no
    /// opinion (the caller must then fall back to `agent.toml`).
    pub fn get(&self, agent_id: &str) -> Option<&OrgEntry> {
        self.entries.get(agent_id.trim())
    }

    /// Does the store carry an entry for this agent? (`get(..).is_some()`,
    /// spelled out because "has an entry" is the authority rule's phrasing.)
    pub fn contains(&self, agent_id: &str) -> bool {
        self.get(agent_id).is_some()
    }

    /// Iterate `(agent_id, entry)` in stable (sorted) order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &OrgEntry)> {
        self.entries.iter()
    }

    /// Number of agents carrying an authoritative record.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Is the store empty? (Also true for a missing / unreadable file.)
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Insert or replace one entry in memory (no I/O). Used by callers that
    /// build a store to serialise in one shot; single-entry persistence goes
    /// through [`upsert`].
    pub fn set(&mut self, agent_id: impl Into<String>, entry: OrgEntry) {
        let key = agent_id.into().trim().to_string();
        if key.is_empty() {
            return;
        }
        self.entries.insert(key, entry);
    }
}

/// `<home>/org.toml`.
pub fn org_store_path(home: &Path) -> PathBuf {
    home.join(ORG_STORE_FILE)
}

/// Has the store been created yet? (Drives the once-only gateway seeding.)
pub fn store_exists(home: &Path) -> bool {
    org_store_path(home).is_file()
}

/// `<home>/.org-seeded` — see [`ORG_SEEDED_FILE`].
pub fn org_seeded_path(home: &Path) -> PathBuf {
    home.join(ORG_SEEDED_FILE)
}

/// Has this home ever been bootstrapped?
pub fn seeded_marker_exists(home: &Path) -> bool {
    org_seeded_path(home).is_file()
}

/// The store is **gone but was once here** — i.e. somebody removed
/// `<home>/org.toml` on a home that had already been bootstrapped.
///
/// Delegation silently degrades to the `agent.toml` fallback in this state, so
/// `duduclaw doctor` surfaces it and asks the operator to rebuild the
/// authority with `duduclaw org sync`.
pub fn store_lost(home: &Path) -> bool {
    seeded_marker_exists(home) && !store_exists(home)
}

/// Write the bootstrap marker (best-effort; its absence only costs a future
/// re-bootstrap, never correctness of the current write).
fn ensure_seeded_marker(home: &Path) {
    let path = org_seeded_path(home);
    if path.exists() {
        return;
    }
    if let Err(e) = std::fs::write(
        &path,
        "# DuDuClaw：這個檔案代表組織權威資料（org.toml）已經建立過。\n\
         # 請勿刪除。org.toml 若不見了，DuDuClaw 不會自動從各 agent.toml 重建\n\
         # （那會讓任何改過 agent.toml 的人把自己的修改變成權威），\n\
         # 請由管理者執行 `duduclaw org sync` 重建。\n",
    ) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "could not write the org bootstrap marker"
        );
    }
}

/// Why the authority cannot be used for a **write** (RM-027 review): the
/// strict counterpart of [`load`]'s fail-safe fallback. `Display` is
/// end-user zh-TW copy that tells the operator what to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgStoreError {
    /// The home was bootstrapped ([`ORG_SEEDED_FILE`]) but `org.toml` is gone.
    Lost,
    /// `org.toml` exists but cannot be read.
    Unreadable(String),
    /// `org.toml` is not valid TOML.
    Corrupt(String),
    /// `org.toml` was written by a newer build (its `schema` number).
    UnsupportedSchema(i64),
    /// Writing the store (or taking its lock) failed.
    Io(String),
    /// The write would make `agent_id` its own ancestor (RM-027 round 3:
    /// every edge-adding authority write walks the whole chain, rollbacks
    /// included). Nothing was written.
    Cycle { agent_id: String, reports_to: String },
    /// The agent id is not a valid agent directory name; it is never
    /// written (a strict reader would refuse the whole file afterwards).
    InvalidAgentId(String),
}

impl OrgStoreError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "org_authority_write_failed",
            Self::Cycle { .. } => "cycle",
            Self::InvalidAgentId(_) => "agent_not_found",
            _ => "org_authority_unavailable",
        }
    }
}

impl std::fmt::Display for OrgStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lost => write!(
                f,
                "組織權威資料（org.toml）不見了，但這個環境先前已經建立過它。為了不把各 agent.toml \
                 的內容誤當成權威，組織調整已暫停；請管理員在自己的終端機執行 `duduclaw org sync` 重建後再試。"
            ),
            Self::Unreadable(e) => write!(
                f,
                "無法讀取組織權威資料（org.toml）：{e}。組織調整已暫停，請管理員檢查檔案權限後再試。"
            ),
            Self::Corrupt(reason) => write!(
                f,
                "組織權威資料（org.toml）的內容無法解析（{reason}）。為了不覆蓋其他 AI 員工的紀錄，\
                 組織調整已暫停；請管理員修正這個檔案，或在終端機執行 `duduclaw org sync` \
                 從各 agent.toml 完整重建（原檔會保留為 org.toml.corrupt）。"
            ),
            Self::UnsupportedSchema(n) => write!(
                f,
                "組織權威資料（org.toml）是較新版本的 DuDuClaw 寫入的（schema {n}），\
                 這個版本無法安全地修改它；請升級 DuDuClaw 後再調整組織圖。"
            ),
            Self::Io(e) => write!(f, "寫入組織權威資料（org.toml）失敗：{e}"),
            Self::Cycle { agent_id, reports_to } => write!(
                f,
                "不能讓「{a}」向「{r}」回報：「{r}」目前在「{a}」的團隊之下，這樣會形成循環。組織權威資料沒有變更。",
                a = crate::truncate_chars(agent_id, 64),
                r = crate::truncate_chars(reports_to, 64),
            ),
            Self::InvalidAgentId(id) => write!(
                f,
                "「{}」不是有效的 AI 員工代號，組織權威資料沒有變更。",
                crate::truncate_chars(id, 64)
            ),
        }
    }
}

impl std::error::Error for OrgStoreError {}

impl From<OrgStoreError> for io::Error {
    fn from(e: OrgStoreError) -> Self {
        let kind = match &e {
            OrgStoreError::Lost => io::ErrorKind::NotFound,
            OrgStoreError::Corrupt(_) | OrgStoreError::UnsupportedSchema(_) => io::ErrorKind::InvalidData,
            OrgStoreError::Cycle { .. } | OrgStoreError::InvalidAgentId(_) => io::ErrorKind::InvalidInput,
            OrgStoreError::Unreadable(_) | OrgStoreError::Io(_) => io::ErrorKind::Other,
        };
        io::Error::new(kind, e.to_string())
    }
}

/// Read the store, **never failing** — for read-only / display / delegation
/// paths. Writers that must not act on a degraded view use [`load_for_write`].
///
/// A missing file, an unreadable file, invalid TOML, or a schema this build
/// does not understand all resolve to an empty store: every reader then falls
/// back to `agent.toml`, which is the pre-WP22 behaviour. Losing the authority
/// file must degrade to "the old rules", never to "nobody may delegate" (a
/// self-inflicted outage) and never to a panic.
pub fn load(home: &Path) -> OrgStore {
    let path = org_store_path(home);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return OrgStore::new(),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "org.toml unreadable — organisational authority falls back to each \
                 agent.toml for this read"
            );
            return OrgStore::new();
        }
    };
    match parse(&content) {
        Ok(store) => store,
        Err(reason) => {
            tracing::warn!(
                path = %path.display(),
                reason = %reason,
                "org.toml could not be interpreted — organisational authority falls \
                 back to each agent.toml for this read (run `duduclaw org sync` to rebuild)"
            );
            OrgStore::new()
        }
    }
}

/// Read the store **strictly**, for a caller about to change the org chart.
///
/// - `org.toml` present and exactly in the documented shape → the store
///   ([`parse_strict`]);
/// - absent on a home that was never bootstrapped → an empty store (the
///   genuine first run: readers fall back to `agent.toml`, and the first
///   write bootstraps from the mirrors);
/// - absent on a bootstrapped home, unreadable, invalid TOML, a newer schema
///   or any shape [`parse_strict`] refuses → `Err`. Validating against the
///   mirror fallback, or writing on top of a half-understood store, would let
///   a write adopt mirrors or silently drop rows; only `duduclaw org sync`
///   may rebuild from that state.
pub fn load_for_write(home: &Path) -> Result<OrgStore, OrgStoreError> {
    match std::fs::read_to_string(org_store_path(home)) {
        Ok(content) => parse_strict(&content),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if seeded_marker_exists(home) {
                Err(OrgStoreError::Lost)
            } else {
                Ok(OrgStore::new())
            }
        }
        Err(e) => Err(OrgStoreError::Unreadable(e.to_string())),
    }
}

/// The **write-path** parser (RM-027 round 3): anything but exactly the
/// shape [`serialize`] produces is [`OrgStoreError::Corrupt`] (or
/// [`OrgStoreError::UnsupportedSchema`]) — fail closed. The tolerant
/// [`parse`] used for display would otherwise treat `schema = "x"` as v1,
/// skip non-table rows, default missing or non-string fields, and the next
/// serialize would silently drop whatever it skipped.
///
/// - top-level keys: only `schema` (required, an integer `1..=`
///   [`ORG_STORE_SCHEMA`]) and `agents` (optional, a table);
/// - every `agents` key a valid agent id ([`crate::is_valid_agent_id`]),
///   unique after canonicalisation;
/// - every row a table with exactly `reports_to` and `department`, both
///   strings (`""` = none).
fn parse_strict(content: &str) -> Result<OrgStore, OrgStoreError> {
    let corrupt = |reason: String| OrgStoreError::Corrupt(reason);
    let table = content
        .parse::<toml::Table>()
        .map_err(|e| corrupt(format!("invalid TOML: {}", first_line(&e.to_string()))))?;
    for key in table.keys() {
        if key != "schema" && key != "agents" {
            return Err(corrupt(format!("unknown top-level key `{}`", crate::truncate_chars(key, 64))));
        }
    }
    let schema = match table.get("schema") {
        Some(toml::Value::Integer(n)) => *n,
        Some(_) => return Err(corrupt("`schema` is not an integer".into())),
        None => return Err(corrupt("`schema` is missing".into())),
    };
    if schema > ORG_STORE_SCHEMA {
        return Err(OrgStoreError::UnsupportedSchema(schema));
    }
    if schema < 1 {
        return Err(corrupt(format!("unsupported `schema = {schema}`")));
    }

    let mut store = OrgStore::new();
    let agents = match table.get("agents") {
        None => return Ok(store),
        Some(toml::Value::Table(t)) => t,
        Some(_) => return Err(corrupt("`agents` is not a table".into())),
    };
    for (raw_id, value) in agents {
        let agent_id = raw_id.trim();
        if agent_id != raw_id.as_str() || !crate::is_valid_agent_id(agent_id) {
            return Err(corrupt(format!("invalid agent id `{}`", crate::truncate_chars(raw_id, 64))));
        }
        let Some(row) = value.as_table() else {
            return Err(corrupt(format!("row `{agent_id}` is not a table")));
        };
        for key in row.keys() {
            if key != "reports_to" && key != "department" {
                return Err(corrupt(format!(
                    "row `{agent_id}` has an unknown key `{}`",
                    crate::truncate_chars(key, 64)
                )));
            }
        }
        let entry = OrgEntry::new(
            strict_field(row, agent_id, "reports_to")?,
            strict_field(row, agent_id, "department")?,
        );
        if store.entries.contains_key(agent_id) {
            return Err(corrupt(format!("duplicate agent id `{agent_id}`")));
        }
        store.entries.insert(agent_id.to_string(), entry);
    }
    Ok(store)
}

/// One required string field of a strict row.
fn strict_field<'a>(row: &'a toml::Table, agent_id: &str, key: &str) -> Result<&'a str, OrgStoreError> {
    match row.get(key) {
        Some(toml::Value::String(s)) => Ok(s.as_str()),
        Some(_) => Err(OrgStoreError::Corrupt(format!("row `{agent_id}`: `{key}` is not a string"))),
        None => Err(OrgStoreError::Corrupt(format!("row `{agent_id}`: `{key}` is missing"))),
    }
}

/// Parse store content **tolerantly** (display / delegation reads through
/// [`load`]). The error's reason text is logged, and shown to operators
/// through [`OrgStoreError`]'s `Display`.
fn parse(content: &str) -> Result<OrgStore, OrgStoreError> {
    let table = content
        .parse::<toml::Table>()
        .map_err(|e| OrgStoreError::Corrupt(format!("invalid TOML: {}", first_line(&e.to_string()))))?;

    // A missing `schema` key is treated as v1 (hand-written files, and the
    // very first release). A *higher* number is refused — see the constant.
    let schema = table.get("schema").and_then(|v| v.as_integer()).unwrap_or(ORG_STORE_SCHEMA);
    if schema > ORG_STORE_SCHEMA {
        return Err(OrgStoreError::UnsupportedSchema(schema));
    }

    let mut store = OrgStore::new();
    let Some(agents) = table.get("agents").and_then(|v| v.as_table()) else {
        return Ok(store); // `schema` only — a legitimately empty store
    };
    for (agent_id, value) in agents {
        let Some(entry) = value.as_table() else {
            continue; // not a table — ignore this row rather than fail the file
        };
        let field = |k: &str| entry.get(k).and_then(|v| v.as_str()).unwrap_or_default();
        store.set(
            agent_id.clone(),
            OrgEntry::new(field("reports_to"), field("department")),
        );
    }
    Ok(store)
}

/// Render the store as TOML.
///
/// Hand-rolled rather than `toml::to_string_pretty`: the document mixes a
/// top-level value (`schema`) with a table (`[agents]`), and TOML requires
/// values before tables — a serialiser driven by a sorted map would emit them
/// the wrong way round. Every id and value goes through `toml::Value::String`,
/// so a hostile agent id cannot inject structure.
fn serialize(store: &OrgStore) -> String {
    let mut out = String::new();
    out.push_str(
        "# DuDuClaw 組織權威資料（org.toml）\n\
         #\n\
         # 這是「誰是誰的主管、誰屬於哪個部門」的唯一權威來源，委派判定只看這個檔案。\n\
         # 各 AI 員工目錄下的 agent.toml 只是顯示用的鏡像，改它不會改變判定。\n\
         # 由 DuDuClaw 自動維護：請透過儀表板的組織圖調整，或手改 agent.toml 後\n\
         # 執行 `duduclaw org sync` 匯入。`duduclaw doctor` 會提示兩者不一致。\n\n",
    );
    out.push_str(&format!("schema = {ORG_STORE_SCHEMA}\n"));
    for (agent_id, entry) in store.iter() {
        out.push_str(&format!("\n[agents.{}]\n", toml_key(agent_id)));
        out.push_str(&format!("reports_to = {}\n", toml_string(&entry.reports_to)));
        out.push_str(&format!("department = {}\n", toml_string(&entry.department)));
    }
    out
}

/// A TOML key: bare when it is unambiguously bare-safe, quoted otherwise.
fn toml_key(key: &str) -> String {
    let bare_safe = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if bare_safe {
        key.to_string()
    } else {
        toml_string(key)
    }
}

/// A correctly escaped TOML **basic** string (`"…"`).
///
/// Hand-rolled rather than `toml::Value::String(..).to_string()`: the `toml`
/// crate's `Display` promotes a value containing newlines to a multi-line
/// literal (`'''…'''`), which is fine for a value but produces a document this
/// module cannot round-trip when the same helper renders a *key*. Escaping
/// every control character here keeps keys and values on one line, always.
fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().to_string()
}

/// How [`mutate`] treats an authority it cannot use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteMode {
    /// Ordinary gated writers (agent creation, removal, …): an unreadable,
    /// corrupt or newer-schema file is refused (never rebuilt from an empty
    /// store — that would wipe every other entry); a lost store on a
    /// bootstrapped home records only the caller's entry (WP22 T5).
    Lenient,
    /// Org-chart changes under [`OrgMutationGuard`]: everything
    /// [`load_for_write`] refuses is refused, including a lost store.
    Strict,
    /// `duduclaw org sync`, the explicit rebuild: a corrupt file is kept as
    /// `org.toml.corrupt` and rebuilt; a newer schema is still refused.
    Rebuild,
}

/// Read-modify-write the store under the `org.toml` sidecar lock.
///
/// Only reachable through an [`OrgMutationGuard`] (the global org lock), so
/// this sidecar lock is only ever taken under the org lock. The closure sees the entries as they are on disk
/// *right now*, parsed strictly ([`parse_strict`]), and returns whatever the
/// caller wants back — or an error, in which case nothing is written; the
/// file is rewritten atomically (temp + rename) only when the content
/// changed.
fn mutate<T>(
    home: &Path,
    mode: WriteMode,
    f: impl FnOnce(&mut OrgStore) -> Result<T, OrgStoreError>,
) -> Result<T, OrgStoreError> {
    let path = org_store_path(home);
    crate::config::create_home_dir(home).map_err(|e| OrgStoreError::Io(e.to_string()))?;
    let locked = with_file_lock(&path, || {
        // Set when this write adopts the mirrors wholesale (first-run
        // bootstrap, `org sync` rebuild): the result is a batch write and
        // gets the whole-graph cycle walk below.
        let mut batch_import = mode == WriteMode::Rebuild;
        let mut store = match std::fs::read_to_string(&path) {
            Ok(content) => match parse_strict(&content) {
                Ok(s) => s,
                Err(OrgStoreError::Corrupt(reason)) if mode == WriteMode::Rebuild => {
                    // Do not silently discard an operator's file: keep a copy
                    // before this write replaces it, and say so loudly.
                    let backup = path.with_extension("toml.corrupt");
                    let _ = std::fs::copy(&path, &backup);
                    tracing::warn!(
                        path = %path.display(),
                        backup = %backup.display(),
                        reason = %reason,
                        "org.toml was not interpretable and is being rebuilt by `org sync` — \
                         the previous file was kept as a .corrupt copy"
                    );
                    OrgStore::new()
                }
                Err(e) => {
                    // Rebuilding from an empty store would wipe every other
                    // agent's entry: refuse and leave the bytes alone.
                    tracing::warn!(
                        path = %path.display(),
                        reason = %e,
                        "org.toml is not usable — refusing to write the org authority \
                         (run `duduclaw org sync` to rebuild)"
                    );
                    return Ok(Err(e));
                }
            },
            // First write of *any* kind bootstraps the whole store from the
            // `agent.toml` mirrors, then applies the caller's change on top.
            // Without this, whichever gated writer happened to run first on an
            // upgraded install (a `duduclaw agent create` before the gateway's
            // first boot, say) would create a one-entry file — and because the
            // file's existence is the "already bootstrapped" flag, every other
            // agent would then be stranded on the fallback path forever.
            // Trusting the mirrors here is the same trust decision the gateway
            // makes when it seeds: at bootstrap there is no prior authority to
            // protect. It happens exactly once…
            //
            // …and *only* once: the [`ORG_SEEDED_FILE`] marker distinguishes
            // "never bootstrapped" from "bootstrapped, then the store was
            // deleted". Re-importing in the second case is a laundering
            // channel — tamper with a mirror, delete `org.toml`, trigger any
            // gated write, and the tampered value becomes authority. When the
            // marker is present the store starts empty instead: this write
            // records its own entry, everyone else degrades to the pre-WP22
            // `agent.toml` fallback (fail-safe, not an outage), and
            // `duduclaw doctor` tells the operator to rebuild with
            // `duduclaw org sync`.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if seeded_marker_exists(home) && mode == WriteMode::Strict {
                    return Ok(Err(OrgStoreError::Lost));
                }
                if seeded_marker_exists(home) {
                    tracing::warn!(
                        path = %path.display(),
                        "org.toml is missing on a home that was already bootstrapped — \
                         refusing to re-import the agent.toml mirrors (that would promote \
                         whatever they now say to authority). Delegation falls back to \
                         agent.toml until an operator runs `duduclaw org sync`."
                    );
                    OrgStore::new()
                } else {
                    // Bootstrap trusts the mirrors as they are (there is no
                    // prior authority to protect); the caller's own edge is
                    // still cycle-checked against the result below.
                    let mut seeded = OrgStore::new();
                    for (agent_id, entry) in scan_mirrors(home) {
                        seeded.set(agent_id, entry);
                    }
                    batch_import = true;
                    seeded
                }
            }
            Err(e) => return Ok(Err(OrgStoreError::Unreadable(e.to_string()))),
        };

        let before = store.clone();
        let out = match f(&mut store) {
            Ok(out) => out,
            // Refused (a cycle, an invalid id, …): leave the bytes alone.
            Err(e) => return Ok(Err(e)),
        };
        // RM-027 round 4: a wholesale import is a batch write — the mirrors
        // it adopted are never trusted to be acyclic. Refuse the whole write
        // (nothing is written) when the resulting stored graph has a loop.
        if batch_import {
            if let Err(e) = check_store_graph(&store) {
                tracing::warn!(
                    path = %path.display(),
                    reason = %e,
                    "the agent.toml mirrors form a reporting cycle — refusing to adopt them \
                     as the org authority (fix the agent.toml files, then run `duduclaw org sync`)"
                );
                return Ok(Err(e));
            }
        }
        // Rewrite when the content changed, and always on first creation — the
        // file's *existence* is the "already bootstrapped" flag that stops the
        // gateway re-seeding (and re-adopting tampered mirrors) every boot.
        if store != before || !path.exists() {
            write_atomic(&path, &serialize(&store))?;
        }
        // The marker is written whenever the store file exists, so a home
        // upgraded from a build that predates it acquires one on the next
        // write rather than staying re-bootstrappable forever.
        ensure_seeded_marker(home);
        Ok(Ok(out))
    });
    match locked {
        Ok(inner) => inner,
        Err(e) => Err(OrgStoreError::Io(e.to_string())),
    }
}

// ── The global org mutation lock (RM-027 review) ────────────────────────────

/// Basename (under `<home>/locks/`) of the one lock every org-chart change
/// holds: the dashboard `org.*` RPCs and the `org_change` approval, the
/// `agents.update` org fields, the MCP `agent_update` org path and
/// `duduclaw org sync`. The lock file itself is `<name>.lock`.
pub const ORG_MUTATION_LOCK: &str = "org-mutation";

/// Directory (under `<home>`) of the `org_change` approval journal
/// (`org_change_journal/<id>.json`). AI-unwritable like `org.toml`.
pub const ORG_CHANGE_JOURNAL_DIR: &str = "org_change_journal";

/// RM-027 round 5 (moved to core in round 6 so the CLI shares it): the
/// longest a writer holding the global org lock ([`OrgMutationGuard`]) waits
/// for an `agent.toml` — or any other per-agent file — lock (in-process
/// mutex + advisory lock, one budget). An ordinary write holds such a lock
/// for milliseconds; 30 s absorbs a slow disk or a lifecycle-barrier drain
/// (the admission wait is also 30 s). Past it the waiter gives up with a
/// `lock_wait_timeout` error and writes nothing.
pub const MIRROR_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// `<home>/locks/org-mutation` (the advisory lock lives on `<that>.lock`).
pub fn org_mutation_lock_path(home: &Path) -> PathBuf {
    home.join("locks").join(ORG_MUTATION_LOCK)
}

/// Proof that the caller holds the global org mutation lock.
///
/// The per-file lock [`mutate`] takes only keeps two writers from losing
/// each other's entries; it does not make "read the tree, validate, write"
/// atomic. Two moves validated against the same tree (A→B and B→A) would
/// both pass the cycle check and together form a cycle. Every org-chart
/// change therefore runs inside this one cross-process lock: re-read the
/// authority ([`Self::load`]), compare the base, validate the **whole** tree,
/// commit ([`Self::upsert`]) — then release.
///
/// RM-027 round 3: the guard is also the only way to write the authority at
/// all. [`upsert`], [`upsert_many`] and [`remove`] take it as their first
/// argument, so agent creation / removal, ephemeral scaffolding, templates,
/// premium kits, expert install and rollback, migration, seeding and sync
/// are all serialised with org-chart changes (an expert rollback can no
/// longer restore B→A while an `org.apply_change` commits A→B). Every
/// edge-adding write — rollbacks included — re-walks the whole supervisor
/// chain and refuses a cycle ([`OrgStoreError::Cycle`]).
///
/// Lock order, everywhere: this lock is taken **first** — before any
/// `agent.toml` lock and before the autonomy lifecycle barrier. The
/// `org.toml` sidecar lock (inside [`mutate`]) is only ever taken while this
/// lock is held, so it can never be part of a lock cycle. Never take this
/// lock while holding an `agent.toml` lock or the barrier.
///
/// Not re-entrant: never take it twice in one call chain.
///
/// RM-027 round 5/6: while this lock is held, every wait on an `agent.toml`
/// (or other per-agent file) lock is bounded by [`MIRROR_LOCK_WAIT`] — a
/// stuck writer fails the org mutation (nothing written) instead of pinning
/// this lock for every other org writer.
#[derive(Debug)]
pub struct OrgMutationGuard {
    home: PathBuf,
    _lock: crate::fs_lock::FileLockGuard,
}

/// Take the global org mutation lock (blocking). Async callers acquire it on
/// a blocking thread and keep the guard across their `.await`s.
pub fn lock_org_mutations(home: &Path) -> io::Result<OrgMutationGuard> {
    let lock = crate::fs_lock::lock_exclusive(&org_mutation_lock_path(home))?;
    Ok(OrgMutationGuard { home: home.to_path_buf(), _lock: lock })
}

/// Run `f` while holding the global org lock (blocking) — the one-liner for
/// a synchronous gated writer: `with_org_lock(home, |org| upsert(org, id, e))`.
pub fn with_org_lock<T>(home: &Path, f: impl FnOnce(&OrgMutationGuard) -> io::Result<T>) -> io::Result<T> {
    let guard = lock_org_mutations(home)?;
    f(&guard)
}

impl OrgMutationGuard {
    /// The home this guard locks.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The authority as it is now, strictly ([`load_for_write`]).
    pub fn load(&self) -> Result<OrgStore, OrgStoreError> {
        load_for_write(&self.home)
    }

    /// Record one agent's placement, refusing a lost / unreadable / corrupt /
    /// newer-schema store and a cycle. Returns whether the stored value
    /// changed.
    pub fn upsert(&self, agent_id: &str, entry: OrgEntry) -> Result<bool, OrgStoreError> {
        let agent_id = agent_id.trim().to_string();
        if agent_id.is_empty() {
            return Ok(false);
        }
        let home = self.home.clone();
        mutate(&self.home, WriteMode::Strict, move |store| set_checked(store, &home, &agent_id, entry))
    }

    /// Put back what the agent had before a failed change (`None` = it had no
    /// entry, so remove it). Restoring an edge is an edge-adding write: it is
    /// cycle-checked against the tree as it is now.
    pub fn restore(&self, agent_id: &str, prior: Option<OrgEntry>) -> Result<(), OrgStoreError> {
        let agent_id = agent_id.trim().to_string();
        if agent_id.is_empty() {
            return Ok(());
        }
        let home = self.home.clone();
        mutate(&self.home, WriteMode::Strict, move |store| match prior {
            Some(entry) => set_checked(store, &home, &agent_id, entry).map(|_| ()),
            None => {
                store.entries.remove(&agent_id);
                Ok(())
            }
        })
    }
}

/// Safety bound on the supervisor walk (operator-sized trees; a longer chain
/// is treated as a cycle — fail closed). Same bound as `org_change`.
const MAX_CHAIN_DEPTH: usize = 64;

/// `agent_id`'s supervisor under the authority rule: the store entry when
/// there is one, the `agent.toml` mirror otherwise (empty = root / unknown).
fn effective_parent(store: &OrgStore, home: &Path, agent_id: &str) -> String {
    if let Some(entry) = store.get(agent_id) {
        return entry.reports_to.clone();
    }
    if !crate::is_valid_agent_id(agent_id) || crate::is_internal_agent_dir(agent_id) {
        return String::new();
    }
    read_mirror(&home.join("agents").join(agent_id).join("agent.toml"))
        .map(|e| e.reports_to)
        .unwrap_or_default()
}

/// Walk up from `agent_id`'s (new) supervisor through the whole effective
/// tree; reaching `agent_id`, revisiting a node or running past the bound is
/// a cycle.
fn check_no_cycle(store: &OrgStore, home: &Path, agent_id: &str) -> Result<(), OrgStoreError> {
    let target = effective_parent(store, home, agent_id);
    if target.is_empty() {
        return Ok(());
    }
    let cycle = || OrgStoreError::Cycle { agent_id: agent_id.to_string(), reports_to: target.clone() };
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut current = target.clone();
    for _ in 0..MAX_CHAIN_DEPTH {
        if current == agent_id || !visited.insert(current.clone()) {
            return Err(cycle());
        }
        let next = effective_parent(store, home, &current);
        if next.is_empty() {
            return Ok(());
        }
        current = next;
    }
    Err(cycle())
}

/// Does writing `entry` for `agent_id` add a supervisor edge the effective
/// tree does not already have? (Department-only writes, and re-recording the
/// edge the mirror fallback already implies, do not.)
fn adds_edge(store: &OrgStore, home: &Path, agent_id: &str, entry: &OrgEntry) -> bool {
    !entry.reports_to.is_empty()
        && (entry.reports_to == agent_id || effective_parent(store, home, agent_id) != entry.reports_to)
}

/// Set one entry: the id must be a valid agent id, and an edge-adding write
/// must not close a loop anywhere up the chain. Returns whether the stored
/// value changed. On `Err` the caller ([`mutate`]) writes nothing.
fn set_checked(store: &mut OrgStore, home: &Path, agent_id: &str, entry: OrgEntry) -> Result<bool, OrgStoreError> {
    if !crate::is_valid_agent_id(agent_id) {
        return Err(OrgStoreError::InvalidAgentId(agent_id.to_string()));
    }
    let new_edge = adds_edge(store, home, agent_id, &entry);
    let changed = store.get(agent_id) != Some(&entry);
    store.set(agent_id.to_string(), entry);
    if new_edge {
        check_no_cycle(store, home, agent_id)?;
    }
    Ok(changed)
}

/// RM-027 round 4: walk the **whole** stored graph — every row with a
/// non-empty `reports_to` — using the store's own rows only (no mirror
/// fallback). A batch write (`org sync`, a corrupt / lost rebuild, the
/// first-write bootstrap, [`upsert_many`]) cannot rely on [`adds_edge`]: when
/// the store started empty, the mirror fallback makes every adopted edge
/// look pre-existing, so a mirror cycle `A→B, B→A` would be promoted to
/// authority unchecked. The walk stops at a parent the store has no row for.
fn check_store_graph(store: &OrgStore) -> Result<(), OrgStoreError> {
    for (agent_id, entry) in store.iter() {
        if entry.reports_to.is_empty() {
            continue;
        }
        let cycle = || OrgStoreError::Cycle { agent_id: agent_id.clone(), reports_to: entry.reports_to.clone() };
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut current: &str = entry.reports_to.as_str();
        let mut closed = true;
        for _ in 0..MAX_CHAIN_DEPTH {
            if current == agent_id.as_str() || !visited.insert(current) {
                return Err(cycle());
            }
            match store.get(current) {
                Some(next) if !next.reports_to.is_empty() => current = next.reports_to.as_str(),
                _ => {
                    closed = false;
                    break;
                }
            }
        }
        if closed {
            return Err(cycle());
        }
    }
    Ok(())
}

/// [`set_checked`] for a batch: every row is applied first, then every row
/// that added an edge is walked (a batch can close a loop across several of
/// its own rows), and finally the whole resulting stored graph is walked
/// without mirror fallback ([`check_store_graph`]) — any cycle refuses the
/// whole batch. Returns the number of rows whose value changed.
fn set_all_checked(store: &mut OrgStore, home: &Path, entries: &[(String, OrgEntry)]) -> Result<usize, OrgStoreError> {
    let mut changed = 0usize;
    let mut new_edges: Vec<&str> = Vec::new();
    for (agent_id, entry) in entries {
        let agent_id = agent_id.trim();
        if !crate::is_valid_agent_id(agent_id) {
            return Err(OrgStoreError::InvalidAgentId(agent_id.to_string()));
        }
        if adds_edge(store, home, agent_id, entry) {
            new_edges.push(agent_id);
        }
        if store.get(agent_id) != Some(entry) {
            changed += 1;
        }
        store.set(agent_id.to_string(), entry.clone());
    }
    for agent_id in new_edges {
        check_no_cycle(store, home, agent_id)?;
    }
    check_store_graph(store)?;
    Ok(changed)
}

/// Atomic commit: write a sibling temp file, then rename over the target.
fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, content)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Record (or replace) one agent's authoritative placement, holding the
/// global org lock (`org` — see [`OrgMutationGuard`] for the lock order).
///
/// Returns `true` when the stored value actually changed. Callers are the
/// *gated* writers only — MCP `create_agent` / ephemeral spawn, the
/// dashboard agent RPCs, template staging, pack / kit / expert install and
/// rollback — each of which has already run its WP21 C4 authorization
/// check. Refuses an invalid id and an edge that would close a cycle.
pub fn upsert(org: &OrgMutationGuard, agent_id: &str, entry: OrgEntry) -> io::Result<bool> {
    let agent_id = agent_id.trim().to_string();
    if agent_id.is_empty() {
        return Ok(false);
    }
    let home = org.home.clone();
    Ok(mutate(&org.home, WriteMode::Lenient, move |store| set_checked(store, &home, &agent_id, entry))?)
}

/// Replace many entries in one locked write (seeding, migration). Every row
/// that adds an edge is cycle-checked against the resulting tree, and the
/// whole resulting stored graph is walked without mirror fallback (RM-027
/// round 4) — a loop anywhere refuses the whole batch.
///
/// Returns the number of entries whose value changed.
pub fn upsert_many(org: &OrgMutationGuard, entries: &[(String, OrgEntry)]) -> io::Result<usize> {
    if entries.is_empty() {
        return Ok(0);
    }
    let home = org.home.clone();
    Ok(mutate(&org.home, WriteMode::Lenient, |store| set_all_checked(store, &home, entries))?)
}

/// Drop one agent's entry (removal / offboarding / ephemeral GC / rollback
/// of a creation), holding the global org lock.
///
/// Returns `true` when an entry was actually present. A no-op on a missing
/// store — removing what was never recorded is success, not an error.
pub fn remove(org: &OrgMutationGuard, agent_id: &str) -> io::Result<bool> {
    let agent_id = agent_id.trim().to_string();
    if agent_id.is_empty() {
        return Ok(false);
    }
    if !store_exists(&org.home) {
        return Ok(false);
    }
    Ok(mutate(&org.home, WriteMode::Lenient, move |store| Ok(store.entries.remove(&agent_id).is_some()))?)
}

// ── Staged agent creation (RM-027 round 4) ──────────────────────────────────

/// Basename prefix of a creation's staging directory. Dot-prefixed, so the
/// registry scan, [`scan_mirrors`] and every `is_internal_agent_dir` filter
/// skip it: a half-built employee is never visible, and its `agent.toml`
/// can never be read as a mirror.
pub const AGENT_STAGING_PREFIX: &str = ".creating-";

/// An agent directory being built **off to the side** of its final path.
///
/// # Why (RM-027 round 4)
///
/// Creation flows used to publish `agents/<id>/agent.toml` first and only
/// then wait for the org lock to record the authority row — and when that
/// wait or the write failed (a corrupt store, a cycle) they only warned and
/// reported success. The employee was then governed by its own mirror
/// forever (fail-open), and the cycle check never ran.
///
/// Now a creation takes the org lock at its start, builds the directory here
/// (same parent as the target, so the publish is a same-filesystem
/// `rename`), and calls [`Self::publish`] while still holding the lock: the
/// authority row is validated and committed first, the directory renamed
/// into place second. Any failure on the way — including an authority
/// refusal — drops the staging directory (see `Drop`) and fails the whole
/// creation; there is no mirror fallback.
#[derive(Debug)]
pub struct StagedAgentDir {
    staging: PathBuf,
    target: PathBuf,
    agent_id: String,
    published: bool,
}

impl StagedAgentDir {
    /// Create an empty staging directory next to `target`, holding the org
    /// lock (`org` — proof only). Refuses with `AlreadyExists` when `target`
    /// is already taken.
    pub fn create(org: &OrgMutationGuard, target: &Path, agent_id: &str) -> io::Result<Self> {
        let _ = org;
        let agent_id = agent_id.trim();
        if !crate::is_valid_agent_id(agent_id) {
            return Err(OrgStoreError::InvalidAgentId(agent_id.to_string()).into());
        }
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "agent directory has no parent"))?;
        std::fs::create_dir_all(parent)?;
        if std::fs::symlink_metadata(target).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("agent '{agent_id}' already exists"),
            ));
        }
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let staging = parent.join(format!(
            "{AGENT_STAGING_PREFIX}{agent_id}-{}-{nanos:x}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir(&staging)?;
        Ok(Self { staging, target: target.to_path_buf(), agent_id: agent_id.to_string(), published: false })
    }

    /// Where to build the directory's contents.
    pub fn path(&self) -> &Path {
        &self.staging
    }

    /// The final directory [`Self::publish`] renames the staging dir to.
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Inside the org lock: validate and commit the authority row (a lost
    /// store records only this row, a corrupt / newer store and a cycle are
    /// refused — [`upsert`]), then publish the directory by `rename`. On any
    /// failure the row is put back as it was, the staging directory removed,
    /// and the error returned — the employee never appears.
    pub fn publish(mut self, org: &OrgMutationGuard, entry: OrgEntry) -> io::Result<PathBuf> {
        if std::fs::symlink_metadata(&self.target).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("agent '{}' already exists", self.agent_id),
            ));
        }
        let prior = load(org.home()).get(&self.agent_id).cloned();
        upsert(org, &self.agent_id, entry)?;
        if let Err(e) = std::fs::rename(&self.staging, &self.target) {
            let rollback = match prior {
                Some(p) => upsert(org, &self.agent_id, p).map(|_| ()),
                None => remove(org, &self.agent_id).map(|_| ()),
            };
            if let Err(re) = rollback {
                tracing::warn!(
                    agent = %self.agent_id,
                    error = %re,
                    "could not roll back the org authority row after a failed agent publish"
                );
            }
            return Err(e);
        }
        self.published = true;
        Ok(self.target.clone())
    }
}

impl Drop for StagedAgentDir {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_dir_all(&self.staging);
        }
    }
}

// ── Mirror scanning (seeding / sync / drift) ────────────────────────────────

/// Read `[agent] reports_to` / `department` out of one `agent.toml`.
///
/// Parsed as a generic table (not `AgentConfig`) for the same reason
/// `delegation_gate::parse_org_node` does: a partially-written or
/// forward-versioned file must still yield the org fields it *does* have.
pub fn read_mirror(agent_toml: &Path) -> Option<OrgEntry> {
    let content = std::fs::read_to_string(agent_toml).ok()?;
    let table = content.parse::<toml::Table>().ok()?;
    let agent = table.get("agent")?.as_table()?;
    let field = |k: &str| agent.get(k).and_then(|v| v.as_str()).unwrap_or_default();
    Some(OrgEntry::new(field("reports_to"), field("department")))
}

/// Scan `<home>/agents/*/agent.toml` into `(dir_name, entry)` pairs, sorted.
///
/// Skips `_trash` / `_archive` and every dot-directory (`.ephemeral` included:
/// ephemeral scaffolds are transient and are recorded by their spawn path, not
/// by a directory sweep). Unreadable agents are skipped, not failed.
pub fn scan_mirrors(home: &Path) -> Vec<(String, OrgEntry)> {
    let agents_dir = home.join("agents");
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return Vec::new();
    };
    let mut out: BTreeMap<String, OrgEntry> = BTreeMap::new();
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let Some(dir_name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if dir_name.starts_with('_') || dir_name.starts_with('.') {
            continue;
        }
        // Only directories that are valid agent ids can carry an authority
        // row (the strict write-path parser refuses any other key).
        if !crate::is_valid_agent_id(&dir_name) {
            continue;
        }
        if let Some(mirror) = read_mirror(&entry.path().join("agent.toml")) {
            out.insert(dir_name, mirror);
        }
    }
    out.into_iter().collect()
}

/// One agent whose store record disagrees with its `agent.toml` mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgDrift {
    pub agent_id: String,
    /// What the store says (the value delegation actually uses).
    pub store: OrgEntry,
    /// What `agent.toml` says (display only).
    pub mirror: OrgEntry,
}

/// Agents whose `agent.toml` disagrees with the authoritative store.
///
/// Only agents that *have* a store entry are compared: one without an entry is
/// governed by its `agent.toml` under the fallback rule, so there is nothing to
/// disagree with. A store entry whose agent directory has vanished is likewise
/// not drift — `duduclaw doctor` would only be noise about deleted agents.
pub fn detect_drift(home: &Path) -> Vec<OrgDrift> {
    let store = load(home);
    if store.is_empty() {
        return Vec::new();
    }
    scan_mirrors(home)
        .into_iter()
        .filter_map(|(agent_id, mirror)| {
            let recorded = store.get(&agent_id)?;
            (recorded != &mirror).then(|| OrgDrift {
                agent_id,
                store: recorded.clone(),
                mirror,
            })
        })
        .collect()
}

/// Bootstrap the store from the `agent.toml` mirrors — **once**.
///
/// `Ok(None)` when the file already exists: re-importing on every boot would
/// re-open the exact hole this module closes (tamper, restart, get promoted).
/// `Ok(Some(n))` reports how many agents were imported into the new file.
///
/// The import itself lives in [`mutate`], so a gated writer that happens to
/// run before the gateway's first boot bootstraps identically — this function
/// is just the explicit "do it now, and tell me what happened" entry point.
/// The file is created even with zero agents: its *existence* is the "already
/// bootstrapped" flag, and a fresh install must not be re-seeded (and
/// re-tamperable) on the next boot.
pub fn seed_if_absent(home: &Path) -> io::Result<Option<usize>> {
    if store_exists(home) {
        // Upgrade path: a home whose store predates [`ORG_SEEDED_FILE`] would
        // otherwise stay re-bootstrappable (delete the store → next gated
        // write re-imports the mirrors). The gateway calls this on every boot,
        // so the marker lands on the first boot after the upgrade.
        ensure_seeded_marker(home);
        return Ok(None);
    }
    // Seeding is an authority write like any other: under the global org
    // lock, re-checked once held (another process may have seeded first).
    let _org = lock_org_mutations(home)?;
    if store_exists(home) {
        ensure_seeded_marker(home);
        return Ok(None);
    }
    let imported = mutate(home, WriteMode::Lenient, |store| Ok(store.len()))?;
    Ok(Some(imported))
}

/// What one `duduclaw org sync` would change (or did change).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgSyncChange {
    pub agent_id: String,
    /// Previous store value; `None` when the agent had no record yet.
    pub before: Option<OrgEntry>,
    /// Value adopted from `agent.toml`.
    pub after: OrgEntry,
}

/// Adopt `agent.toml` mirrors into the store — the operator's explicit
/// "yes, I meant that edit" action (`duduclaw org sync`).
///
/// `only` restricts the sync to a single agent id. `dry_run` computes the diff
/// without writing. Returns the changes (empty ⇒ already in sync).
///
/// Runs inside the global org mutation lock ([`lock_org_mutations`]), so it
/// cannot interleave with a dashboard / MCP org change. This is the one
/// writer allowed to rebuild a lost or corrupt store from the mirrors — and
/// only as a full rebuild (`only` is refused then: a one-agent rebuild would
/// leave everyone else without an entry). A newer schema is never rebuilt.
pub fn sync_from_mirrors(
    home: &Path,
    only: Option<&str>,
    dry_run: bool,
) -> io::Result<Vec<OrgSyncChange>> {
    let _org_lock = lock_org_mutations(home)?;
    let (store, rebuilding) = match load_for_write(home) {
        Ok(store) => (store, false),
        Err(OrgStoreError::Lost) | Err(OrgStoreError::Corrupt(_)) => (OrgStore::new(), true),
        Err(e) => return Err(e.into()),
    };
    if rebuilding && only.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "org.toml 遺失或損壞，只能完整重建：請不要指定單一 AI 員工，直接執行 `duduclaw org sync`。",
        ));
    }
    let mirrors: Vec<(String, OrgEntry)> = scan_mirrors(home)
        .into_iter()
        .filter(|(id, _)| only.is_none_or(|want| want.trim() == id))
        .collect();

    let changes: Vec<OrgSyncChange> = mirrors
        .iter()
        .filter(|(id, mirror)| store.get(id) != Some(mirror))
        .map(|(id, mirror)| OrgSyncChange {
            agent_id: id.clone(),
            before: store.get(id).cloned(),
            after: mirror.clone(),
        })
        .collect();

    if !dry_run && !changes.is_empty() {
        let to_write: Vec<(String, OrgEntry)> = changes
            .iter()
            .map(|c| (c.agent_id.clone(), c.after.clone()))
            .collect();
        // Adopted mirrors are edge-adding writes too: a set of mirrors that
        // forms a loop is refused as a whole (fix the agent.toml, re-run).
        mutate(home, WriteMode::Rebuild, |store| set_all_checked(store, home, &to_write))?;
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// A gated writer's single upsert: global org lock, then the write.
    fn put(home: &Path, id: &str, entry: OrgEntry) -> io::Result<bool> {
        with_org_lock(home, |org| upsert(org, id, entry))
    }

    /// A gated writer's single removal.
    fn drop_entry(home: &Path, id: &str) -> io::Result<bool> {
        with_org_lock(home, |org| remove(org, id))
    }

    fn write_agent(home: &Path, dir: &str, reports_to: &str, department: &str) {
        let d = home.join("agents").join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("agent.toml"),
            format!(
                "[agent]\nname = \"{dir}\"\ndisplay_name = \"{dir}\"\nrole = \"specialist\"\n\
                 status = \"active\"\ntrigger = \"@{dir}\"\nreports_to = \"{reports_to}\"\n\
                 department = \"{department}\"\nicon = \"x\"\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn missing_file_loads_as_empty_store() {
        let h = home();
        let store = load(h.path());
        assert!(store.is_empty());
        assert_eq!(store.get("anyone"), None);
        assert!(!store_exists(h.path()));
    }

    #[test]
    fn upsert_then_load_round_trips_and_normalises() {
        let h = home();
        assert!(put(h.path(), "sales-lead", OrgEntry::new("boss", "業務")).unwrap());
        // "none" is the historical spelling for root — canonicalised to "".
        assert!(put(h.path(), "boss", OrgEntry::new(" none ", "  ")).unwrap());

        let store = load(h.path());
        assert_eq!(store.len(), 2);
        assert_eq!(
            store.get("sales-lead"),
            Some(&OrgEntry {
                reports_to: "boss".into(),
                department: "業務".into()
            })
        );
        assert_eq!(
            store.get("boss"),
            Some(&OrgEntry {
                reports_to: String::new(),
                department: String::new()
            })
        );
        // Idempotent: re-writing the same value reports "no change".
        assert!(!put(h.path(), "sales-lead", OrgEntry::new("boss", "業務")).unwrap());
        // What this module writes, the strict write-path parser accepts.
        assert_eq!(load_for_write(h.path()).unwrap(), load(h.path()));
        // The file really is TOML with the documented shape.
        let raw = std::fs::read_to_string(org_store_path(h.path())).unwrap();
        assert!(raw.contains("schema = 1"));
        assert!(raw.contains("[agents.sales-lead]"));
    }

    #[test]
    fn remove_drops_the_entry_and_is_a_noop_when_absent() {
        let h = home();
        put(h.path(), "eph-1", OrgEntry::new("boss", "")).unwrap();
        assert!(drop_entry(h.path(), "eph-1").unwrap());
        assert!(!drop_entry(h.path(), "eph-1").unwrap());
        assert!(load(h.path()).get("eph-1").is_none());
        // Removing from a store that was never created must not create one.
        let fresh = home();
        assert!(!drop_entry(fresh.path(), "nobody").unwrap());
        assert!(!store_exists(fresh.path()));
    }

    #[test]
    fn writes_commit_atomically_and_leave_no_temp_file() {
        let h = home();
        put(h.path(), "a", OrgEntry::new("", "")).unwrap();
        put(h.path(), "b", OrgEntry::new("a", "")).unwrap();
        let tmp = org_store_path(h.path()).with_extension("toml.tmp");
        assert!(!tmp.exists(), "temp file must be renamed away");
        assert!(org_store_path(h.path()).is_file());
        assert_eq!(load(h.path()).len(), 2);
    }

    /// RM-027 review: a corrupt file used to be "rebuilt" by the next upsert
    /// from an empty store — wiping every other agent's entry. Reads stay
    /// fail-safe; ordinary writes are refused with the bytes untouched; only
    /// `org sync` rebuilds (keeping a `.corrupt` copy).
    #[test]
    fn corrupt_file_is_fail_safe_on_read_refused_on_write_and_rebuilt_only_by_sync() {
        let h = home();
        write_agent(h.path(), "boss", "", "經營");
        write_agent(h.path(), "a", "boss", "業務");
        seed_if_absent(h.path()).unwrap();
        let corrupt = "this is [not( toml";
        std::fs::write(org_store_path(h.path()), corrupt).unwrap();
        // Read: empty store, no panic — callers fall back to agent.toml.
        assert!(load(h.path()).is_empty());
        assert!(matches!(load_for_write(h.path()), Err(OrgStoreError::Corrupt(_))));
        // Write: refused, nothing replaced.
        assert!(put(h.path(), "a", OrgEntry::new("", "")).is_err());
        assert!(drop_entry(h.path(), "a").is_err());
        let guard = lock_org_mutations(h.path()).unwrap();
        assert!(matches!(guard.upsert("a", OrgEntry::new("", "")), Err(OrgStoreError::Corrupt(_))));
        drop(guard);
        assert_eq!(std::fs::read_to_string(org_store_path(h.path())).unwrap(), corrupt);
        // A one-agent sync cannot rebuild it (it would strand everyone else)…
        assert!(sync_from_mirrors(h.path(), Some("a"), false).is_err());
        assert_eq!(std::fs::read_to_string(org_store_path(h.path())).unwrap(), corrupt);
        // …a full `org sync` does, and keeps the operator's bytes.
        assert_eq!(sync_from_mirrors(h.path(), None, false).unwrap().len(), 2);
        assert_eq!(load(h.path()).len(), 2);
        let backup = org_store_path(h.path()).with_extension("toml.corrupt");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), corrupt, "corrupt original must be preserved");
    }

    #[test]
    fn future_schema_is_refused_rather_than_half_interpreted() {
        let h = home();
        let newer = "schema = 99\n[agents.a]\nreports_to = \"boss\"\n";
        std::fs::write(org_store_path(h.path()), newer).unwrap();
        assert!(load(h.path()).is_empty());
        assert_eq!(load_for_write(h.path()), Err(OrgStoreError::UnsupportedSchema(99)));
        // No writer — not even `org sync` — downgrades a newer file.
        write_agent(h.path(), "a", "", "");
        assert!(put(h.path(), "a", OrgEntry::new("", "")).is_err());
        assert!(sync_from_mirrors(h.path(), None, false).is_err());
        assert_eq!(std::fs::read_to_string(org_store_path(h.path())).unwrap(), newer);
    }

    #[test]
    fn strict_load_tells_a_first_run_from_a_lost_store() {
        let h = home();
        write_agent(h.path(), "ceo", "", "");
        // Never bootstrapped: an empty view (mirror fallback), not an error.
        assert_eq!(load_for_write(h.path()), Ok(OrgStore::new()));
        seed_if_absent(h.path()).unwrap();
        assert_eq!(load_for_write(h.path()).unwrap().len(), 1);
        // Bootstrapped, then deleted: refused for writers under the lock.
        std::fs::remove_file(org_store_path(h.path())).unwrap();
        assert_eq!(load_for_write(h.path()), Err(OrgStoreError::Lost));
        let guard = lock_org_mutations(h.path()).unwrap();
        assert_eq!(guard.upsert("ceo", OrgEntry::new("", "x")), Err(OrgStoreError::Lost));
        assert!(!store_exists(h.path()), "a strict write must not recreate a lost store");
        drop(guard);
        // An unreadable path (a directory where the file should be).
        let u = home();
        std::fs::create_dir_all(org_store_path(u.path())).unwrap();
        assert!(matches!(load_for_write(u.path()), Err(OrgStoreError::Unreadable(_))));
    }

    #[test]
    fn the_org_mutation_lock_is_exclusive_and_sync_waits_for_it() {
        let h = home();
        write_agent(h.path(), "boss", "", "");
        seed_if_absent(h.path()).unwrap();
        write_agent(h.path(), "boss", "", "經營");
        let guard = lock_org_mutations(h.path()).unwrap();
        assert!(org_mutation_lock_path(h.path()).starts_with(h.path().join("locks")));
        let path = h.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let syncer = std::thread::spawn(move || {
            let changes = sync_from_mirrors(&path, None, false).unwrap();
            tx.send(changes.len()).unwrap();
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200)).is_err(),
            "`org sync` must wait for the org mutation lock"
        );
        assert_eq!(load(h.path()).get("boss").unwrap().department, "");
        // Committing inside the lock, then a rollback, both work.
        assert!(guard.upsert("boss", OrgEntry::new("", "業務")).unwrap());
        guard.restore("boss", Some(OrgEntry::new("", ""))).unwrap();
        guard.upsert("tmp", OrgEntry::new("boss", "")).unwrap();
        guard.restore("tmp", None).unwrap();
        assert!(load(h.path()).get("tmp").is_none());
        drop(guard);
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(), 1);
        syncer.join().unwrap();
        assert_eq!(load(h.path()).get("boss").unwrap().department, "經營");
    }

    #[test]
    fn hostile_agent_ids_cannot_inject_toml_structure() {
        let h = home();
        // An id that is not an agent directory name is never written…
        let e = put(h.path(), "weird id\"]\n[agents.ceo", OrgEntry::new("", "")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
        assert!(!store_exists(h.path()));
        // …and a hostile *value* is escaped, never structure.
        put(h.path(), "weird", OrgEntry::new("x\"\n[agents.root]\nreports_to = \"y", "")).unwrap();
        let store = load_for_write(h.path()).unwrap();
        assert_eq!(store.len(), 1, "no extra table may appear: {store:?}");
        assert!(store.get("ceo").is_none());
        assert!(store.get("root").is_none());
    }

    /// RM-027 round 3, finding 3: the write path accepts exactly the shape
    /// `serialize` produces. Every other shape is Corrupt (writes refused,
    /// bytes untouched) — the tolerant reader would have defaulted or
    /// skipped it and the next serialize would have dropped it silently.
    #[test]
    fn the_write_path_parser_is_strict_and_fails_closed() {
        let cases: &[(&str, &str)] = &[
            ("schema = \"x\"\n[agents.rep]\nreports_to = \"\"\ndepartment = \"\"\n", "string schema"),
            ("[agents.rep]\nreports_to = \"\"\ndepartment = \"\"\n", "missing schema"),
            ("schema = 0\n", "schema below 1"),
            ("schema = 1\nother = 1\n", "unknown top-level key"),
            ("schema = 1\nagents = 5\n", "agents not a table"),
            ("schema = 1\n[agents]\nrep = \"boss\"\n", "row not a table"),
            ("schema = 1\n[agents.rep]\nreports_to = \"boss\"\n", "missing department"),
            ("schema = 1\n[agents.rep]\ndepartment = \"業務\"\n", "missing reports_to"),
            ("schema = 1\n[agents.rep]\nreports_to = 7\ndepartment = \"\"\n", "non-string reports_to"),
            ("schema = 1\n[agents.rep]\nreports_to = \"\"\ndepartment = [\"a\"]\n", "non-string department"),
            ("schema = 1\n[agents.rep]\nreports_to = \"\"\ndepartment = \"\"\nrole = \"x\"\n", "unknown row key"),
            ("schema = 1\n[agents.\"bad id\"]\nreports_to = \"\"\ndepartment = \"\"\n", "invalid agent id"),
            ("schema = 1\n[agents.\" rep\"]\nreports_to = \"\"\ndepartment = \"\"\n", "untrimmed agent id"),
        ];
        for (content, why) in cases {
            let h = home();
            write_agent(h.path(), "rep", "", "");
            std::fs::write(org_store_path(h.path()), content).unwrap();
            assert!(
                matches!(load_for_write(h.path()), Err(OrgStoreError::Corrupt(_))),
                "{why}: {:?}",
                load_for_write(h.path())
            );
            assert!(put(h.path(), "rep", OrgEntry::new("", "x")).is_err(), "{why}");
            assert!(drop_entry(h.path(), "rep").is_err(), "{why}");
            let guard = lock_org_mutations(h.path()).unwrap();
            assert!(guard.upsert("rep", OrgEntry::new("", "x")).is_err(), "{why}");
            drop(guard);
            assert_eq!(std::fs::read_to_string(org_store_path(h.path())).unwrap(), *content, "{why}: bytes untouched");
            // Display stays tolerant (never panics, never errors).
            let _ = load(h.path());
        }
        // Explicit empty strings are the documented "none" and are accepted.
        let h = home();
        std::fs::write(
            org_store_path(h.path()),
            "schema = 1\n[agents.rep]\nreports_to = \"\"\ndepartment = \"\"\n",
        )
        .unwrap();
        assert_eq!(load_for_write(h.path()).unwrap().get("rep"), Some(&OrgEntry::new("", "")));
        // A strictly refused file is still rebuilt by `org sync` (with a copy).
        let h = home();
        write_agent(h.path(), "rep", "", "業務");
        std::fs::write(org_store_path(h.path()), "schema = \"x\"\n").unwrap();
        assert_eq!(sync_from_mirrors(h.path(), None, false).unwrap().len(), 1);
        assert_eq!(load_for_write(h.path()).unwrap().get("rep").unwrap().department, "業務");
        assert!(org_store_path(h.path()).with_extension("toml.corrupt").is_file());
    }

    /// RM-027 round 3, finding 1: every authority writer needs the global
    /// org lock (a lenient `upsert` / `remove` waits for it like an org
    /// change does), and every edge-adding write — a rollback restoring an
    /// old edge included — walks the whole tree and refuses a cycle.
    #[test]
    fn every_writer_takes_the_org_lock_and_no_write_can_close_a_cycle() {
        let h = home();
        write_agent(h.path(), "ceo", "", "");
        write_agent(h.path(), "a", "ceo", "");
        write_agent(h.path(), "b", "ceo", "");
        seed_if_absent(h.path()).unwrap();

        // A lenient writer waits for a held org lock.
        let held = lock_org_mutations(h.path()).unwrap();
        let path = h.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            tx.send(put(&path, "newcomer", OrgEntry::new("ceo", "")).unwrap()).unwrap();
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200)).is_err(),
            "a gated upsert must wait for the org mutation lock"
        );
        assert!(load(h.path()).get("newcomer").is_none());

        // Under the lock: A→B is applied; a "rollback" that restores B→A
        // (expert replace, a creation rollback, …) is refused — by the
        // lenient writer and by the guard's own restore alike.
        assert!(held.upsert("a", OrgEntry::new("b", "")).unwrap());
        let e = upsert(&held, "b", OrgEntry::new("a", "")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
        assert!(matches!(
            held.restore("b", Some(OrgEntry::new("a", ""))),
            Err(OrgStoreError::Cycle { .. })
        ));
        assert!(matches!(held.upsert("ceo", OrgEntry::new("b", "")), Err(OrgStoreError::Cycle { .. })));
        assert!(matches!(held.upsert("ceo", OrgEntry::new("ceo", "")), Err(OrgStoreError::Cycle { .. })));
        // A batch closing a loop across its own rows is refused as a whole.
        let batch = vec![
            ("ceo".to_string(), OrgEntry::new("x1", "")),
            ("x1".to_string(), OrgEntry::new("ceo", "")),
        ];
        assert!(upsert_many(&held, &batch).is_err());
        assert_eq!(load(h.path()).get("b"), Some(&OrgEntry::new("ceo", "")));
        assert_eq!(load(h.path()).get("ceo"), Some(&OrgEntry::new("", "")));
        assert!(load(h.path()).get("x1").is_none());
        // A department-only write on an agent in the chain is not an edge.
        assert!(held.upsert("b", OrgEntry::new("ceo", "業務")).unwrap());
        drop(held);
        writer.join().unwrap();
        assert!(rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap());
        assert_eq!(load(h.path()).get("newcomer").unwrap().reports_to, "ceo");

        // `org sync` refuses to adopt mirrors that form a loop.
        write_agent(h.path(), "ceo", "b", "");
        let before = std::fs::read_to_string(org_store_path(h.path())).unwrap();
        assert!(sync_from_mirrors(h.path(), None, false).is_err());
        assert_eq!(std::fs::read_to_string(org_store_path(h.path())).unwrap(), before);
    }

    /// RM-027 round 4, finding 2: a rebuild from an empty store used to skip
    /// the cycle check — `adds_edge` read the same mirror the batch adopted,
    /// so every edge looked pre-existing. The whole resulting graph is now
    /// walked without mirror fallback and a loop refuses the whole batch.
    #[test]
    fn a_rebuild_or_sync_of_cyclic_mirrors_is_refused_as_a_whole() {
        // Corrupt store, mirrors A→B, B→A.
        let h = home();
        write_agent(h.path(), "ceo", "", "");
        write_agent(h.path(), "a", "b", "");
        write_agent(h.path(), "b", "a", "");
        let corrupt = "this is [not( toml";
        std::fs::write(org_store_path(h.path()), corrupt).unwrap();
        std::fs::write(org_seeded_path(h.path()), "").unwrap();
        let e = sync_from_mirrors(h.path(), None, false).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
        assert_eq!(std::fs::read_to_string(org_store_path(h.path())).unwrap(), corrupt);

        // Lost store (marker present, file gone), same cyclic mirrors.
        let h = home();
        write_agent(h.path(), "ceo", "", "");
        write_agent(h.path(), "a", "b", "");
        write_agent(h.path(), "b", "a", "");
        std::fs::write(org_seeded_path(h.path()), "").unwrap();
        assert!(store_lost(h.path()));
        assert!(sync_from_mirrors(h.path(), None, false).is_err());
        assert!(!store_exists(h.path()), "a refused rebuild must not create the store");

        // Healthy store; a brand-new, never-registered pair of mirrors
        // forms a loop among themselves — `org sync` refuses the batch.
        let h = home();
        write_agent(h.path(), "ceo", "", "");
        seed_if_absent(h.path()).unwrap();
        write_agent(h.path(), "x", "y", "");
        write_agent(h.path(), "y", "x", "");
        write_agent(h.path(), "z", "ceo", "業務");
        let before = std::fs::read_to_string(org_store_path(h.path())).unwrap();
        assert!(sync_from_mirrors(h.path(), None, false).is_err());
        assert_eq!(std::fs::read_to_string(org_store_path(h.path())).unwrap(), before);
        assert!(load(h.path()).get("z").is_none(), "the whole batch is refused");
        // A longer loop (x→y→w→x) is refused too.
        write_agent(h.path(), "y", "w", "");
        write_agent(h.path(), "w", "x", "");
        assert!(sync_from_mirrors(h.path(), None, false).is_err());
        // Once the loop is broken, the sync adopts everything.
        write_agent(h.path(), "x", "ceo", "");
        assert_eq!(sync_from_mirrors(h.path(), None, false).unwrap().len(), 4);

        // `upsert_many` gets the same whole-graph walk.
        let guard = lock_org_mutations(h.path()).unwrap();
        let batch = vec![
            ("p".to_string(), OrgEntry::new("q", "")),
            ("q".to_string(), OrgEntry::new("p", "")),
        ];
        assert!(upsert_many(&guard, &batch).is_err());
        assert!(load(h.path()).get("p").is_none());
    }

    /// RM-027 round 4, finding 1: a staged creation is invisible until it
    /// publishes, and an authority refusal removes it instead of leaving an
    /// employee governed by its own mirror.
    #[test]
    fn a_staged_agent_publishes_only_after_its_authority_row_commits() {
        let h = home();
        write_agent(h.path(), "ceo", "", "");
        seed_if_absent(h.path()).unwrap();
        let target = h.path().join("agents").join("newbie");

        // Success: invisible while staged, published with its row.
        let org = lock_org_mutations(h.path()).unwrap();
        let staged = StagedAgentDir::create(&org, &target, "newbie").unwrap();
        std::fs::write(staged.path().join("agent.toml"), "[agent]\nname = \"newbie\"\nreports_to = \"ceo\"\n").unwrap();
        assert!(!target.exists());
        assert!(scan_mirrors(h.path()).iter().all(|(id, _)| id != "newbie"));
        assert!(staged.path().file_name().unwrap().to_str().unwrap().starts_with(AGENT_STAGING_PREFIX));
        assert_eq!(staged.publish(&org, OrgEntry::new("ceo", "")).unwrap(), target);
        assert!(target.join("agent.toml").is_file());
        assert_eq!(load(h.path()).get("newbie").unwrap().reports_to, "ceo");
        // A taken name is refused up front.
        assert_eq!(
            StagedAgentDir::create(&org, &target, "newbie").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );

        // Cycle: a stale row says `ceo` reports to `loop`; creating `loop`
        // under `ceo` would close it — refused, no directory, no row.
        org.upsert("ceo", OrgEntry::new("loop", "")).unwrap();
        let looped = h.path().join("agents").join("loop");
        let staged = StagedAgentDir::create(&org, &looped, "loop").unwrap();
        let staging = staged.path().to_path_buf();
        let e = staged.publish(&org, OrgEntry::new("ceo", "")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
        assert!(!looped.exists() && !staging.exists());
        assert!(load(h.path()).get("loop").is_none());
        org.upsert("ceo", OrgEntry::new("", "")).unwrap();

        // Corrupt authority: refused, nothing published, bytes untouched.
        std::fs::write(org_store_path(h.path()), "schema = \"x\"\n").unwrap();
        let other = h.path().join("agents").join("other");
        let staged = StagedAgentDir::create(&org, &other, "other").unwrap();
        let staging = staged.path().to_path_buf();
        assert!(staged.publish(&org, OrgEntry::new("ceo", "")).is_err());
        assert!(!other.exists() && !staging.exists());
        assert_eq!(std::fs::read_to_string(org_store_path(h.path())).unwrap(), "schema = \"x\"\n");

        // Dropped without publishing (an aborted build): no trace left.
        let staged = StagedAgentDir::create(&org, &other, "other").unwrap();
        let staging = staged.path().to_path_buf();
        drop(staged);
        assert!(!staging.exists() && !other.exists());
    }

    /// RM-027 round 4, finding 2: the first-write bootstrap adopts every
    /// mirror wholesale — a mirror cycle is refused, not promoted.
    #[test]
    fn bootstrap_refuses_mirrors_that_form_a_cycle() {
        let h = home();
        write_agent(h.path(), "a", "b", "");
        write_agent(h.path(), "b", "a", "");
        assert!(seed_if_absent(h.path()).is_err());
        assert!(put(h.path(), "newcomer", OrgEntry::new("", "")).is_err());
        assert!(!store_exists(h.path()));
        assert!(!seeded_marker_exists(h.path()));
        // The ordinary acyclic bootstrap is unaffected.
        write_agent(h.path(), "b", "", "");
        assert_eq!(seed_if_absent(h.path()).unwrap(), Some(2));
    }

    #[test]
    fn seed_imports_once_then_never_again() {
        let h = home();
        write_agent(h.path(), "boss", "", "經營");
        write_agent(h.path(), "sales-lead", "boss", "業務");
        std::fs::create_dir_all(h.path().join("agents/_trash/old")).unwrap();

        assert_eq!(seed_if_absent(h.path()).unwrap(), Some(2));
        let store = load(h.path());
        assert_eq!(store.get("sales-lead").unwrap().reports_to, "boss");
        assert_eq!(store.get("boss").unwrap().department, "經營");

        // Tamper with the mirror, then "restart": the authority must not move.
        write_agent(h.path(), "sales-lead", "ceo-impostor", "經營");
        assert_eq!(seed_if_absent(h.path()).unwrap(), None);
        assert_eq!(load(h.path()).get("sales-lead").unwrap().reports_to, "boss");
    }

    /// A gated write on a not-yet-bootstrapped home must import *everyone*,
    /// not just the agent being written. The file's existence is the
    /// "bootstrapped" flag, so a one-entry file would strand every other agent
    /// on the fallback path permanently.
    #[test]
    fn first_gated_write_bootstraps_the_whole_store() {
        let h = home();
        write_agent(h.path(), "boss", "", "經營");
        write_agent(h.path(), "sales-lead", "boss", "業務");
        write_agent(h.path(), "mkt-lead", "boss", "行銷");
        assert!(!store_exists(h.path()));

        put(h.path(), "newcomer", OrgEntry::new("boss", "客服")).unwrap();

        let store = load(h.path());
        assert_eq!(store.len(), 4, "{store:?}");
        assert_eq!(store.get("mkt-lead").unwrap().department, "行銷");
        assert_eq!(store.get("newcomer").unwrap().department, "客服");
        assert_eq!(seed_if_absent(h.path()).unwrap(), None);
    }

    #[test]
    fn seed_on_an_empty_install_still_creates_the_file() {
        let h = home();
        assert_eq!(seed_if_absent(h.path()).unwrap(), Some(0));
        assert!(store_exists(h.path()));
        assert_eq!(seed_if_absent(h.path()).unwrap(), None);
    }

    #[test]
    fn drift_lists_only_agents_the_store_governs() {
        let h = home();
        write_agent(h.path(), "boss", "", "");
        write_agent(h.path(), "sales-lead", "boss", "業務");
        seed_if_absent(h.path()).unwrap();
        assert!(detect_drift(h.path()).is_empty());

        // Mirror edited by hand → drift.
        write_agent(h.path(), "sales-lead", "boss", "行銷");
        let drift = detect_drift(h.path());
        assert_eq!(drift.len(), 1);
        assert_eq!(drift[0].agent_id, "sales-lead");
        assert_eq!(drift[0].store.department, "業務");
        assert_eq!(drift[0].mirror.department, "行銷");

        // An agent with no store entry is governed by its agent.toml — not drift.
        write_agent(h.path(), "newcomer", "boss", "客服");
        assert_eq!(detect_drift(h.path()).len(), 1);
    }

    #[test]
    fn sync_adopts_mirrors_and_reports_a_diff() {
        let h = home();
        write_agent(h.path(), "boss", "", "");
        write_agent(h.path(), "sales-lead", "boss", "業務");
        seed_if_absent(h.path()).unwrap();
        write_agent(h.path(), "sales-lead", "boss", "行銷");
        write_agent(h.path(), "newcomer", "boss", "客服");

        // Dry run reports without writing.
        let preview = sync_from_mirrors(h.path(), None, true).unwrap();
        assert_eq!(preview.len(), 2);
        assert_eq!(load(h.path()).get("sales-lead").unwrap().department, "業務");

        let changes = sync_from_mirrors(h.path(), None, false).unwrap();
        assert_eq!(changes.len(), 2);
        let lead = changes.iter().find(|c| c.agent_id == "sales-lead").unwrap();
        assert_eq!(lead.before.as_ref().unwrap().department, "業務");
        assert_eq!(lead.after.department, "行銷");
        let newcomer = changes.iter().find(|c| c.agent_id == "newcomer").unwrap();
        assert_eq!(newcomer.before, None);

        assert!(detect_drift(h.path()).is_empty());
        assert!(sync_from_mirrors(h.path(), None, false).unwrap().is_empty());
    }

    #[test]
    fn sync_can_be_scoped_to_one_agent() {
        let h = home();
        write_agent(h.path(), "boss", "", "");
        write_agent(h.path(), "a", "boss", "業務");
        write_agent(h.path(), "b", "boss", "業務");
        seed_if_absent(h.path()).unwrap();
        write_agent(h.path(), "a", "boss", "行銷");
        write_agent(h.path(), "b", "boss", "客服");

        let changes = sync_from_mirrors(h.path(), Some("a"), false).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(load(h.path()).get("a").unwrap().department, "行銷");
        assert_eq!(load(h.path()).get("b").unwrap().department, "業務");
    }

    /// The store is only authoritative if agents cannot write it. Placed here
    /// rather than in `org_field_guard`'s own tests so the guarantee sits next
    /// to the module that depends on it.
    #[test]
    fn agents_are_refused_write_access_to_the_store() {
        let h = home();
        let home_path = h.path();
        let target = org_store_path(home_path);

        assert_eq!(
            crate::org_field_guard::classify_identity_surface(&target, home_path),
            Some(crate::org_field_guard::ProtectedSurface::OrgStore)
        );
        let decision = crate::org_field_guard::check_identity_surface_write(
            &target,
            home_path,
            None,
            Some("schema = 1\n[agents.me]\nreports_to = \"ceo\"\n"),
        );
        assert!(
            matches!(
                decision,
                crate::agent_guard::GuardDecision::BlockedIdentitySurface { .. }
            ),
            "org.toml must be refused outright, got {decision:?}"
        );

        // Scoped exactly: an `org.toml` in a project the agent works on is
        // none of the guard's business.
        let unrelated = home_path.join("projects").join("app").join(ORG_STORE_FILE);
        assert_eq!(
            crate::org_field_guard::classify_identity_surface(&unrelated, home_path),
            None
        );
    }

    /// WP22 T5, laundering attack #2: an agent with a shell deletes
    /// `<home>/org.toml`, then triggers any gated write (creating one
    /// sub-agent is enough). Before the marker, `mutate`'s
    /// bootstrap-on-missing branch re-imported every `agent.toml` — including
    /// the one it had just tampered with — straight into the authority.
    #[test]
    fn deleting_the_store_does_not_re_import_tampered_mirrors() {
        let h = home();
        write_agent(h.path(), "ceo", "", "經營");
        write_agent(h.path(), "sales-rep", "ceo", "業務");
        seed_if_absent(h.path()).unwrap();
        assert!(seeded_marker_exists(h.path()), "seeding must leave a marker");

        // Attack: tamper with the mirror, delete the store, trigger a write.
        write_agent(h.path(), "sales-rep", "", "經營"); // promote self to root
        std::fs::remove_file(org_store_path(h.path())).unwrap();
        assert!(store_lost(h.path()));

        put(h.path(), "helper", OrgEntry::new("sales-rep", "")).unwrap();

        let store = load(h.path());
        assert_eq!(
            store.get("sales-rep"),
            None,
            "the tampered mirror must NOT have been promoted to authority: {store:?}"
        );
        assert_eq!(store.len(), 1, "only the gated write's own entry: {store:?}");
        assert_eq!(store.get("helper").unwrap().reports_to, "sales-rep");
    }

    /// The marker must not break the legitimate first-run bootstrap, and the
    /// operator's recovery path (`org sync`) must still work afterwards.
    #[test]
    fn a_genuine_first_run_still_bootstraps_and_sync_still_recovers() {
        let h = home();
        write_agent(h.path(), "ceo", "", "經營");
        write_agent(h.path(), "sales-rep", "ceo", "業務");
        assert!(!seeded_marker_exists(h.path()));

        // No marker ⇒ genuine first run ⇒ full import (unchanged behaviour).
        put(h.path(), "helper", OrgEntry::new("ceo", "")).unwrap();
        assert_eq!(load(h.path()).len(), 3);

        // Now simulate the loss, then the operator's explicit rebuild.
        std::fs::remove_file(org_store_path(h.path())).unwrap();
        assert!(store_lost(h.path()));
        sync_from_mirrors(h.path(), None, false).unwrap();
        assert_eq!(load(h.path()).get("sales-rep").unwrap().reports_to, "ceo");
        assert!(!store_lost(h.path()));
    }

    /// Upgrade path: a home whose store predates the marker acquires one on
    /// the gateway's next boot, so it does not stay re-bootstrappable.
    #[test]
    fn an_unmarked_pre_existing_store_is_marked_on_the_next_seed_call() {
        let h = home();
        write_agent(h.path(), "ceo", "", "");
        seed_if_absent(h.path()).unwrap();
        std::fs::remove_file(org_seeded_path(h.path())).unwrap();
        assert!(!seeded_marker_exists(h.path()));

        assert_eq!(seed_if_absent(h.path()).unwrap(), None);
        assert!(seeded_marker_exists(h.path()));
    }

    /// Both halves of the store's on-disk footprint are refused to agents —
    /// deleting the marker would re-open the re-import channel.
    #[test]
    fn the_bootstrap_marker_is_guarded_like_the_store_itself() {
        let h = home();
        let marker = org_seeded_path(h.path());
        assert_eq!(
            crate::org_field_guard::classify_identity_surface(&marker, h.path()),
            Some(crate::org_field_guard::ProtectedSurface::OrgStore)
        );
        assert!(matches!(
            crate::org_field_guard::check_identity_surface_write(
                &marker,
                h.path(),
                None,
                Some("")
            ),
            crate::agent_guard::GuardDecision::BlockedIdentitySurface { .. }
        ));
    }

    #[test]
    fn concurrent_writers_do_not_lose_entries() {
        let h = home();
        let path = h.path().to_path_buf();
        seed_if_absent(&path).unwrap();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let p = path.clone();
                std::thread::spawn(move || {
                    put(&p, &format!("agent-{i}"), OrgEntry::new("boss", "業務")).unwrap()
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(load(&path).len(), 8);
    }
}
