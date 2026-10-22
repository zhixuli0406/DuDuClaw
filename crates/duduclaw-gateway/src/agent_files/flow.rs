//! The dashboard save pipeline shared by `agent_files.save`,
//! `agent_files.rollback` and `contract.update`:
//!
//! `check` → rules → (AI review) → merged verdict → `pass` applies at once;
//! `warn` / `block` return a short-lived `analysis_id`. `confirm` (a reason;
//! for `block` or an incomplete review a current admin who acknowledges the
//! risk) or `request_approval` (non-admins) must match that record exactly.
//! They re-run the fixed rules at the caller's CURRENT permission level, never
//! the AI review; when the rules now come out stricter than the recorded
//! result, the change has to be checked again.
//!
//! A file that had been changed outside the dashboard (drifted baseline) is
//! checked as a whole: the injection scan reads the full text and the
//! reviewer compares against the last approved version.

use std::path::Path;
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_auth::UserContext;
use duduclaw_auth::models::AccessLevel;
use duduclaw_security::audit::Severity;
use serde_json::{Map, Value, json};
use tokio::sync::RwLock;

use super::rules::{self, RuleInput};
use super::verdict::{self, AiOutcome, Analysis, RuleLevel, Verdict};
use super::writer::{self, Baseline, Commit, CommitError, CommitMeta};
use super::{AgentFilesConfig, FileKey};

/// What the caller asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Check,
    Confirm,
    RequestApproval,
}

impl Action {
    pub(crate) fn parse(s: Option<&str>) -> Result<Self, String> {
        match s.unwrap_or("check") {
            "check" => Ok(Self::Check),
            "confirm" => Ok(Self::Confirm),
            "request_approval" => Ok(Self::RequestApproval),
            _ => Err("action must be check, confirm or request_approval".into()),
        }
    }
}

/// One save.
#[derive(Debug, Clone)]
pub(crate) struct SaveInput {
    pub agent_id: String,
    pub key: FileKey,
    pub content: String,
    pub base_hash: String,
    pub action: Action,
    pub analysis_id: Option<String>,
    pub reason: String,
    pub acknowledge_risk: bool,
    /// `dashboard`, `rollback` or `contract.update`.
    pub source: &'static str,
}

impl SaveInput {
    /// Read the common fields from RPC params (`content` is supplied by
    /// the caller: the text, a version, or a rendered contract).
    pub(crate) fn from_params(params: &Value, key: FileKey, content: String, base_hash: String, source: &'static str) -> Result<Self, String> {
        let s = |k: &str| params.get(k).and_then(Value::as_str);
        Ok(Self {
            agent_id: s("agent_id").unwrap_or("").to_string(),
            key,
            content,
            base_hash,
            action: Action::parse(s("action"))?,
            analysis_id: s("analysis_id").map(str::to_string),
            reason: s("reason").unwrap_or("").trim().to_string(),
            acknowledge_risk: params.get("acknowledge_risk").and_then(Value::as_bool).unwrap_or(false),
            source,
        })
    }
}

/// Who is saving: the identity re-read from `users.db` for this request, and
/// the session identity the writer re-checks inside the file lock.
pub(crate) struct Caller<'a> {
    pub home: &'a Path,
    pub registry: &'a Arc<RwLock<AgentRegistry>>,
    pub live: &'a UserContext,
    pub session: &'a UserContext,
}

/// The live access check every write uses: `users.db` is re-read (role,
/// active, password-change flag, bindings), never the session token.
pub(crate) fn require_live_agent_access(
    home: &Path,
    ctx: &UserContext,
    agent_id: &str,
    min: AccessLevel,
) -> Result<UserContext, String> {
    let fresh = crate::review_evidence::audience::fresh_dashboard_context(home, ctx)
        .map_err(|_| "permission denied".to_string())?;
    if !fresh.has_agent_access(agent_id, min) {
        return Err("permission denied".into());
    }
    Ok(fresh)
}

fn base(input: &SaveInput, status: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("status".into(), json!(status));
    m.insert("file".into(), json!(input.key.as_str()));
    m.insert("agent_id".into(), json!(input.agent_id));
    m
}

fn refused_audit(home: &Path, input: &SaveInput, user_id: &str, why: &str) {
    crate::security_autopilot::audit_and_emit(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            "agent_file_change_refused",
            input.agent_id.as_str(),
            Severity::Warning,
            json!({
                "agent_id": input.agent_id,
                "file": input.key.as_str(),
                "user_id": user_id,
                "source": input.source,
                "reason": duduclaw_core::truncate_chars(why, 300),
            }),
        ),
    );
}

struct Profile {
    display_name: String,
    role: String,
    department: String,
}

