//! Expert-pack admin surface — the shared source of truth for the on-disk
//! expert-pack contracts, consumed by BOTH the dashboard RPCs
//! (`experts.list` / `experts.remove` / `experts.hooks_apply` /
//! `POST /api/experts/upload`) and the `duduclaw expert` CLI (which re-exports
//! these types so the two paths can never drift).
//!
//! Dependency direction: `duduclaw-cli` depends on `duduclaw-gateway`, so the
//! shared logic sinks HERE (not into `duduclaw-core` — the hooks state machine
//! needs [`crate::approval::ApprovalBroker`]).
//!
//! What deliberately does NOT live here: the install pipeline
//! (format detection, safe_zip extraction, prompt-injection / skill-security
//! scanning, agent scaffolding). It is entangled with cli-only scaffolding
//! helpers, so the gateway RPC reuses it by spawning
//! `duduclaw expert install <path>` as a subprocess (same pattern as
//! `doctor_probes` spawning `duduclaw mcp-server`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};

// ─────────────────────────── Install record ───────────────────────────

/// What kind of source produced an installed pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackKind {
    Native,
    ClaudePlugin,
    Skill,
}

impl PackKind {
    pub fn label(self) -> &'static str {
        match self {
            PackKind::Native => "expert.toml",
            PackKind::ClaudePlugin => "claude-plugin",
            PackKind::Skill => "agent-skill",
        }
    }
}

/// On-disk record of an install, written to
/// `<home>/experts/<slug>/install.json`. Drives `list` and `remove`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRecord {
    pub slug: String,
    pub kind: PackKind,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub version: String,
    /// Free-form description shown in list UIs (may be empty for packs
    /// installed before this field existed).
    #[serde(default)]
    pub description: String,
    /// Agent ids created by this install (removed on uninstall).
    #[serde(default)]
    pub agents: Vec<String>,
    /// Global skill dir names created by this install (removed on uninstall;
    /// skills that pre-existed are NOT recorded here so they survive removal).
    #[serde(default)]
    pub global_skills: Vec<String>,
    /// Wiki file paths (relative to `<home>/shared/wiki`) written by this
    /// install (removed on uninstall).
    #[serde(default)]
    pub wiki_files: Vec<String>,
    #[serde(default)]
    pub installed_at: String,
}

/// `<home>/experts`
pub fn experts_dir(home: &Path) -> PathBuf {
    home.join("experts")
}

fn record_path(home: &Path, slug: &str) -> PathBuf {
    experts_dir(home).join(slug).join("install.json")
}

/// Persist an install record (atomic temp + rename).
pub fn write_record(home: &Path, rec: &InstallRecord) -> Result<(), String> {
    let dir = experts_dir(home).join(&rec.slug);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建立 {} 失敗: {e}", dir.display()))?;
    let path = dir.join("install.json");
    let tmp = dir.join("install.json.tmp");
    let content =
        serde_json::to_string_pretty(rec).map_err(|e| format!("序列化 install.json 失敗: {e}"))?;
    std::fs::write(&tmp, content).map_err(|e| format!("寫入暫存檔失敗: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("覆寫 install.json 失敗: {e}")
    })?;
    Ok(())
}

/// Read all install records under `<home>/experts/*/install.json`.
pub fn list_records(home: &Path) -> Vec<InstallRecord> {
    let mut out = Vec::new();
    let dir = experts_dir(home);
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let rec = entry.path().join("install.json");
            if let Ok(content) = std::fs::read_to_string(&rec)
                && let Ok(r) = serde_json::from_str::<InstallRecord>(&content)
            {
                out.push(r);
            }
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

pub fn read_record(home: &Path, slug: &str) -> Option<InstallRecord> {
    let content = std::fs::read_to_string(record_path(home, slug)).ok()?;
    serde_json::from_str(&content).ok()
}

// ─────────────────────────── Hooks state machine ───────────────────────────

/// `action_kind` stored in `approvals.db` for hook-enable requests.
pub const HOOKS_ACTION_KIND: &str = "expert_hooks_enable";

/// Persisted hook-enablement status for one installed pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HooksStatus {
    /// Imported but not granted (default; also the post-deny/expiry state).
    Disabled,
    /// An ApprovalBroker request is filed and undecided.
    PendingApproval,
    /// Explicitly granted (`--trust-hooks` or an approved request).
    Enabled,
}

