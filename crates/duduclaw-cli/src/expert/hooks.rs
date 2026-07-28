//! Expert-pack hooks lifecycle — ApprovalBroker-gated enablement.
//!
//! Imported hooks are a supply-chain risk (arbitrary commands wired into the
//! agent runtime), so they are copied **disabled** to
//! `<home>/experts/<slug>/hooks-disabled/` and promoted to the enabled
//! location `<home>/experts/<slug>/hooks/` only through an explicit grant:
//!
//! - `duduclaw expert install --trust-hooks …` — the operator's explicit CLI
//!   grant (same convention as the codex / claude plugin `--trust` flag), or
//! - an `ApprovalBroker` approval (`approvals.db`, surfaced in the dashboard
//!   approval center via `approvals.list` / decided via `approvals.decide`)
//!   applied afterwards by `duduclaw expert hooks <slug>`.
//!
//! Fail-closed throughout: no grant, a denied approval, or a TTL-expired
//! approval all leave the hooks disabled. The state machine is persisted in
//! `<home>/experts/<slug>/hooks-state.json` (disabled → pending_approval →
//! enabled | disabled). "Enabled" means the files are staged into the active
//! hooks dir and the state records the grant — DuDuClaw never wires pack
//! hooks into any runtime config implicitly.

use std::path::{Path, PathBuf};

use console::style;
use serde::{Deserialize, Serialize};

use duduclaw_core::error::Result;
use duduclaw_gateway::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};

use super::install::InstallCtx;
use super::{Report, cfg_err, copy_dir, io_err, now_iso};

/// `action_kind` stored in `approvals.db` for hook-enable requests. Unknown
/// kinds render verbatim in the dashboard approval center
/// (`governance::ApprovalKind::Other`), so no UI change is needed.
pub const HOOKS_ACTION_KIND: &str = "expert_hooks_enable";

/// Max chars of each hook file excerpt included in the approval summary.
const HOOK_EXCERPT_CHARS: usize = 200;

/// TTL for a hooks approval request (24 h). Expiry = DENY (fail-closed).
const HOOKS_APPROVAL_TTL_SECS: i64 = 86_400;

// ─────────────────────────── State machine ───────────────────────────

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

fn state_path(home: &Path, slug: &str) -> PathBuf {
    super::experts_dir(home).join(slug).join("hooks-state.json")
}

/// The quarantine location every import lands in.
pub fn disabled_dir(home: &Path, slug: &str) -> PathBuf {
    super::experts_dir(home).join(slug).join("hooks-disabled")
}

/// The active location hooks are promoted to on grant.
pub fn enabled_dir(home: &Path, slug: &str) -> PathBuf {
    super::experts_dir(home).join(slug).join("hooks")
}

/// Read the persisted state (`None` when absent / unparseable — treated as
/// "no managed hooks", never as a grant).
pub fn read_state(home: &Path, slug: &str) -> Option<HooksState> {
    let content = std::fs::read_to_string(state_path(home, slug)).ok()?;
    serde_json::from_str(&content).ok()
}

/// Persist the state atomically (temp + rename).
fn write_state(home: &Path, slug: &str, state: &HooksState) -> Result<()> {
    let dir = super::experts_dir(home).join(slug);
    std::fs::create_dir_all(&dir)
        .map_err(|e| io_err(format!("建立 {} 失敗: {e}", dir.display())))?;
    let path = state_path(home, slug);
    let tmp = dir.join("hooks-state.json.tmp");
    let content = serde_json::to_string_pretty(state)
        .map_err(|e| cfg_err(format!("序列化 hooks-state.json 失敗: {e}")))?;
    std::fs::write(&tmp, content).map_err(|e| io_err(format!("寫入暫存檔失敗: {e}")))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_err(format!("覆寫 hooks-state.json 失敗: {e}"))
    })?;
    Ok(())
}

// ─────────────────────────── Summary ───────────────────────────

