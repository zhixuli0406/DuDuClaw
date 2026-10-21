//! Per-stage JSON shapes the model emits, plus their parsers and validators.
//!
//! Every stage reply is treated as EXTERNAL content: it is parsed into a
//! narrow struct (unknown keys ignored), then validated. A validation problem
//! list is fed back to the model on the single retry; nothing here writes a
//! file.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::expert_generate::{
    GeneratedContract, GeneratedSkill, GeneratedWiki, KNOWN_CHANNELS, extract_json_object,
};
use crate::premium_templates::is_safe_slug;

use super::contract::sanitize_contract_for;

/// Minimum SOUL length (chars). The bundled industry packs run 1.5–4 k chars
/// per member; below this a SOUL cannot carry the required sections with any
/// substance.
pub const MIN_SOUL_CHARS: usize = 900;
/// Minimum SKILL.md body length (chars).
pub const MIN_SKILL_CHARS: usize = 500;
/// Minimum wiki page length (chars).
pub const MIN_WIKI_PAGE_CHARS: usize = 500;
/// Upper bound on wiki pages one team may ship (keeps install + review sane).
pub const MAX_WIKI_PAGES: usize = 10;
/// Longest member id: leaves room under the 64-char agent-id limit for the
/// installer's team-wide `-N` / `-imported` rename suffix.
pub const MAX_MEMBER_ID_LEN: usize = 54;
/// Upper bound on skills per member.
pub const MAX_SKILLS_PER_MEMBER: usize = 3;

/// SOUL sections every member must carry, each as `(label, keywords)`: a
/// `## ` heading containing any keyword satisfies the section.
pub const SOUL_SECTIONS: [(&str, &[&str]); 7] = [
    ("## 我是誰", &["我是誰", "身分", "角色"]),
    ("## 服務範圍", &["服務範圍", "範圍", "職責"]),
    ("## 工作流程", &["工作流程", "流程", "步驟"]),
    ("## 好回答範例", &["範例", "示範"]),
    ("## 紅線", &["紅線", "絕不", "禁止"]),
    ("## 升級真人", &["升級", "轉交", "交給真人"]),
    ("## 語氣", &["語氣", "口吻", "說話方式"]),
];

/// SKILL.md sections, same matching rule as [`SOUL_SECTIONS`].
pub const SKILL_SECTIONS: [(&str, &[&str]); 4] = [
    ("## 何時使用", &["何時使用", "觸發", "使用時機"]),
    ("## 步驟", &["步驟", "流程", "程序"]),
    ("## 輸出範本", &["輸出範本", "範本", "模板"]),
    ("## 範例", &["範例", "示範", "實例"]),
];

/// Wiki page kinds the team knowledge base must cover, with their fixed
/// on-disk file names (file names are never taken from the model).
pub const REQUIRED_WIKI_KINDS: [(&str, &str); 4] = [
    ("sop", "sop.md"),
    ("glossary", "glossary.md"),
    ("compliance", "compliance.md"),
    ("faq", "faq.md"),
];

/// Disclaimer prepended to every compliance page, deterministically (never
/// left to the model to remember).
pub const COMPLIANCE_DISCLAIMER: &str = "> 本頁由 AI 產生，只整理需要注意的方向。法規名稱、條號與數字都必須由專人向主管機關或專業人士核對後，才可以對外引用。AI 員工遇到具體法律判斷時一律轉交真人。";

// ─────────────────────────── Stage A: roster ───────────────────────────

/// Stage A reply: the team skeleton (no prose content yet).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RosterPlan {
    pub slug: String,
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub prompts: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub members: Vec<RosterMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RosterMember {
    pub name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub reports_to: String,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub department: String,
    #[serde(default)]
    pub responsibilities: Vec<String>,
}

impl RosterMember {
    pub fn is_front_desk(&self) -> bool {
        self.role.trim() == "front_desk"
    }
    /// Human label for progress rows and prompts.
    pub fn label(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.name
        } else {
            self.display_name.trim()
        }
    }
}