fn profile(home: &Path, agent_id: &str) -> Profile {
    let t: toml::Table = super::file_path(home, agent_id, FileKey::AgentToml)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or_default();
    let g = |k: &str| t.get("agent").and_then(|a| a.get(k)).and_then(|v| v.as_str()).unwrap_or("").to_string();
    Profile { display_name: g("display_name"), role: g("role"), department: g("department") }
}

/// The employee's current `[boundaries]` (lenient: an unreadable file has none).
fn current_contract(home: &Path, agent_id: &str) -> rules::ContractBounds {
    super::file_path(home, agent_id, FileKey::Contract)
        .ok()
        .and_then(|p| super::read_or_empty(&p).ok())
        .and_then(|t| rules::parse_contract(&t).ok())
        .unwrap_or_default()
}

fn ai_applies(key: FileKey, source: &str) -> bool {
    key.text_editable() && matches!(source, "dashboard" | "rollback" | "contract.update")
}

/// The red-team coverage of a contract, without calling a model: how many
/// generated attacks the input filter already blocks.
pub(crate) fn contract_coverage(text: &str) -> Value {
    let Ok(b) = rules::parse_contract(text) else { return Value::Null };
    let cases = crate::redteam::generate_attacks(&b.must_not, crate::redteam::Locale::ALL);
    let ledger = crate::redteam::build_ledger(&cases, |p| {
        let r = duduclaw_security::input_guard::scan_input(p, duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD);
        (r.blocked, r.risk_score, r.matched_rules)
    });
    let s = crate::redteam::summarize_ledger(&ledger);
    json!({ "covered": s.covered, "needs_live_validation": s.needs_validation, "units": s.units })
}

fn rejected(input: &SaveInput, findings: &[verdict::RuleFinding]) -> Value {
    let mut m = base(input, "rejected");
    let reasons: Vec<&verdict::Reason> = findings.iter().filter(|f| f.level == RuleLevel::Reject).map(|f| &f.reason).collect();
    m.insert(
        "review".into(),
        json!({
            "analysis_id": null, "verdict": "block", "basis": "rules", "summary": "",
            "reasons": reasons, "can_confirm": false, "can_request_approval": false,
        }),
    );
    Value::Object(m)
}

/// Run the pipeline. `Err` = a request the caller must not retry as is
/// (permission, a stale or mismatched confirm record, a missing reason).
pub(crate) async fn save(c: &Caller<'_>, input: SaveInput) -> Result<Value, String> {
    if !input.key.text_editable() {
        return Err("基本設定請在員工的編輯頁修改".into());
    }
    let path = super::file_path(c.home, &input.agent_id, input.key)?;
    let current = super::read_or_empty(&path)?;
    let current_hash = super::hash_of(&current);
    if input.base_hash != current_hash {
        refused_audit(c.home, &input, &c.live.user_id, "base_hash conflict");
        let mut m = base(&input, "conflict");
        m.insert("current_hash".into(), json!(current_hash));
        return Ok(Value::Object(m));
    }
    if input.content == current {
        let mut m = base(&input, "unchanged");
        m.insert("hash".into(), json!(current_hash));
        return Ok(Value::Object(m));
    }
    let is_admin = c.live.is_admin();
    let contract = current_contract(c.home, &input.agent_id);
    // Going back to exactly the approved version needs no outside-change
    // treatment: that text was already approved.
    let baseline = approved_baseline(c.home, &input.agent_id, input.key).filter(|b| b.text != input.content);
    let compare_with = baseline.as_ref().map(|b| b.text.as_str()).unwrap_or(&current);
    let mut findings = rules::check(&RuleInput {
        key: input.key,
        old: compare_with,
        new: &input.content,
        is_admin,
        must_not: &contract.must_not,
        full_scan: baseline.is_some(),
    });
    if baseline.is_some() {
        findings.push(verdict::RuleFinding {
            level: RuleLevel::Flag,
            reason: verdict::Reason::new(
                verdict::category::OUTSIDE_CHANGE,
                "medium",
                "這個檔案在系統外被改過，這次以整份內容檢查，並和上次核可的版本比較",
                "",
            ),
        });
    }
    let level = verdict::rule_level(&findings);
    if level == RuleLevel::Reject {
        refused_audit(c.home, &input, &c.live.user_id, "content rejected by the fixed rules");
        return Ok(rejected(&input, &findings));
    }
    match input.action {
        Action::Check => {
            let cfg = AgentFilesConfig::load(c.home);
            if !super::quota::allow_check(c.home, &c.live.user_id, cfg.checks_per_minute) {
                return Err("檢查得太頻繁了，請稍等一分鐘再試".into());
            }
            Box::pin(check(c, input, compare_with, findings, &contract, &cfg)).await
        }
        Action::Confirm => confirm(c, input, level).await,
        Action::RequestApproval => request(c, input, level).await,
    }
}