impl HooksStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            HooksStatus::Disabled => "disabled",
            HooksStatus::PendingApproval => "pending_approval",
            HooksStatus::Enabled => "enabled",
        }
    }
}

/// `<home>/experts/<slug>/hooks-state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HooksState {
    pub status: HooksStatus,
    /// The ApprovalBroker request id (kept after a terminal decision for
    /// audit cross-reference into `approvals.db`).
    #[serde(default)]
    pub approval_id: Option<String>,
    /// Relative file paths inside the hooks dir.
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub updated_at: String,
}

fn hooks_state_path(home: &Path, slug: &str) -> PathBuf {
    experts_dir(home).join(slug).join("hooks-state.json")
}

/// The quarantine location every import lands in.
pub fn hooks_disabled_dir(home: &Path, slug: &str) -> PathBuf {
    experts_dir(home).join(slug).join("hooks-disabled")
}

/// The active location hooks are promoted to on grant.
pub fn hooks_enabled_dir(home: &Path, slug: &str) -> PathBuf {
    experts_dir(home).join(slug).join("hooks")
}

/// Read the persisted state (`None` when absent / unparseable — treated as
/// "no managed hooks", never as a grant).
pub fn read_hooks_state(home: &Path, slug: &str) -> Option<HooksState> {
    let content = std::fs::read_to_string(hooks_state_path(home, slug)).ok()?;
    serde_json::from_str(&content).ok()
}

/// Persist the state atomically (temp + rename).
pub fn write_hooks_state(home: &Path, slug: &str, state: &HooksState) -> Result<(), String> {
    let dir = experts_dir(home).join(slug);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建立 {} 失敗: {e}", dir.display()))?;
    let path = hooks_state_path(home, slug);
    let tmp = dir.join("hooks-state.json.tmp");
    let content = serde_json::to_string_pretty(state)
        .map_err(|e| format!("序列化 hooks-state.json 失敗: {e}"))?;
    std::fs::write(&tmp, content).map_err(|e| format!("寫入暫存檔失敗: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("覆寫 hooks-state.json 失敗: {e}")
    })?;
    Ok(())
}