pub fn parse_roster(raw: &str) -> Result<RosterPlan, String> {
    let obj = extract_json_object(raw)?;
    serde_json::from_str::<RosterPlan>(&obj).map_err(|e| format!("名單 JSON 結構不符: {e}"))
}

/// Validate a roster against the requested team size. Returns ALL problems.
pub fn validate_roster(plan: &RosterPlan, team_size: usize) -> Vec<String> {
    let mut problems = Vec::new();
    if !is_safe_slug(&plan.slug) {
        problems.push(format!("slug '{}' 非法（小寫英數與連字號）", plan.slug.escape_debug()));
    }
    if plan.display_name.trim().is_empty() {
        problems.push("display_name 不可空白".to_string());
    }
    if plan.description.trim().is_empty() {
        problems.push("description 不可空白".to_string());
    }
    for ch in &plan.channels {
        if !KNOWN_CHANNELS.contains(&ch.as_str()) {
            problems.push(format!("channels 含不支援的通路: {}", ch.escape_debug()));
        }
    }
    if plan.members.len() != team_size {
        problems.push(format!(
            "團隊人數必須是 {team_size} 位（目前 {} 位）",
            plan.members.len()
        ));
    }
    problems.extend(validate_members(&plan.members));
    problems
}

/// The prefix every member id of a team must carry: the team slug without a
/// trailing `-team` (`flowershop-team` → `flowershop`).
pub fn member_prefix(slug: &str) -> &str {
    slug.strip_suffix("-team").unwrap_or(slug)
}

/// Prefix member names (and the `reports_to` that point at them) with the
/// team stem, so ids from two generated teams cannot collide. Names that
/// already carry the prefix are kept. An unsafe slug is left untouched (the
/// validator rejects it).
pub fn prefix_member_names(plan: RosterPlan) -> RosterPlan {
    if !is_safe_slug(&plan.slug) {
        return plan;
    }
    let stem = member_prefix(&plan.slug).to_string();
    let fix = |name: &str| -> String {
        let n = name.trim();
        if n.is_empty() || n == stem || n.starts_with(&format!("{stem}-")) {
            n.to_string()
        } else {
            format!("{stem}-{n}")
        }
    };
    let members = plan
        .members
        .iter()
        .map(|m| RosterMember {
            name: fix(&m.name),
            reports_to: if m.reports_to.trim().is_empty() {
                String::new()
            } else {
                fix(&m.reports_to)
            },
            ..m.clone()
        })
        .collect();
    RosterPlan { members, ..plan }
}

fn validate_members(members: &[RosterMember]) -> Vec<String> {
    let mut problems = Vec::new();
    let Some(first) = members.first() else {
        problems.push("members 為空".to_string());
        return problems;
    };
    if !first.is_front_desk() || !first.reports_to.trim().is_empty() {
        problems.push("第一位必須是 role=front_desk 且 reports_to 為空字串".to_string());
    }
    if members.iter().filter(|m| m.is_front_desk()).count() != 1 {
        problems.push("恰好一位 front_desk".to_string());
    }
    let names: BTreeSet<&str> = members.iter().map(|m| m.name.as_str()).collect();
    if names.len() != members.len() {
        problems.push("成員 name 有重複".to_string());
    }
    // The routing trigger is derived from display_name, so it must be unique.
    let labels: BTreeSet<&str> = members.iter().map(|m| m.label()).collect();
    if labels.len() != members.len() {
        problems.push("成員 display_name 有重複".to_string());
    }
    for m in members {
        if !is_safe_slug(&m.name) {
            problems.push(format!("成員 name '{}' 非法", m.name.escape_debug()));
        } else if m.name.len() > MAX_MEMBER_ID_LEN {
            problems.push(format!("成員 name '{}' 太長（最多 {MAX_MEMBER_ID_LEN} 字元）", m.name));
        }
        let rt = m.reports_to.trim();
        if !m.is_front_desk() && rt != first.name {
            problems.push(format!("worker '{}' 的 reports_to 必須是 '{}'", m.name, first.name));
        }
        if !m.department.trim().is_empty() && !duduclaw_core::is_valid_department(m.department.trim())
        {
            problems.push(format!("'{}' 的 department 非合法部門名", m.name));
        }
        if m.responsibilities.iter().filter(|r| !r.trim().is_empty()).count() < 2 {
            problems.push(format!("'{}' 至少要列出 2 項具體職責", m.name));
        }
    }
    problems
}