/// The last approved version of a SOUL / INSTRUCTIONS file that is
/// currently drifted, or `None` when the file is intact (or has no baseline
/// concept). An approved version no longer kept reads as empty, so the
/// reviewer sees the whole file as new.
struct Approved {
    text: String,
}

fn approved_baseline(home: &Path, agent_id: &str, key: FileKey) -> Option<Approved> {
    let doc = key.protected_doc()?;
    let dir = super::agent_dir(home, agent_id).ok()?;
    if duduclaw_security::soul_guard::doc_drift(doc, &dir) != duduclaw_security::soul_guard::DocDrift::Drifted {
        return None;
    }
    let text = duduclaw_security::soul_guard::read_doc_hash(doc, &dir)
        .and_then(|h| super::history::find_by_sha(home, agent_id, key, &h))
        .and_then(|v| super::history::read_version(home, agent_id, key, &v).ok())
        .unwrap_or_default();
    Some(Approved { text })
}

async fn check(
    c: &Caller<'_>,
    input: SaveInput,
    current: &str,
    findings: Vec<verdict::RuleFinding>,
    contract: &rules::ContractBounds,
    cfg: &AgentFilesConfig,
) -> Result<Value, String> {
    let rule_reasons: Vec<verdict::Reason> = findings.iter().map(|f| f.reason.clone()).collect();
    let ai = if ai_applies(input.key, input.source) {
        let p = profile(c.home, &input.agent_id);
        let flagged = verdict::categories(&rule_reasons);
        Box::pin(super::review::review(
            &super::review::ReviewInput {
                home: c.home,
                user_id: &c.live.user_id,
                agent_id: &input.agent_id,
                display_name: &p.display_name,
                role: &p.role,
                department: &p.department,
                key: input.key,
                old: current,
                new: &input.content,
                must_not: &contract.must_not,
                must_always: &contract.must_always,
                flagged: &flagged,
            },
            cfg,
        ))
        .await
    } else {
        AiOutcome::NotApplicable
    };
    let level = verdict::rule_level(&findings);
    let final_verdict = verdict::merge(level, &ai, cfg.on_analysis_failure);
    let mut reasons = rule_reasons;
    let mut summary = String::new();
    match &ai {
        AiOutcome::Done { summary: s, reasons: r, partial, .. } => {
            summary = s.clone();
            reasons.extend(r.iter().cloned());
            if *partial {
                reasons.push(verdict::partial_reason());
            }
        }
        AiOutcome::Failed(why) => {
            tracing::info!(agent = %input.agent_id, file = input.key.as_str(), why = %why, "agent file review unavailable; counted as at least a warning");
            reasons.push(verdict::unavailable_reason());
        }
        AiOutcome::NotApplicable => {}
    }
    let analysis = Analysis {
        user_id: c.live.user_id.clone(),
        agent_id: input.agent_id.clone(),
        file: input.key,
        base_hash: input.base_hash.clone(),
        content_sha256: super::hash_of(&input.content),
        verdict: final_verdict,
        basis: verdict::basis(&ai),
        summary,
        reasons,
        source: input.source,
    };
    if final_verdict == Verdict::Pass {
        return apply(c, &input, &analysis, None, Severity::Info, false).await;
    }
    let id = verdict::remember(analysis.clone());
    let status = if final_verdict == Verdict::Block { "blocked" } else { "needs_confirmation" };
    let mut m = base(&input, status);
    m.insert("review".into(), verdict::review_json(Some(&id), &analysis, c.live.is_admin()));
    Ok(Value::Object(m))
}

fn recall(c: &Caller<'_>, input: &SaveInput) -> Result<(String, Analysis), String> {
    let id = input.analysis_id.clone().ok_or("analysis_id is required")?;
    let sha = super::hash_of(&input.content);
    match verdict::recall(&id, &c.live.user_id, &input.agent_id, input.key, &input.base_hash, &sha) {
        Ok(a) => Ok((id, a)),
        Err(e) => {
            refused_audit(c.home, input, &c.live.user_id, "confirm record mismatch");
            Err(e)
        }
    }
}

fn require_reason(input: &SaveInput) -> Result<(), String> {
    if input.reason.chars().count() < 4 {
        return Err("請說明為什麼要這樣改（至少 4 個字）".into());
    }
    Ok(())
}

/// The rules, re-run now at the caller's current permission level, may not
/// be stricter than what was checked (a permission lost in between, or a
/// rule that changed): then the change needs a new check.
fn still_matches(c: &Caller<'_>, input: &SaveInput, analysis: &Analysis, level_now: RuleLevel) -> Result<(), String> {
    if verdict::rules_verdict(level_now) > analysis.verdict {
        refused_audit(c.home, input, &c.live.user_id, "rules stricter than at check time");
        return Err("你的權限或規則和檢查時不同，請重新檢查".into());
    }
    Ok(())
}