/// Recursive dir copy (skips symlinks — a pack must not smuggle links).
pub fn copy_dir(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let to = dest.join(entry.file_name());
        if ft.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else if ft.is_file() {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Copy the quarantined hooks into the active dir (grant application).
pub fn promote_hooks_enabled(home: &Path, slug: &str) -> Result<(), String> {
    let src = hooks_disabled_dir(home, slug);
    let dest = hooks_enabled_dir(home, slug);
    copy_dir(&src, &dest).map_err(|e| format!("啟用 hooks 複製失敗: {e}"))
}

/// ISO-8601 timestamp for state files.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Outcome of applying the current ApprovalBroker decision to a pack's hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HooksApplyOutcome {
    /// Hooks were (or already are) enabled.
    Enabled { files: usize },
    /// Hooks stay disabled (no grant / prior deny).
    Disabled,
    /// The approval request is still undecided.
    StillPending { approval_id: String },
    /// The approval was denied or expired — hooks stay disabled (fail-closed).
    DeniedOrExpired { status: &'static str },
}

/// Apply a decided approval to a pack's hooks — the exact semantics of
/// `duduclaw expert hooks <slug>`: approved → promote + Enabled;
/// denied / expired → Disabled (fail-closed); pending → unchanged.
/// Never enables without a grant.
pub async fn apply_hooks_decision(home: &Path, slug: &str) -> Result<HooksApplyOutcome, String> {
    let Some(state) = read_hooks_state(home, slug) else {
        return Err(format!("專家包 '{slug}' 沒有受管理的 hooks（或尚未安裝）"));
    };

    match state.status {
        HooksStatus::Enabled => Ok(HooksApplyOutcome::Enabled {
            files: state.files.len(),
        }),
        HooksStatus::Disabled => Ok(HooksApplyOutcome::Disabled),
        HooksStatus::PendingApproval => {
            let Some(id_str) = state.approval_id.clone() else {
                // Inconsistent state (pending without an id) ⇒ fail closed.
                let _ = write_hooks_state(
                    home,
                    slug,
                    &HooksState {
                        status: HooksStatus::Disabled,
                        updated_at: now_iso(),
                        ..state
                    },
                );
                return Err("hooks 狀態缺少審批編號，已改回停用（fail-closed）".into());
            };
            let broker = ApprovalBroker::open(home)?;
            let id = ApprovalId::from(id_str.clone());
            let status = broker.poll(&id).await?;
            match status {
                ApprovalStatus::Approved => {
                    promote_hooks_enabled(home, slug)?;
                    let file_count = state.files.len();
                    write_hooks_state(
                        home,
                        slug,
                        &HooksState {
                            status: HooksStatus::Enabled,
                            updated_at: now_iso(),
                            ..state
                        },
                    )?;
                    Ok(HooksApplyOutcome::Enabled { files: file_count })
                }
                ApprovalStatus::Denied | ApprovalStatus::Expired => {
                    write_hooks_state(
                        home,
                        slug,
                        &HooksState {
                            status: HooksStatus::Disabled,
                            updated_at: now_iso(),
                            ..state
                        },
                    )?;
                    tracing::warn!(
                        slug,
                        approval_id = %id_str,
                        status = status.as_str(),
                        "expert pack hooks approval not granted — hooks stay disabled"
                    );
                    Ok(HooksApplyOutcome::DeniedOrExpired {
                        status: if status == ApprovalStatus::Denied {
                            "denied"
                        } else {
                            "expired"
                        },
                    })
                }
                ApprovalStatus::Pending => Ok(HooksApplyOutcome::StillPending {
                    approval_id: id_str,
                }),
            }
        }
    }
}

// ─────────────────────────── Remove ───────────────────────────

/// Per-item outcome of a pack removal (structured — rendered by both the CLI
/// report printer and the dashboard RPC).
#[derive(Debug, Clone, Serialize)]
pub struct RemoveItem {
    /// "removed" | "skipped" | "missing"
    pub status: &'static str,
    /// "agent" | "skill" | "wiki"
    pub kind: &'static str,
    pub name: String,
    #[serde(default)]
    pub detail: String,
}

/// Remove an installed pack: its agents, pack-owned skills, and wiki pages
/// (fenced under `<home>/shared/wiki`), then the record dir itself. Assets
/// that pre-existed the install were never recorded, so they survive.
///
/// The exact semantics of `duduclaw expert remove <slug>`.
pub async fn remove_pack(home: &Path, slug: &str) -> Result<Vec<RemoveItem>, String> {
    let Some(rec) = read_record(home, slug) else {
        return Err(format!(
            "找不到已安裝的專家包 '{slug}'（用 `duduclaw expert list` 查看）"
        ));
    };

    let mut items: Vec<RemoveItem> = Vec::new();

    // Agents.
    for agent in &rec.agents {
        if !duduclaw_core::is_valid_agent_id(agent) {
            items.push(RemoveItem {
                status: "skipped",
                kind: "agent",
                name: agent.clone(),
                detail: "非法 agent id，未刪除".into(),
            });
            continue;
        }
        let dir = home.join("agents").join(agent);
        if dir.exists() {
            match tokio::fs::remove_dir_all(&dir).await {
                Ok(()) => items.push(RemoveItem {
                    status: "removed",
                    kind: "agent",
                    name: agent.clone(),
                    detail: String::new(),
                }),
                Err(e) => items.push(RemoveItem {
                    status: "skipped",
                    kind: "agent",
                    name: agent.clone(),
                    detail: format!("刪除失敗: {e}"),
                }),
            }
        } else {
            items.push(RemoveItem {
                status: "missing",
                kind: "agent",
                name: agent.clone(),
                detail: "目錄不存在".into(),
            });
        }
    }

    // Pack-owned global skills.
    for skill in &rec.global_skills {
        if !duduclaw_agent::skill_loader::is_safe_skill_name(skill) {
            items.push(RemoveItem {
                status: "skipped",
                kind: "skill",
                name: skill.clone(),
                detail: "非安全名稱，未刪除".into(),
            });
            continue;
        }
        let dir = home.join("skills").join(skill);
        if dir.exists() {
            match tokio::fs::remove_dir_all(&dir).await {
                Ok(()) => items.push(RemoveItem {
                    status: "removed",
                    kind: "skill",
                    name: skill.clone(),
                    detail: String::new(),
                }),
                Err(e) => items.push(RemoveItem {
                    status: "skipped",
                    kind: "skill",
                    name: skill.clone(),
                    detail: format!("刪除失敗: {e}"),
                }),
            }
        } else {
            items.push(RemoveItem {
                status: "missing",
                kind: "skill",
                name: skill.clone(),
                detail: "不存在".into(),
            });
        }
    }

    // Wiki files (fenced under shared/wiki — a record listing `../…` paths
    // must never delete outside the wiki root).
    let wiki_root = home.join("shared").join("wiki");
    let wiki_canon = wiki_root.canonicalize().ok();
    for rel in &rec.wiki_files {
        let path = wiki_root.join(rel);
        let safe = wiki_canon
            .as_ref()
            .zip(path.parent().and_then(|p| p.canonicalize().ok()))
            .map(|(root, parent)| parent.starts_with(root))
            .unwrap_or(false);
        if !safe {
            items.push(RemoveItem {
                status: "skipped",
                kind: "wiki",
                name: rel.clone(),
                detail: "路徑逃逸 wiki 圍欄，未刪除".into(),
            });
            continue;
        }
        if path.exists() {
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    items.push(RemoveItem {
                        status: "removed",
                        kind: "wiki",
                        name: rel.clone(),
                        detail: String::new(),
                    });
                    // Prune now-empty parent dirs up to the wiki fence
                    // (`remove_dir` refuses non-empty dirs, so shared
                    // namespaces that still hold other packs' pages survive).
                    let mut parent = path.parent().map(|p| p.to_path_buf());
                    while let Some(dir) = parent {
                        if dir == wiki_root || std::fs::remove_dir(&dir).is_err() {
                            break;
                        }
                        parent = dir.parent().map(|p| p.to_path_buf());
                    }
                }
                Err(e) => items.push(RemoveItem {
                    status: "skipped",
                    kind: "wiki",
                    name: rel.clone(),
                    detail: format!("刪除失敗: {e}"),
                }),
            }
        } else {
            items.push(RemoveItem {
                status: "missing",
                kind: "wiki",
                name: rel.clone(),
                detail: "不存在".into(),
            });
        }
    }

    // The record dir itself.
    let rec_dir = experts_dir(home).join(slug);
    let _ = std::fs::remove_dir_all(&rec_dir);

    Ok(items)
}