// ─────────────────────────── Stage B: member content ───────────────────────────

/// Stage B reply for ONE member: persona + its skills + its contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberContent {
    pub soul_md: String,
    #[serde(default)]
    pub skills: Vec<MemberSkill>,
    #[serde(default)]
    pub contract: GeneratedContract,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberSkill {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub skill_md: String,
}

pub fn parse_member(raw: &str) -> Result<MemberContent, String> {
    let obj = extract_json_object(raw)?;
    serde_json::from_str::<MemberContent>(&obj).map_err(|e| format!("員工內容 JSON 結構不符: {e}"))
}

/// Does `md` carry a `## ` (or deeper) heading matching any keyword?
fn has_section(md: &str, keywords: &[&str]) -> bool {
    md.lines().any(|line| {
        let t = line.trim_start();
        t.starts_with("##") && keywords.iter().any(|k| t.contains(k))
    })
}

fn missing_sections(md: &str, sections: &[(&'static str, &[&str])]) -> Vec<&'static str> {
    sections
        .iter()
        .filter(|(_, kws)| !has_section(md, kws))
        .map(|(label, _)| *label)
        .collect()
}

/// Validate one member's content and return it NORMALIZED (contract
/// sanitized). `Err` carries all problems for the retry prompt.
pub fn validate_member(member: &RosterMember, mc: MemberContent) -> Result<MemberContent, Vec<String>> {
    let mut problems = Vec::new();
    let soul = mc.soul_md.trim();
    if soul.chars().count() < MIN_SOUL_CHARS {
        problems.push(format!(
            "soul_md 太短（{} 字，至少 {MIN_SOUL_CHARS} 字）",
            soul.chars().count()
        ));
    }
    for label in missing_sections(soul, &SOUL_SECTIONS) {
        problems.push(format!("soul_md 缺少章節「{label}」"));
    }
    if mc.skills.is_empty() {
        problems.push(if member.is_front_desk() {
            "總機必須附一個分派技能（skills 不可為空）".to_string()
        } else {
            "每位員工至少要有 1 個專屬技能（skills 不可為空）".to_string()
        });
    }
    if mc.skills.len() > MAX_SKILLS_PER_MEMBER {
        problems.push(format!("skills 最多 {MAX_SKILLS_PER_MEMBER} 個"));
    }
    for s in &mc.skills {
        problems.extend(validate_skill(s));
    }
    // Rules that repeat the member's own handoff / example wording would
    // block its own replies; they are dropped like keyword rules.
    let contract = sanitize_contract_for(&mc.contract, Some(soul));
    if contract.must_not.is_empty() {
        problems.push("contract.must_not 至少 1 條（每條 10 字以上的完整描述）".to_string());
    }
    if contract.must_always.is_empty() {
        problems.push("contract.must_always 至少 1 條".to_string());
    }
    if problems.is_empty() {
        Ok(MemberContent { contract, ..mc })
    } else {
        Err(problems)
    }
}

fn validate_skill(s: &MemberSkill) -> Vec<String> {
    let mut problems = Vec::new();
    if !is_safe_slug(&s.name) || !duduclaw_agent::skill_loader::is_safe_skill_name(&s.name) {
        problems.push(format!("skill name '{}' 非法（小寫英數與連字號）", s.name.escape_debug()));
    }
    let body = s.skill_md.trim();
    if body.chars().count() < MIN_SKILL_CHARS {
        problems.push(format!(
            "技能 '{}' 的 skill_md 太短（至少 {MIN_SKILL_CHARS} 字）",
            s.name
        ));
    }
    for label in missing_sections(body, &SKILL_SECTIONS) {
        problems.push(format!("技能 '{}' 缺少章節「{label}」", s.name));
    }
    problems
}

/// Convert a member's skills into pack skills owned by that member.
pub fn owned_skills(owner: &str, skills: &[MemberSkill]) -> Vec<GeneratedSkill> {
    skills
        .iter()
        .map(|s| GeneratedSkill {
            name: s.name.trim().to_string(),
            description: s.description.trim().to_string(),
            skill_md: s.skill_md.clone(),
            owner: owner.to_string(),
        })
        .collect()
}

/// Make skill names unique across the team: a later duplicate gets a
/// deterministic `-2`, `-3`… suffix (frontmatter is rebuilt at
/// materialization, so the rename is consistent on disk).
pub fn dedupe_skill_names(skills: Vec<GeneratedSkill>) -> Vec<GeneratedSkill> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    skills
        .into_iter()
        .map(|s| {
            let mut name = s.name.clone();
            let mut n = 2;
            while seen.contains(&name) {
                name = format!("{}-{n}", s.name);
                n += 1;
            }
            seen.insert(name.clone());
            GeneratedSkill { name, ..s }
        })
        .collect()
}

// ─────────────────────────── Stage C: wiki ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiBundle {
    #[serde(default)]
    pub pages: Vec<WikiPageDraft>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiPageDraft {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    pub content: String,
}

pub fn parse_wiki(raw: &str) -> Result<WikiBundle, String> {
    let obj = extract_json_object(raw)?;
    serde_json::from_str::<WikiBundle>(&obj).map_err(|e| format!("知識庫 JSON 結構不符: {e}"))
}

/// Validate a wiki bundle and convert it into pack wiki pages with FIXED
/// file names (`sop.md`, `glossary.md`, `compliance.md`, `faq.md`, then
/// `page-<n>.md` for extras). The compliance page always gets
/// [`COMPLIANCE_DISCLAIMER`] prepended.
pub fn validate_wiki(bundle: &WikiBundle) -> Result<Vec<GeneratedWiki>, Vec<String>> {
    let mut problems = Vec::new();
    if bundle.pages.len() < REQUIRED_WIKI_KINDS.len() {
        problems.push(format!(
            "知識庫至少 {} 頁（目前 {} 頁）",
            REQUIRED_WIKI_KINDS.len(),
            bundle.pages.len()
        ));
    }
    if bundle.pages.len() > MAX_WIKI_PAGES {
        problems.push(format!("知識庫最多 {MAX_WIKI_PAGES} 頁"));
    }
    for (kind, _) in REQUIRED_WIKI_KINDS {
        if !bundle.pages.iter().any(|p| p.kind.trim() == kind) {
            problems.push(format!("缺少 kind = \"{kind}\" 的頁面"));
        }
    }
    for p in &bundle.pages {
        if p.title.trim().is_empty() {
            problems.push(format!("kind '{}' 的頁面缺少 title", p.kind.escape_debug()));
        }
        if p.content.trim().chars().count() < MIN_WIKI_PAGE_CHARS {
            problems.push(format!(
                "「{}」內容太短（至少 {MIN_WIKI_PAGE_CHARS} 字）",
                p.title.trim()
            ));
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(assign_wiki_files(&bundle.pages))
}

fn assign_wiki_files(pages: &[WikiPageDraft]) -> Vec<GeneratedWiki> {
    let mut used: BTreeSet<&str> = BTreeSet::new();
    let mut extra = 0usize;
    pages
        .iter()
        .map(|p| {
            let kind = p.kind.trim();
            let fixed = REQUIRED_WIKI_KINDS
                .iter()
                .find(|(k, _)| *k == kind)
                .map(|(_, f)| *f)
                .filter(|f| !used.contains(f));
            let file = match fixed {
                Some(f) => {
                    used.insert(f);
                    f.to_string()
                }
                None => {
                    extra += 1;
                    format!("page-{extra}.md")
                }
            };
            let content = if kind == "compliance" {
                format!("{COMPLIANCE_DISCLAIMER}\n\n{}", p.content.trim())
            } else {
                p.content.trim().to_string()
            };
            GeneratedWiki {
                file,
                title: p.title.trim().to_string(),
                content,
                kind: kind.to_string(),
                web_research: false,
            }
        })
        .collect()
}
