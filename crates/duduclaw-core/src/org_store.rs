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
//! - **absent entirely** for the codex / gemini / antigravity runtimes, which
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
//! old or the new file, never a torn one). Every write takes the cross-process
//! advisory lock ([`crate::with_file_lock`]) around a full read-modify-write,
//! so the MCP server process and the gateway process cannot lose each other's
//! entries.

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

/// Read the store, **never failing**.
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

/// Parse store content. `Err(reason)` is a human-readable zh-agnostic reason
/// (logged, not shown to end users).
fn parse(content: &str) -> Result<OrgStore, String> {
    let table = content
        .parse::<toml::Table>()
        .map_err(|e| format!("invalid TOML: {}", first_line(&e.to_string())))?;

    // A missing `schema` key is treated as v1 (hand-written files, and the
    // very first release). A *higher* number is refused — see the constant.
    let schema = table.get("schema").and_then(|v| v.as_integer()).unwrap_or(ORG_STORE_SCHEMA);
    if schema > ORG_STORE_SCHEMA {
        return Err(format!(
            "schema {schema} is newer than this build understands ({ORG_STORE_SCHEMA})"
        ));
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

/// Read-modify-write the store under the cross-process lock.
///
/// The closure sees the entries as they are on disk *right now* (re-read
/// inside the lock, so a concurrent writer in another process is never lost)
/// and returns whatever the caller wants back; the file is rewritten
/// atomically (temp + rename) only when the closure reports a change.
fn mutate<T>(
    home: &Path,
    f: impl FnOnce(&mut OrgStore) -> T,
) -> io::Result<T> {
    let path = org_store_path(home);
    std::fs::create_dir_all(home)?;
    with_file_lock(&path, || {
        let mut store = match std::fs::read_to_string(&path) {
            Ok(content) => match parse(&content) {
                Ok(s) => s,
                Err(reason) => {
                    // Do not silently discard an operator's file: keep a copy
                    // before this write replaces it, and say so loudly.
                    let backup = path.with_extension("toml.corrupt");
                    let _ = std::fs::copy(&path, &backup);
                    tracing::warn!(
                        path = %path.display(),
                        backup = %backup.display(),
                        reason = %reason,
                        "org.toml was not interpretable and is being rebuilt — the \
                         previous file was kept as a .corrupt copy"
                    );
                    OrgStore::new()
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
                    let mut seeded = OrgStore::new();
                    for (agent_id, entry) in scan_mirrors(home) {
                        seeded.set(agent_id, entry);
                    }
                    seeded
                }
            }
            Err(e) => return Err(e),
        };

        let before = store.clone();
        let out = f(&mut store);
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
        Ok(out)
    })
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

/// Record (or replace) one agent's authoritative placement.
///
/// Returns `true` when the stored value actually changed. Callers are the
/// *gated* writers only — MCP `create_agent` / `agent_update` / ephemeral
/// spawn, the dashboard agent RPCs, template staging and expert install —
/// each of which has already run its WP21 C4 authorization check.
pub fn upsert(home: &Path, agent_id: &str, entry: OrgEntry) -> io::Result<bool> {
    let agent_id = agent_id.trim().to_string();
    if agent_id.is_empty() {
        return Ok(false);
    }
    mutate(home, move |store| {
        let changed = store.get(&agent_id) != Some(&entry);
        store.set(agent_id, entry);
        changed
    })
}

/// Replace many entries in one locked write (seeding, `org sync`).
///
/// Returns the number of entries whose value changed.
pub fn upsert_many(home: &Path, entries: &[(String, OrgEntry)]) -> io::Result<usize> {
    if entries.is_empty() {
        return Ok(0);
    }
    mutate(home, |store| {
        let mut changed = 0usize;
        for (agent_id, entry) in entries {
            if store.get(agent_id) != Some(entry) {
                changed += 1;
            }
            store.set(agent_id.clone(), entry.clone());
        }
        changed
    })
}

/// Drop one agent's entry (removal / offboarding / ephemeral GC).
///
/// Returns `true` when an entry was actually present. A no-op on a missing
/// store — removing what was never recorded is success, not an error.
pub fn remove(home: &Path, agent_id: &str) -> io::Result<bool> {
    let agent_id = agent_id.trim().to_string();
    if agent_id.is_empty() {
        return Ok(false);
    }
    if !store_exists(home) {
        return Ok(false);
    }
    mutate(home, move |store| store.entries.remove(&agent_id).is_some())
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
    let imported = mutate(home, |store| store.len())?;
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
pub fn sync_from_mirrors(
    home: &Path,
    only: Option<&str>,
    dry_run: bool,
) -> io::Result<Vec<OrgSyncChange>> {
    let store = load(home);
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
        upsert_many(home, &to_write)?;
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
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
        assert!(upsert(h.path(), "sales-lead", OrgEntry::new("boss", "業務")).unwrap());
        // "none" is the historical spelling for root — canonicalised to "".
        assert!(upsert(h.path(), "boss", OrgEntry::new(" none ", "  ")).unwrap());

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
        assert!(!upsert(h.path(), "sales-lead", OrgEntry::new("boss", "業務")).unwrap());
        // The file really is TOML with the documented shape.
        let raw = std::fs::read_to_string(org_store_path(h.path())).unwrap();
        assert!(raw.contains("schema = 1"));
        assert!(raw.contains("[agents.sales-lead]"));
    }

    #[test]
    fn remove_drops_the_entry_and_is_a_noop_when_absent() {
        let h = home();
        upsert(h.path(), "eph-1", OrgEntry::new("boss", "")).unwrap();
        assert!(remove(h.path(), "eph-1").unwrap());
        assert!(!remove(h.path(), "eph-1").unwrap());
        assert!(load(h.path()).get("eph-1").is_none());
        // Removing from a store that was never created must not create one.
        let fresh = home();
        assert!(!remove(fresh.path(), "nobody").unwrap());
        assert!(!store_exists(fresh.path()));
    }

    #[test]
    fn writes_commit_atomically_and_leave_no_temp_file() {
        let h = home();
        upsert(h.path(), "a", OrgEntry::new("", "")).unwrap();
        upsert(h.path(), "b", OrgEntry::new("a", "")).unwrap();
        let tmp = org_store_path(h.path()).with_extension("toml.tmp");
        assert!(!tmp.exists(), "temp file must be renamed away");
        assert!(org_store_path(h.path()).is_file());
        assert_eq!(load(h.path()).len(), 2);
    }

    #[test]
    fn corrupt_file_is_fail_safe_on_read_and_backed_up_on_write() {
        let h = home();
        std::fs::create_dir_all(h.path()).unwrap();
        std::fs::write(org_store_path(h.path()), "this is [not( toml").unwrap();
        // Read: empty store, no panic — callers fall back to agent.toml.
        assert!(load(h.path()).is_empty());
        // Write: rebuilds, but keeps the operator's bytes.
        upsert(h.path(), "a", OrgEntry::new("boss", "")).unwrap();
        assert_eq!(load(h.path()).len(), 1);
        let backup = org_store_path(h.path()).with_extension("toml.corrupt");
        assert!(backup.exists(), "corrupt original must be preserved");
    }

    #[test]
    fn future_schema_is_refused_rather_than_half_interpreted() {
        let h = home();
        std::fs::write(
            org_store_path(h.path()),
            "schema = 99\n[agents.a]\nreports_to = \"boss\"\n",
        )
        .unwrap();
        assert!(load(h.path()).is_empty());
    }

    #[test]
    fn hostile_agent_ids_cannot_inject_toml_structure() {
        let h = home();
        upsert(
            h.path(),
            "weird id\"]\n[agents.ceo",
            OrgEntry::new("x\"\n[agents.root]\nreports_to = \"y", ""),
        )
        .unwrap();
        let store = load(h.path());
        assert_eq!(store.len(), 1, "no extra table may appear: {store:?}");
        assert!(store.get("ceo").is_none());
        assert!(store.get("root").is_none());
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

        upsert(h.path(), "newcomer", OrgEntry::new("boss", "客服")).unwrap();

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

        upsert(h.path(), "helper", OrgEntry::new("sales-rep", "")).unwrap();

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
        upsert(h.path(), "helper", OrgEntry::new("ceo", "")).unwrap();
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
                    upsert(&p, &format!("agent-{i}"), OrgEntry::new("boss", "業務")).unwrap()
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(load(&path).len(), 8);
    }
}
