//! Staged LLM generation of a custom AI team (`experts.generate`).
//!
//! One JSON reply used to carry the whole team, which capped it at one skill
//! and a page or two of knowledge. Generation now runs in stages:
//!
//! - **A — roster**: names, roles, responsibilities, reporting lines.
//! - **B — one call per member**: SOUL with explicit depth requirements, the
//!   member's own skill(s), and CONTRACT rules derived from the SOUL's red
//!   lines (same call, so the contract restates the SOUL's own red lines).
//! - **C — knowledge base**: ≥ 4 shared wiki pages (SOP, glossary,
//!   compliance, FAQ).
//!
//! B and C run with bounded concurrency ([`STAGE_CONCURRENCY`]). Each stage
//! validates its JSON and gets exactly one retry with the problems fed back.
//! A member or the wiki failing twice does not discard the rest: the member
//! falls back to a placeholder persona built from the roster, and the result
//! carries a note per fallback (reported as a partial draft). Only a roster
//! failure, or every member failing, fails the whole run.
//!
//! The model call itself sits behind [`StageLlm`] so the orchestration is
//! unit-testable without a model.

pub mod contract;
pub mod example;
pub mod job;
pub mod prompts;
pub mod report;
pub mod schema;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;

use futures_util::StreamExt;

use crate::expert_generate::{
    GenerateRequest, GeneratedAgent, GeneratedContract, GeneratedPack, GeneratedSkill,
    GeneratedWiki,
};

use job::{ProgressSink, StepStatus};
use schema::{MemberContent, RosterMember, RosterPlan};

/// Model calls in flight at once during stages B and C.
pub const STAGE_CONCURRENCY: usize = 2;

/// Which stage a model call belongs to (drives output budget and tools).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Roster,
    Member,
    Wiki,
}

impl Stage {
    /// Output-token budget for the Direct API fallback. Sized so each
    /// stage's reply fits: a roster is small; one member's SOUL + skills runs
    /// 4–8 k CJK tokens; four or more wiki pages run 6–12 k.
    pub fn max_tokens(self) -> u32 {
        match self {
            Stage::Roster => 4096,
            Stage::Member => 12_000,
            Stage::Wiki => 16_000,
        }
    }
}

/// One model call. Implementations: the rotated Claude CLI with Direct API
/// fallback (`handlers::experts_rpc`), and canned replies in tests.
// `async_trait` expands to a boxed future the lint already treats as must-use.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait StageLlm: Send + Sync {
    async fn complete(&self, stage: Stage, system: &str, user: &str) -> Result<String, String>;
}

/// Inputs for one staged run.
pub struct StagedRequest<'a> {
    pub req: &'a GenerateRequest,
    /// A cached industry pack's manifest, used as an extra roster example.
    pub installed_example: Option<String>,
    /// The previous accepted draft (revise rounds).
    pub prior: Option<&'a GeneratedPack>,
    pub feedback: Option<&'a str>,
}

#[derive(Debug)]
pub struct StagedResult {
    pub pack: GeneratedPack,
    /// One line per fallback (empty ⇒ every stage succeeded).
    pub notes: Vec<String>,
}

/// Run the same content scan the CLI installer applies (prompt
/// injection guard + skill security scanner). `None` ⇒ clean; `Some(reason)`
/// ⇒ the installer would skip this item.
pub fn scan_pack_content(content: &str) -> Option<String> {
    let inj = duduclaw_security::input_guard::scan_input(
        content,
        duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD,
    );
    if inj.blocked {
        return Some(format!(
            "prompt-injection risk {} (rules: {})",
            inj.risk_score,
            inj.matched_rules.join("/")
        ));
    }
    let scan = crate::skill_lifecycle::security_scanner::scan_skill(content, None);
    if !scan.passed {
        let cats: Vec<String> = scan
            .findings
            .iter()
            .map(|f| format!("{:?}", f.category))
            .collect();
        return Some(format!(
            "security scan {:?} (findings: {})",
            scan.risk_level,
            cats.join("/")
        ));
    }
    None
}

/// Why one attempt failed: a transport error retries unchanged, a content
/// problem retries with the problems fed back.
enum AttemptError {
    Transport(String),
    Invalid(Vec<String>),
}

impl AttemptError {
    fn summary(&self) -> String {
        match self {
            AttemptError::Transport(e) => e.clone(),
            AttemptError::Invalid(p) => p.join("；"),
        }
    }
}

/// Run `attempt` once, and once more on failure (feeding back validation
/// problems). Returns the final error summary on a second failure.
async fn twice<T, F, Fut>(mut attempt: F) -> Result<T, String>
where
    F: FnMut(Vec<String>) -> Fut,
    Fut: Future<Output = Result<T, AttemptError>>,
{
    match attempt(Vec::new()).await {
        Ok(v) => Ok(v),
        Err(first) => {
            let feedback = match &first {
                AttemptError::Invalid(p) => p.clone(),
                AttemptError::Transport(_) => Vec::new(),
            };
            attempt(feedback).await.map_err(|e| e.summary())
        }
    }
}