/// Collect regular files under `dir` as sorted relative paths (skips links).
fn collect_hook_files(base: &Path, dir: &Path, out: &mut Vec<String>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            if ft.is_dir() {
                collect_hook_files(base, &path, out);
            } else if ft.is_file()
                && let Ok(rel) = path.strip_prefix(base)
            {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

fn list_hook_files(hooks_dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_hook_files(hooks_dir, hooks_dir, &mut files);
    files.sort();
    files
}

/// Human-readable zh-TW approval summary: file name + the first 200 chars of
/// each hook's content (CJK-safe truncation; the content is DATA — shown for
/// human review, never executed here).
fn build_summary(slug: &str, hooks_dir: &Path, files: &[String]) -> String {
    let mut out = format!(
        "專家包 {slug} 內含 {} 個 hooks，核准後才會啟用（過期視同拒絕）：",
        files.len()
    );
    for rel in files {
        let content = std::fs::read_to_string(hooks_dir.join(rel)).unwrap_or_default();
        let excerpt = duduclaw_core::truncate_chars(content.trim(), HOOK_EXCERPT_CHARS)
            .replace('\n', " ⏎ ");
        out.push_str(&format!("\n- {rel}: {excerpt}"));
    }
    out
}

/// Copy the quarantined hooks into the active dir (grant application).
fn promote_enabled(home: &Path, slug: &str) -> Result<()> {
    let src = disabled_dir(home, slug);
    let dest = enabled_dir(home, slug);
    copy_dir(&src, &dest).map_err(|e| io_err(format!("啟用 hooks 複製失敗: {e}")))
}

// ─────────────────────────── Import (install path) ───────────────────────────

/// Import a pack's `hooks/` directory. Called by both the native and the
/// Claude-plugin importers. Behavior:
///
/// - always quarantines to `hooks-disabled/`;
/// - `--trust-hooks` → enable immediately (explicit operator grant);
/// - otherwise → file an ApprovalBroker request and stay disabled
///   (fail-closed), printing how to grant later.
pub(super) async fn import_hooks(
    ctx: &InstallCtx,
    pack_dir: &Path,
    slug: &str,
    report: &mut Report,
) {
    let hooks_src = pack_dir.join("hooks");
    if !hooks_src.is_dir() {
        return;
    }
    let files = list_hook_files(&hooks_src);
    if files.is_empty() {
        return;
    }

    if ctx.dry_run {
        for f in &files {
            let detail = if ctx.trust_hooks {
                "將以 --trust-hooks 顯式放行啟用"
            } else {
                "將匯入但停用，並建立審批請求"
            };
            report.warning("hook", f, detail);
        }
        return;
    }

    // Quarantine copy (canonical storage; survives grant/deny either way).
    if let Err(e) = copy_dir(&hooks_src, &disabled_dir(&ctx.home, slug)) {
        report.skipped("hooks", slug, format!("複製失敗: {e}"));
        return;
    }

    if ctx.trust_hooks {
        // Explicit operator grant — the CLI equivalent of the plugin --trust
        // convention. Enable without filing an approval request.
        match promote_enabled(&ctx.home, slug) {
            Ok(()) => {
                let _ = write_state(
                    &ctx.home,
                    slug,
                    &HooksState {
                        status: HooksStatus::Enabled,
                        approval_id: None,
                        files: files.clone(),
                        updated_at: now_iso(),
                    },
                );
                for f in &files {
                    report.imported("hook", f);
                }
                println!(
                    "  {} hooks 已依 --trust-hooks 顯式放行並啟用。",
                    style("✓").green()
                );
            }
            Err(e) => {
                // Promotion failed ⇒ stays disabled (fail-closed).
                let _ = write_state(
                    &ctx.home,
                    slug,
                    &HooksState {
                        status: HooksStatus::Disabled,
                        approval_id: None,
                        files: files.clone(),
                        updated_at: now_iso(),
                    },
                );
                report.skipped("hooks", slug, format!("啟用失敗，維持停用: {e}"));
            }
        }
        return;
    }

    // No trust flag → file an approval request; hooks stay disabled.
    let summary = build_summary(slug, &hooks_src, &files);
    let payload = serde_json::json!({
        "slug": slug,
        "files": files,
        "action": "enable_pack_hooks",
    });
    let approval_id = match ApprovalBroker::open(&ctx.home) {
        Ok(broker) => match broker
            .request(slug, HOOKS_ACTION_KIND, &summary, payload, HOOKS_APPROVAL_TTL_SECS)
            .await
        {
            Ok(id) => Some(id.to_string()),
            Err(e) => {
                report.warning("hooks", slug, format!("建立審批請求失敗（維持停用）: {e}"));
                None
            }
        },
        Err(e) => {
            report.warning("hooks", slug, format!("開啟 approvals.db 失敗（維持停用）: {e}"));
            None
        }
    };
    let status = if approval_id.is_some() {
        HooksStatus::PendingApproval
    } else {
        HooksStatus::Disabled
    };
    let _ = write_state(
        &ctx.home,
        slug,
        &HooksState {
            status,
            approval_id: approval_id.clone(),
            files: files.clone(),
            updated_at: now_iso(),
        },
    );
    for f in &files {
        report.warning("hook", f, "已匯入但停用（fail-closed），待審批放行");
    }
    println!(
        "  {} 專家包內含 hooks，為防供應鏈攻擊已全部停用。放行方式：\n\
         \x20   1) 在 dashboard 審批中心核准後，執行 `duduclaw expert hooks {slug}` 套用\n\
         \x20   2) 或重新安裝時帶 `--trust-hooks` 顯式放行",
        style("ℹ").cyan()
    );
    if let Some(id) = approval_id {
        println!("     審批編號：{id}");
    }
}

// ─────────────────────────── `expert hooks <slug>` ───────────────────────────

/// Show the hooks state and apply a decided approval: approved → enable,
/// denied / expired → keep disabled (logged). Never enables without a grant.
pub async fn cmd_hooks(home: &Path, slug: &str) -> Result<()> {
    let Some(state) = read_state(home, slug) else {
        return Err(cfg_err(format!(
            "專家包 '{slug}' 沒有受管理的 hooks（或尚未安裝）"
        )));
    };

    match state.status {
        HooksStatus::Enabled => {
            println!(
                "\n  {} '{slug}' 的 hooks 已啟用（{} 個檔案）。\n",
                style("✓").green(),
                state.files.len()
            );
            Ok(())
        }
        HooksStatus::Disabled => {
            println!(
                "\n  {} '{slug}' 的 hooks 目前停用。若要啟用：重新安裝時帶 `--trust-hooks`，\n\
                 \x20   或請管理者在 dashboard 審批中心建立新的核准後再執行本指令。\n",
                style("ℹ").cyan()
            );
            Ok(())
        }
        HooksStatus::PendingApproval => {
            let Some(id_str) = state.approval_id.clone() else {
                // Inconsistent state (pending without an id) ⇒ fail closed.
                let _ = write_state(
                    home,
                    slug,
                    &HooksState {
                        status: HooksStatus::Disabled,
                        updated_at: now_iso(),
                        ..state
                    },
                );
                return Err(cfg_err(
                    "hooks 狀態缺少審批編號，已改回停用（fail-closed）".into(),
                ));
            };
            let broker = ApprovalBroker::open(home).map_err(cfg_err)?;
            let id = ApprovalId::from(id_str.clone());
            let status = broker.poll(&id).await.map_err(cfg_err)?;
            match status {
                ApprovalStatus::Approved => {
                    promote_enabled(home, slug)?;
                    let file_count = state.files.len();
                    write_state(
                        home,
                        slug,
                        &HooksState {
                            status: HooksStatus::Enabled,
                            updated_at: now_iso(),
                            ..state
                        },
                    )?;
                    println!(
                        "\n  {} 審批已核准，'{slug}' 的 hooks 已啟用（{file_count} 個檔案）。\n",
                        style("✓").green()
                    );
                    Ok(())
                }
                ApprovalStatus::Denied | ApprovalStatus::Expired => {
                    write_state(
                        home,
                        slug,
                        &HooksState {
                            status: HooksStatus::Disabled,
                            updated_at: now_iso(),
                            ..state
                        },
                    )?;
                    let verdict = if status == ApprovalStatus::Denied {
                        "已被拒絕"
                    } else {
                        "已逾期（視同拒絕）"
                    };
                    tracing::warn!(
                        slug,
                        approval_id = %id_str,
                        status = status.as_str(),
                        "expert pack hooks approval not granted — hooks stay disabled"
                    );
                    println!(
                        "\n  {} 審批{verdict}，'{slug}' 的 hooks 維持停用（fail-closed）。\n",
                        style("✗").red()
                    );
                    Ok(())
                }
                ApprovalStatus::Pending => {
                    println!(
                        "\n  {} 審批仍在等待決定（編號 {id_str}）。請在 dashboard 審批中心\n\
                         \x20   核准或拒絕後，再執行 `duduclaw expert hooks {slug}`。\n",
                        style("…").yellow()
                    );
                    Ok(())
                }
            }
        }
    }
}