// ─────────────────────────── Upload fencing ───────────────────────────

/// Max accepted expert-pack upload (50 MiB — matches the safe_zip cap).
pub const MAX_EXPERT_UPLOAD_BYTES: usize = 50 * 1024 * 1024;

/// `<home>/tmp/expert-uploads` — staging area for dashboard uploads.
pub fn upload_dir(home: &Path) -> PathBuf {
    home.join("tmp").join("expert-uploads")
}

/// Sanitize a client-supplied filename into a safe basename: strips any
/// directory components (zip-slip via `../evil.zip` or absolute paths),
/// keeps only `[A-Za-z0-9._-]`, and guarantees a non-empty `.zip` name.
pub fn sanitize_upload_name(raw: &str) -> String {
    // Take the last path component under BOTH separator conventions —
    // a Windows client may send `C:\dir\pack.zip`.
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches('.');
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').to_string();
    if cleaned.is_empty() || cleaned == ".zip" {
        "pack.zip".to_string()
    } else if cleaned.to_ascii_lowercase().ends_with(".zip") {
        cleaned
    } else {
        format!("{cleaned}.zip")
    }
}

/// Build the on-disk staging path for an upload: always inside
/// [`upload_dir`], always a fresh uuid-prefixed name (no overwrite races,
/// no traversal — the client name only contributes a sanitized suffix).
pub fn staged_upload_path(home: &Path, client_name: &str) -> PathBuf {
    let name = sanitize_upload_name(client_name);
    upload_dir(home).join(format!("{}-{}", uuid::Uuid::new_v4(), name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(slug: &str) -> InstallRecord {
        InstallRecord {
            slug: slug.to_string(),
            kind: PackKind::Native,
            display_name: format!("{slug} 顯示名"),
            version: "1.0.0".into(),
            description: String::new(),
            agents: vec![],
            global_skills: vec![],
            wiki_files: vec![],
            installed_at: now_iso(),
        }
    }

    // ── On-disk contract pinning (drift guard vs the CLI expert module) ──

    #[test]
    fn install_record_round_trip_and_paths() {
        let home = tempfile::tempdir().unwrap();
        let rec = record("sales-team");
        write_record(home.path(), &rec).expect("write");
        // Path contract: <home>/experts/<slug>/install.json
        assert!(home
            .path()
            .join("experts/sales-team/install.json")
            .is_file());
        let rows = list_records(home.path());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].slug, "sales-team");
        assert_eq!(rows[0].kind, PackKind::Native);
        // kind serializes kebab-case — the same wire shape the CLI writes.
        let raw = std::fs::read_to_string(home.path().join("experts/sales-team/install.json"))
            .unwrap();
        assert!(raw.contains("\"native\""), "kebab-case kind: {raw}");
    }

    #[test]
    fn hooks_state_round_trip_and_path_contract() {
        let home = tempfile::tempdir().unwrap();
        let st = HooksState {
            status: HooksStatus::PendingApproval,
            approval_id: Some("ap-1".into()),
            files: vec!["pre.sh".into()],
            updated_at: now_iso(),
        };
        write_hooks_state(home.path(), "p1", &st).expect("write");
        assert!(home.path().join("experts/p1/hooks-state.json").is_file());
        let read = read_hooks_state(home.path(), "p1").expect("read");
        assert_eq!(read.status, HooksStatus::PendingApproval);
        assert_eq!(read.approval_id.as_deref(), Some("ap-1"));
        // snake_case status on the wire (CLI-compatible).
        let raw = std::fs::read_to_string(home.path().join("experts/p1/hooks-state.json")).unwrap();
        assert!(raw.contains("pending_approval"), "snake_case: {raw}");
    }

    // ── Remove semantics ──

    #[tokio::test]
    async fn remove_pack_deletes_recorded_assets_and_fences_wiki() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        // Recorded agent + skill + wiki page, plus an escape attempt.
        std::fs::create_dir_all(h.join("agents/pack-agent")).unwrap();
        std::fs::create_dir_all(h.join("skills/pack-skill")).unwrap();
        std::fs::create_dir_all(h.join("shared/wiki/packs/demo")).unwrap();
        std::fs::write(h.join("shared/wiki/packs/demo/sop.md"), "sop").unwrap();
        std::fs::write(h.join("secret.txt"), "keep me").unwrap();
        let mut rec = record("demo");
        rec.agents = vec!["pack-agent".into(), "Bad Agent!".into()];
        rec.global_skills = vec!["pack-skill".into()];
        rec.wiki_files = vec!["packs/demo/sop.md".into(), "../../secret.txt".into()];
        write_record(h, &rec).unwrap();

        let items = remove_pack(h, "demo").await.expect("remove");

        assert!(!h.join("agents/pack-agent").exists(), "agent removed");
        assert!(!h.join("skills/pack-skill").exists(), "skill removed");
        assert!(!h.join("shared/wiki/packs/demo/sop.md").exists(), "wiki removed");
        // Empty namespace dirs pruned up to the wiki fence.
        assert!(!h.join("shared/wiki/packs").exists(), "empty namespace pruned");
        assert!(h.join("shared/wiki").exists(), "wiki root survives");
        // The traversal path must NOT delete outside the fence.
        assert!(h.join("secret.txt").exists(), "escape fenced");
        assert!(
            items
                .iter()
                .any(|i| i.kind == "wiki" && i.status == "skipped" && i.name.contains("secret")),
            "escape reported as skipped: {items:?}"
        );
        // Invalid agent id skipped, valid one removed.
        assert!(items
            .iter()
            .any(|i| i.kind == "agent" && i.status == "skipped" && i.name == "Bad Agent!"));
        // Record dir gone ⇒ list is empty.
        assert!(list_records(h).is_empty());
    }

    #[tokio::test]
    async fn remove_pack_unknown_slug_errors() {
        let home = tempfile::tempdir().unwrap();
        assert!(remove_pack(home.path(), "nope").await.is_err());
    }

    // ── Hooks apply semantics (fail-closed) ──

    #[tokio::test]
    async fn apply_hooks_no_state_errors_and_disabled_stays_disabled() {
        let home = tempfile::tempdir().unwrap();
        assert!(apply_hooks_decision(home.path(), "ghost").await.is_err());

        write_hooks_state(
            home.path(),
            "p",
            &HooksState {
                status: HooksStatus::Disabled,
                approval_id: None,
                files: vec!["h.sh".into()],
                updated_at: now_iso(),
            },
        )
        .unwrap();
        let out = apply_hooks_decision(home.path(), "p").await.unwrap();
        assert_eq!(out, HooksApplyOutcome::Disabled);
        assert!(!hooks_enabled_dir(home.path(), "p").exists(), "never promoted");
    }

    #[tokio::test]
    async fn apply_hooks_pending_without_id_fails_closed_to_disabled() {
        let home = tempfile::tempdir().unwrap();
        write_hooks_state(
            home.path(),
            "p",
            &HooksState {
                status: HooksStatus::PendingApproval,
                approval_id: None,
                files: vec![],
                updated_at: now_iso(),
            },
        )
        .unwrap();
        assert!(apply_hooks_decision(home.path(), "p").await.is_err());
        let st = read_hooks_state(home.path(), "p").unwrap();
        assert_eq!(st.status, HooksStatus::Disabled, "fail-closed");
    }

    #[tokio::test]
    async fn apply_hooks_approved_promotes_and_enables() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        // Quarantined hook file.
        std::fs::create_dir_all(hooks_disabled_dir(h, "p")).unwrap();
        std::fs::write(hooks_disabled_dir(h, "p").join("pre.sh"), "echo hi").unwrap();
        // File an approval and approve it.
        let broker = ApprovalBroker::open(h).unwrap();
        let id = broker
            .request("p", HOOKS_ACTION_KIND, "enable", serde_json::json!({}), 3600)
            .await
            .unwrap();
        broker.decide(&id, true, "tester").await.unwrap();
        write_hooks_state(
            h,
            "p",
            &HooksState {
                status: HooksStatus::PendingApproval,
                approval_id: Some(id.to_string()),
                files: vec!["pre.sh".into()],
                updated_at: now_iso(),
            },
        )
        .unwrap();

        let out = apply_hooks_decision(h, "p").await.unwrap();
        assert_eq!(out, HooksApplyOutcome::Enabled { files: 1 });
        assert!(hooks_enabled_dir(h, "p").join("pre.sh").is_file(), "promoted");
        assert_eq!(
            read_hooks_state(h, "p").unwrap().status,
            HooksStatus::Enabled
        );
    }

    #[tokio::test]
    async fn apply_hooks_denied_stays_disabled() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        std::fs::create_dir_all(hooks_disabled_dir(h, "p")).unwrap();
        std::fs::write(hooks_disabled_dir(h, "p").join("pre.sh"), "echo hi").unwrap();
        let broker = ApprovalBroker::open(h).unwrap();
        let id = broker
            .request("p", HOOKS_ACTION_KIND, "enable", serde_json::json!({}), 3600)
            .await
            .unwrap();
        broker.decide(&id, false, "tester").await.unwrap();
        write_hooks_state(
            h,
            "p",
            &HooksState {
                status: HooksStatus::PendingApproval,
                approval_id: Some(id.to_string()),
                files: vec!["pre.sh".into()],
                updated_at: now_iso(),
            },
        )
        .unwrap();

        let out = apply_hooks_decision(h, "p").await.unwrap();
        assert_eq!(out, HooksApplyOutcome::DeniedOrExpired { status: "denied" });
        assert!(!hooks_enabled_dir(h, "p").exists(), "never promoted");
        assert_eq!(
            read_hooks_state(h, "p").unwrap().status,
            HooksStatus::Disabled
        );
    }

    // ── Upload fencing ──

    #[test]
    fn sanitize_upload_name_strips_traversal_and_forces_zip() {
        assert_eq!(sanitize_upload_name("pack.zip"), "pack.zip");
        assert_eq!(sanitize_upload_name("../../etc/passwd"), "passwd.zip");
        assert_eq!(sanitize_upload_name("..\\..\\evil.zip"), "evil.zip");
        assert_eq!(sanitize_upload_name("/abs/path/team.zip"), "team.zip");
        assert_eq!(sanitize_upload_name("C:\\dir\\pack.zip"), "pack.zip");
        assert_eq!(sanitize_upload_name(""), "pack.zip");
        assert_eq!(sanitize_upload_name("...."), "pack.zip");
        assert_eq!(sanitize_upload_name("含中文 名.zip"), "_____.zip");
        // No separators can survive.
        for bad in ["a/../b.zip", "a\\b\\c.zip", "../x"] {
            let out = sanitize_upload_name(bad);
            assert!(!out.contains('/') && !out.contains('\\'), "{bad} → {out}");
        }
    }

    #[test]
    fn staged_upload_path_always_inside_upload_dir() {
        let home = tempfile::tempdir().unwrap();
        for name in ["../../escape.zip", "/etc/passwd", "ok.zip", "..\\win.zip"] {
            let p = staged_upload_path(home.path(), name);
            assert!(
                p.starts_with(upload_dir(home.path())),
                "{name} escaped: {}",
                p.display()
            );
        }
    }
}