async fn run_roster(
    llm: &dyn StageLlm,
    input: &StagedRequest<'_>,
) -> Result<RosterPlan, String> {
    let prior = input
        .prior
        .map(|p| prompts::prior_roster_digest(&p.slug, &p.display_name, &p.agents));
    twice(|errors| {
        let (system, user) = prompts::roster_prompt(
            input.req,
            input.installed_example.as_deref(),
            prior.as_deref(),
            input.feedback,
            &errors,
        );
        async move {
            let raw = llm
                .complete(Stage::Roster, &system, &user)
                .await
                .map_err(AttemptError::Transport)?;
            let plan = schema::parse_roster(&raw).map_err(|e| AttemptError::Invalid(vec![e]))?;
            let problems = schema::validate_roster(&plan, input.req.team_size);
            if problems.is_empty() {
                Ok(plan)
            } else {
                Err(AttemptError::Invalid(problems))
            }
        }
    })
    .await
}

async fn run_member(
    llm: &dyn StageLlm,
    input: &StagedRequest<'_>,
    plan: &RosterPlan,
    member: &RosterMember,
) -> Result<MemberContent, String> {
    let prior = input.prior.and_then(|p| {
        p.agents
            .iter()
            .find(|a| a.name == member.name)
            .map(|a| prompts::prior_member_digest(a, &p.skills))
    });
    twice(|errors| {
        let (system, user) = prompts::member_prompt(
            input.req,
            plan,
            member,
            prior.as_deref(),
            input.feedback,
            &errors,
        );
        async move {
            let raw = llm
                .complete(Stage::Member, &system, &user)
                .await
                .map_err(AttemptError::Transport)?;
            let mc = schema::parse_member(&raw).map_err(|e| AttemptError::Invalid(vec![e]))?;
            schema::validate_member(member, mc).map_err(AttemptError::Invalid)
        }
    })
    .await
}

async fn run_wiki(
    llm: &dyn StageLlm,
    input: &StagedRequest<'_>,
    plan: &RosterPlan,
) -> Result<Vec<GeneratedWiki>, String> {
    let prior = input.prior.map(|p| prompts::prior_wiki_digest(&p.wiki));
    twice(|errors| {
        let (system, user) = prompts::wiki_prompt(
            input.req,
            plan,
            prior.as_deref(),
            input.feedback,
            &errors,
            input.req.research,
        );
        async move {
            let raw = llm
                .complete(Stage::Wiki, &system, &user)
                .await
                .map_err(AttemptError::Transport)?;
            let bundle = schema::parse_wiki(&raw).map_err(|e| AttemptError::Invalid(vec![e]))?;
            schema::validate_wiki(&bundle).map_err(AttemptError::Invalid)
        }
    })
    .await
}

enum ContentOutcome {
    Member(usize, Result<MemberContent, String>),
    Wiki(Result<Vec<GeneratedWiki>, String>),
}

type ContentFuture<'a> = Pin<Box<dyn Future<Output = ContentOutcome> + Send + 'a>>;

/// Run the whole staged generation. `Err` only when the roster fails or no
/// member could be written; otherwise a (possibly partial) pack.
pub async fn run_staged(
    llm: &dyn StageLlm,
    input: &StagedRequest<'_>,
    sink: &ProgressSink,
) -> Result<StagedResult, String> {
    sink.update(|r| r.progress.roster = StepStatus::Running);
    let plan = match run_roster(llm, input).await {
        Ok(p) => p,
        Err(e) => {
            sink.update(|r| r.progress.roster = StepStatus::Failed);
            return Err(format!("團隊名單生成失敗（已重試 1 次）：{e}"));
        }
    };
    sink.update(|r| {
        r.progress.roster = StepStatus::Done;
        r.progress.members = plan
            .members
            .iter()
            .map(|m| job::MemberProgress {
                name: m.name.clone(),
                display_name: m.label().to_string(),
                ..Default::default()
            })
            .collect();
    });

    // Wiki first: it is the longest single call, so it starts in wave one.
    let mut futs: Vec<ContentFuture<'_>> = Vec::with_capacity(plan.members.len() + 1);
    let plan_ref = &plan;
    futs.push(Box::pin(async move {
        sink.update(|r| r.progress.wiki = StepStatus::Running);
        let res = run_wiki(llm, input, plan_ref).await;
        let st = if res.is_ok() { StepStatus::Done } else { StepStatus::Failed };
        sink.update(|r| r.progress.wiki = st);
        ContentOutcome::Wiki(res)
    }));
    for (idx, member) in plan.members.iter().enumerate() {
        futs.push(Box::pin(async move {
            sink.set_member(idx, StepStatus::Running, "");
            let res = run_member(llm, input, plan_ref, member).await;
            match &res {
                Ok(_) => sink.set_member(idx, StepStatus::Done, ""),
                Err(e) => sink.set_member(idx, StepStatus::Failed, e),
            }
            ContentOutcome::Member(idx, res)
        }));
    }
    let outcomes: Vec<ContentOutcome> = futures_util::stream::iter(futs)
        .buffer_unordered(STAGE_CONCURRENCY)
        .collect()
        .await;

    assemble(&plan, outcomes)
}