async fn confirm(c: &Caller<'_>, input: SaveInput, level_now: RuleLevel) -> Result<Value, String> {
    let (id, analysis) = recall(c, &input)?;
    still_matches(c, &input, &analysis, level_now)?;
    require_reason(&input)?;
    let admin = c.live.is_admin();
    let incomplete = verdict::incomplete(&analysis.reasons);
    // A block or an incomplete review needs a CURRENT admin who acknowledges
    // the risk; the admin check is repeated inside the file lock.
    let need_admin = analysis.verdict == Verdict::Block || incomplete;
    if need_admin && !admin {
        refused_audit(c.home, &input, &c.live.user_id, "non-admin tried to confirm a block or an incomplete review");
        return Err(if analysis.verdict == Verdict::Block {
            "高風險的修改只能送給管理員審核".into()
        } else {
            "自動檢查這次沒有完整完成，只能送給管理員審核".into()
        });
    }
    if need_admin && !input.acknowledge_risk {
        return Err("這次修改需要勾選「我了解風險」才能套用".into());
    }
    let severity = if analysis.verdict == Verdict::Block { Severity::Critical } else { Severity::Warning };
    let reason = duduclaw_core::truncate_chars(&input.reason, MAX_REASON_CHARS);
    let out = apply(c, &input, &analysis, Some(reason), severity, need_admin).await;
    verdict::forget(&id);
    out
}

/// Longest reason kept on a confirmation or an approval request.
const MAX_REASON_CHARS: usize = 1000;

async fn request(c: &Caller<'_>, input: SaveInput, level_now: RuleLevel) -> Result<Value, String> {
    let (id, analysis) = recall(c, &input)?;
    if c.live.is_admin() {
        return Err("管理員可以直接確認套用".into());
    }
    if analysis.verdict == Verdict::Pass {
        return Err("這次修改不需要審核".into());
    }
    still_matches(c, &input, &analysis, level_now)?;
    require_reason(&input)?;
    let reason = duduclaw_core::truncate_chars(&input.reason, MAX_REASON_CHARS);
    let approval_id = super::approval::file_request(c.home, &analysis, &input.content, &c.live.email, &reason).await?;
    verdict::forget(&id);
    let mut m = base(&input, "pending_approval");
    m.insert("approval_id".into(), json!(approval_id));
    m.insert("review".into(), verdict::review_json(None, &analysis, false));
    Ok(Value::Object(m))
}

async fn apply(
    c: &Caller<'_>,
    input: &SaveInput,
    analysis: &Analysis,
    confirm_reason: Option<String>,
    severity: Severity,
    need_admin: bool,
) -> Result<Value, String> {
    let mut meta = CommitMeta::new(&c.live.user_id, input.source);
    meta.verdict = Some(analysis.verdict);
    meta.basis = Some(analysis.basis);
    meta.reason_categories = verdict::categories(&analysis.reasons);
    meta.confirm_reason = confirm_reason;
    meta.severity = severity;
    let home = c.home.to_path_buf();
    let session = c.session.clone();
    let agent = input.agent_id.clone();
    let content = input.content.clone();
    let result = writer::commit(
        c.registry,
        Commit {
            home: c.home,
            agent_id: &input.agent_id,
            key: input.key,
            expected_hash: Some(&input.base_hash),
            meta,
            baseline: Baseline::Accept,
        },
        move |_| Ok(content),
        move || {
            let live = require_live_agent_access(&home, &session, &agent, AccessLevel::Owner)?;
            if need_admin && !live.is_admin() {
                return Err("permission denied".into());
            }
            Ok(())
        },
    )
    .await;
    match result {
        Ok(o) => {
            let mut m = base(input, if o.unchanged { "unchanged" } else { "applied" });
            m.insert("hash".into(), json!(o.hash));
            m.insert("version_id".into(), json!(o.version_id));
            m.insert("review".into(), verdict::review_json(None, analysis, c.live.is_admin()));
            m.insert("effective".into(), json!({ "registry_reloaded": o.registry_reloaded, "applies_from": "next_turn" }));
            if input.key == FileKey::Contract {
                m.insert("contract_coverage".into(), contract_coverage(&o.after));
            }
            Ok(Value::Object(m))
        }
        Err(CommitError::Conflict { current_hash }) => {
            refused_audit(c.home, input, &c.live.user_id, "base_hash conflict");
            let mut m = base(input, "conflict");
            m.insert("current_hash".into(), json!(current_hash));
            Ok(Value::Object(m))
        }
        Err(e) => {
            refused_audit(c.home, input, &c.live.user_id, &e.message());
            Err(e.message())
        }
    }
}