/// Merge stage outcomes into one pack (pure).
fn assemble(plan: &RosterPlan, outcomes: Vec<ContentOutcome>) -> Result<StagedResult, String> {
    let mut contents: Vec<Option<Result<MemberContent, String>>> =
        (0..plan.members.len()).map(|_| None).collect();
    let mut wiki: Result<Vec<GeneratedWiki>, String> = Err("未執行".to_string());
    for o in outcomes {
        match o {
            ContentOutcome::Member(i, r) => {
                if let Some(slot) = contents.get_mut(i) {
                    *slot = Some(r);
                }
            }
            ContentOutcome::Wiki(r) => wiki = r,
        }
    }

    let mut notes = Vec::new();
    let mut agents = Vec::with_capacity(plan.members.len());
    let mut skills: Vec<GeneratedSkill> = Vec::new();
    let mut succeeded = 0usize;
    for (member, content) in plan.members.iter().zip(contents) {
        match content.unwrap_or_else(|| Err("未執行".to_string())) {
            Ok(mc) => {
                succeeded += 1;
                skills.extend(schema::owned_skills(&member.name, &mc.skills));
                agents.push(agent_from(member, mc.soul_md, mc.contract));
            }
            Err(e) => {
                notes.push(format!(
                    "「{}」的角色說明與技能生成失敗（{}），已先用名單資料暫代，建議重新生成",
                    member.label(),
                    duduclaw_core::truncate_chars(&e, 160)
                ));
                agents.push(agent_from(
                    member,
                    placeholder_soul(plan, member),
                    GeneratedContract::default(),
                ));
            }
        }
    }
    if succeeded == 0 {
        return Err(format!(
            "所有成員的內容都生成失敗：{}",
            duduclaw_core::truncate_chars(&notes.join("；"), 600)
        ));
    }
    let wiki = wiki.unwrap_or_else(|e| {
        notes.push(format!(
            "知識庫生成失敗（{}），這版草稿沒有知識庫頁面，建議重新生成",
            duduclaw_core::truncate_chars(&e, 160)
        ));
        Vec::new()
    });

    Ok(StagedResult {
        pack: GeneratedPack {
            slug: plan.slug.clone(),
            display_name: plan.display_name.clone(),
            description: plan.description.clone(),
            prompts: plan.prompts.clone(),
            channels: plan.channels.clone(),
            agents,
            skills: schema::dedupe_skill_names(skills),
            wiki,
        },
        notes,
    })
}

/// Longest display name kept from the model (chars).
const MAX_DISPLAY_NAME_CHARS: usize = 24;
const MAX_SUMMARY_CHARS: usize = 200;

/// The routing trigger is derived here, never taken from the model: a
/// model-chosen trigger such as 「hi」 would capture ordinary channel
/// messages. `@` + the display name without whitespace, like the built-in
/// team converter.
fn derive_trigger(display_name: &str, name: &str) -> String {
    let base: String = display_name.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect();
    let base = if base.is_empty() { name.to_string() } else { base };
    format!("@{base}")
}

fn agent_from(member: &RosterMember, soul_md: String, contract: GeneratedContract) -> GeneratedAgent {
    let display_name = duduclaw_core::truncate_chars(member.display_name.trim(), MAX_DISPLAY_NAME_CHARS);
    GeneratedAgent {
        name: member.name.clone(),
        role: if member.is_front_desk() { "front_desk" } else { "worker" }.to_string(),
        trigger: derive_trigger(&display_name, &member.name),
        display_name,
        reports_to: member.reports_to.trim().to_string(),
        summary: duduclaw_core::truncate_chars(member.summary.trim(), MAX_SUMMARY_CHARS),
        department: member.department.trim().to_string(),
        soul_md,
        agent_partial_toml: String::new(),
        contract,
    }
}

/// Deterministic stand-in persona for a member whose content failed.
fn placeholder_soul(plan: &RosterPlan, member: &RosterMember) -> String {
    let mut s = format!(
        "# {}\n\n> 這份角色說明是暫代版本（AI 生成失敗），請重新生成或由管理者補寫。\n\n## 我是誰\n我是{}的{}。{}\n\n## 服務範圍\n",
        member.label(),
        plan.display_name.trim(),
        member.label(),
        member.summary.trim()
    );
    for r in member.responsibilities.iter().filter(|r| !r.trim().is_empty()) {
        s.push_str(&format!("- {}\n", r.trim()));
    }
    s.push_str("\n## 升級真人\n- 任何不確定、涉及金流、法律或醫療判斷、不可逆操作的事，一律轉交真人。\n");
    s
}
