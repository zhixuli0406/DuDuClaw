//! Orchestration tests with a canned model (no network, no CLI).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use super::job::{JobStatus, ProgressSink, StepStatus};
use super::report::PLACEHOLDER_MARKER;
use super::schema::COMPLIANCE_DISCLAIMER;
use super::*;
use crate::expert_generate::{self as eg, GenerateRequest};

// ─────────────────────────── fixtures ───────────────────────────

fn filler(n: usize) -> String {
    "這一段說明實際處理方式、需要確認的資料與交接時的注意事項。".repeat(n)
}

fn soul_md(label: &str) -> String {
    format!(
        "# {label}\n\n## 我是誰\n我是{label}。{f}\n\n## 服務範圍\n- 受理\n- 整理\n{f}\n\n## 工作流程\n1. 確認\n2. 整理\n3. 交付\n{f}\n\n## 好回答範例\n顧客：「請問？」\n我：「好的，我先幫您確認。」\n{f}\n\n## 紅線\n- 不承諾金額。原因：需要主管核定。\n\n## 升級真人\n- 涉及金流。\n{f}\n\n## 語氣\n口語、有耐心。\n",
        f = filler(6)
    )
}

fn skill_md() -> String {
    format!(
        "# 作業\n\n## 何時使用\n- 顧客提到相關需求。\n\n## 步驟\n1. 確認資料\n2. 整理\n{f}\n\n## 輸出範本\n```\n項目：\n```\n\n## 範例\n項目：範例\n{f}\n",
        f = filler(8)
    )
}

fn member_json(name: &str, label: &str) -> String {
    json!({
        "soul_md": soul_md(label),
        "skills": [{ "name": format!("{name}-work"), "description": "作業技能", "skill_md": skill_md() }],
        "contract": {
            "must_not": ["替客戶決定要不要退費或承諾退費金額", "退款"],
            "must_always": ["涉及金額一律轉交真人確認"]
        }
    })
    .to_string()
}

fn roster_json(size: usize) -> String {
    let mut members = vec![json!({
        "name": "shop-assistant", "role": "front_desk", "display_name": "總機",
        "reports_to": "", "trigger": "@總機", "summary": "對外窗口",
        "responsibilities": ["接待詢問", "分派給員工"]
    })];
    for i in 1..size {
        members.push(json!({
            "name": format!("shop-worker-{i}"), "role": "worker",
            "display_name": format!("員工{i}"), "reports_to": "shop-assistant",
            "summary": "處理業務", "department": "客服",
            "responsibilities": ["處理案件", "回報進度"]
        }));
    }
    json!({
        "slug": "shop-team", "display_name": "商店團隊", "description": "商店客服",
        "prompts": ["我要退貨"], "channels": ["line"], "members": members
    })
    .to_string()
}

fn wiki_json() -> String {
    let page = |kind: &str, title: &str| {
        json!({ "kind": kind, "title": title, "content": format!("## 小節\n{}", filler(20)) })
    };
    json!({ "pages": [
        page("sop", "流程 SOP"),
        page("glossary", "詞彙表"),
        page("compliance", "法規與合規注意事項"),
        page("faq", "常見問答"),
        page("other", "話術範本"),
    ]})
    .to_string()
}

fn request(size: usize) -> GenerateRequest {
    GenerateRequest {
        industry_hint: "網路商店".into(),
        description: "退換貨與客服".into(),
        team_size: size,
        channels: vec!["line".into()],
        research: false,
    }
}

/// Canned model: queued replies per stage (per member for stage B), with an
/// in-flight counter to observe the concurrency bound.
#[derive(Default)]
struct Canned {
    roster: Mutex<Vec<String>>,
    wiki: Mutex<Vec<String>>,
    members: Mutex<HashMap<String, Vec<String>>>,
    calls: Mutex<Vec<(Stage, String)>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl Canned {
    fn happy(size: usize) -> Self {
        let c = Canned::default();
        *c.roster.lock().unwrap() = vec![roster_json(size)];
        *c.wiki.lock().unwrap() = vec![wiki_json()];
        let mut m = HashMap::new();
        m.insert("shop-assistant".to_string(), vec![member_json("shop-assistant", "總機")]);
        for i in 1..size {
            let n = format!("shop-worker-{i}");
            m.insert(n.clone(), vec![member_json(&n, &format!("員工{i}"))]);
        }
        *c.members.lock().unwrap() = m;
        c
    }

    fn pop(queue: &mut Vec<String>) -> Result<String, String> {
        if queue.is_empty() {
            Err("canned: no more replies".into())
        } else {
            Ok(queue.remove(0))
        }
    }

    fn member_name(user: &str) -> String {
        user.split("<member_name>")
            .nth(1)
            .and_then(|r| r.split("</member_name>").next())
            .unwrap_or_default()
            .to_string()
    }
}

#[async_trait::async_trait]
impl StageLlm for Canned {
    async fn complete(&self, stage: Stage, _system: &str, user: &str) -> Result<String, String> {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.calls.lock().unwrap().push((stage, user.to_string()));
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        let out = match stage {
            Stage::Roster => Self::pop(&mut self.roster.lock().unwrap()),
            Stage::Wiki => Self::pop(&mut self.wiki.lock().unwrap()),
            Stage::Member => {
                let name = Self::member_name(user);
                let mut m = self.members.lock().unwrap();
                Self::pop(m.entry(name).or_default())
            }
        };
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        out
    }
}

async fn run(llm: &Canned, size: usize) -> (Result<StagedResult, String>, ProgressSink) {
    let req = request(size);
    let input = StagedRequest {
        req: &req,
        installed_example: None,
        prior: None,
        feedback: None,
    };
    let sink = ProgressSink::in_memory();
    let res = run_staged(llm, &input, &sink).await;
    (res, sink)
}

// ─────────────────────────── orchestration ───────────────────────────

#[tokio::test]
async fn happy_path_gives_every_member_skills_contract_and_four_wiki_pages() {
    let llm = Canned::happy(3);
    let (res, sink) = run(&llm, 3).await;
    let res = res.expect("staged run");
    assert!(res.notes.is_empty(), "{:?}", res.notes);
    let pack = &res.pack;
    assert_eq!(pack.agents.len(), 3);
    for a in &pack.agents {
        assert!(
            pack.skills.iter().any(|s| s.owner == a.name),
            "{} has no skill",
            a.name
        );
        assert_eq!(
            a.contract.must_not,
            vec!["替客戶決定要不要退費或承諾退費金額".to_string()],
            "keyword rule dropped, sentence kept"
        );
    }
    let files: Vec<&str> = pack.wiki.iter().map(|w| w.file.as_str()).collect();
    assert_eq!(files, vec!["sop.md", "glossary.md", "compliance.md", "faq.md", "page-1.md"]);
    let compliance = pack.wiki.iter().find(|w| w.kind == "compliance").unwrap();
    assert!(compliance.content.starts_with(COMPLIANCE_DISCLAIMER));

    // 1 roster + 3 members + 1 wiki, never more than 2 in flight.
    assert_eq!(llm.calls.lock().unwrap().len(), 5);
    let max = llm.max_in_flight.load(Ordering::SeqCst);
    assert!(max <= STAGE_CONCURRENCY, "max in flight {max}");
    assert_eq!(max, STAGE_CONCURRENCY, "stages B/C actually overlap");

    let rec = sink.snapshot();
    assert_eq!(rec.progress.roster, StepStatus::Done);
    assert_eq!(rec.progress.wiki, StepStatus::Done);
    assert!(rec.progress.members.iter().all(|m| m.status == StepStatus::Done));

    // The assembled pack materializes and validates.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("pack");
    eg::materialize_draft(&dir, pack).unwrap();
    assert!(eg::validate_draft_pack(&dir).is_empty(), "{:?}", eg::validate_draft_pack(&dir));
    for a in &pack.agents {
        assert!(dir.join("agents").join(&a.name).join("CONTRACT.toml").is_file());
    }
    assert_eq!(std::fs::read_dir(dir.join("wiki/shop-team")).unwrap().count(), 5);
    // Every page carries its title as an H1, even when the model's content
    // opens with `## ` sections (the preview reads titles from that H1).
    for w in &pack.wiki {
        let body = std::fs::read_to_string(dir.join("wiki/shop-team").join(&w.file)).unwrap();
        assert!(body.starts_with(&format!("# {}", w.title)), "{}: {body:.40}", w.file);
    }
}

#[tokio::test]
async fn invalid_roster_is_retried_once_with_the_problems_fed_back() {
    let llm = Canned::happy(3);
    llm.roster.lock().unwrap().insert(0, roster_json(2)); // wrong size first
    let (res, _) = run(&llm, 3).await;
    assert!(res.is_ok());
    let calls = llm.calls.lock().unwrap();
    let roster_calls: Vec<&String> = calls
        .iter()
        .filter(|(s, _)| *s == Stage::Roster)
        .map(|(_, u)| u)
        .collect();
    assert_eq!(roster_calls.len(), 2);
    assert!(roster_calls[1].contains("團隊人數必須是 3 位"), "errors fed back");
    assert!(roster_calls[0].starts_with("<stage>roster</stage>"));
}

#[tokio::test]
async fn a_failed_member_falls_back_without_discarding_the_others() {
    let llm = Canned::happy(3);
    llm.members
        .lock()
        .unwrap()
        .insert("shop-worker-2".into(), vec!["{}".into(), "not json".into()]);
    let (res, sink) = run(&llm, 3).await;
    let res = res.expect("partial result, not an error");
    assert_eq!(res.notes.len(), 1);
    assert!(res.notes[0].contains("員工2"), "{:?}", res.notes);
    let failed = res.pack.agents.iter().find(|a| a.name == "shop-worker-2").unwrap();
    assert!(failed.soul_md.contains(PLACEHOLDER_MARKER));
    assert!(!res.pack.skills.iter().any(|s| s.owner == "shop-worker-2"));
    assert!(res.pack.skills.iter().any(|s| s.owner == "shop-worker-1"));
    let rec = sink.snapshot();
    let row = rec.progress.members.iter().find(|m| m.name == "shop-worker-2").unwrap();
    assert_eq!(row.status, StepStatus::Failed);
    assert!(!row.error.is_empty());

    // The partial pack still materializes and validates.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("pack");
    eg::materialize_draft(&dir, &res.pack).unwrap();
    assert!(eg::validate_draft_pack(&dir).is_empty());
}

#[tokio::test]
async fn wiki_failure_is_partial_and_roster_or_total_failure_is_an_error() {
    let llm = Canned::happy(2);
    *llm.wiki.lock().unwrap() = vec![json!({"pages": []}).to_string()];
    let (res, _) = run(&llm, 2).await;
    let res = res.unwrap();
    assert!(res.pack.wiki.is_empty());
    assert!(res.notes.iter().any(|n| n.contains("知識庫")));

    let llm = Canned::happy(2);
    *llm.roster.lock().unwrap() = vec!["nope".into(), "still nope".into()];
    let (res, sink) = run(&llm, 2).await;
    assert!(res.unwrap_err().contains("名單"));
    assert_eq!(sink.snapshot().progress.roster, StepStatus::Failed);

    let llm = Canned::happy(2);
    llm.members.lock().unwrap().clear();
    let (res, _) = run(&llm, 2).await;
    assert!(res.unwrap_err().contains("所有成員"));
}

#[tokio::test]
async fn revise_replays_prior_content_and_feedback_per_stage() {
    let llm = Canned::happy(2);
    let (first, _) = run(&llm, 2).await;
    let prior = first.unwrap().pack;

    let llm = Canned::happy(2);
    let req = request(2);
    let input = StagedRequest {
        req: &req,
        installed_example: Some("[expert]\nname = \"cached-team\"".into()),
        prior: Some(&prior),
        feedback: Some("總機改名小花"),
    };
    let sink = ProgressSink::in_memory();
    run_staged(&llm, &input, &sink).await.unwrap();
    let calls = llm.calls.lock().unwrap();
    for (stage, user) in calls.iter() {
        assert!(user.contains("總機改名小花"), "{stage:?} lacks feedback");
        assert!(user.contains("<previous_draft>"), "{stage:?} lacks prior");
    }
    let roster = calls.iter().find(|(s, _)| *s == Stage::Roster).unwrap();
    assert!(roster.1.contains("cached-team"), "installed manifest used as extra example");
    let member = calls.iter().find(|(s, _)| *s == Stage::Member).unwrap();
    assert!(member.1.contains("<worked_example>"), "embedded example always present");
}

#[tokio::test]
async fn routing_triggers_are_derived_never_taken_from_the_model() {
    let llm = Canned::happy(2);
    let mut roster: serde_json::Value = serde_json::from_str(&roster_json(2)).unwrap();
    roster["members"][1]["trigger"] = json!("hi");
    roster["members"][1]["display_name"] = json!("售後 小安");
    *llm.roster.lock().unwrap() = vec![roster.to_string()];
    let (res, _) = run(&llm, 2).await;
    let pack = res.unwrap().pack;
    assert_eq!(pack.agents[0].trigger, "@總機");
    assert_eq!(pack.agents[1].trigger, "@售後小安");
}

#[test]
fn skill_descriptions_cannot_break_frontmatter() {
    let mut pack: eg::GeneratedPack = serde_json::from_value(json!({
        "slug": "t-team", "display_name": "t", "description": "d",
        "agents": [{ "name": "t-front", "role": "front_desk", "soul_md": "# t" }],
        "skills": [{ "name": "t-dispatch", "description": "分派: 規則\nname: evil\r", "skill_md": "# x" }]
    }))
    .unwrap();
    pack.skills[0].owner = String::new();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("pack");
    eg::materialize_draft(&dir, &pack).unwrap();
    let md = std::fs::read_to_string(dir.join("skills/t-dispatch/SKILL.md")).unwrap();
    let meta = duduclaw_agent::skill_loader::parse_skill_meta_from_content(&md, "t-dispatch");
    assert_eq!(meta.name, "t-dispatch");
    assert_eq!(meta.description, "分派: 規則 name: evil");
}

// ─────────────────────────── validators ───────────────────────────

#[test]
fn member_validation_reports_missing_depth() {
    let member: schema::RosterMember = serde_json::from_value(json!({
        "name": "a", "role": "worker", "responsibilities": ["x", "y"]
    }))
    .unwrap();
    let thin: schema::MemberContent = serde_json::from_value(json!({
        "soul_md": "# a\n\n## 我是誰\n短", "skills": [], "contract": {}
    }))
    .unwrap();
    let problems = schema::validate_member(&member, thin).unwrap_err();
    let joined = problems.join("\n");
    for needle in ["太短", "## 工作流程", "## 紅線", "專屬技能", "must_not", "must_always"] {
        assert!(joined.contains(needle), "missing {needle}: {joined}");
    }
}

#[test]
fn wiki_validation_requires_all_four_kinds() {
    let bundle: schema::WikiBundle = serde_json::from_value(json!({ "pages": [
        { "kind": "sop", "title": "a", "content": filler(20) },
        { "kind": "sop", "title": "b", "content": filler(20) },
        { "kind": "faq", "title": "c", "content": filler(20) },
        { "kind": "other", "title": "d", "content": "短" },
    ]}))
    .unwrap();
    let problems = schema::validate_wiki(&bundle).unwrap_err().join("\n");
    assert!(problems.contains("glossary") && problems.contains("compliance"), "{problems}");
    assert!(problems.contains("太短"));
}

#[test]
fn duplicate_skill_names_get_deterministic_suffixes() {
    let mk = |name: &str, owner: &str| eg::GeneratedSkill {
        name: name.into(),
        description: String::new(),
        skill_md: "x".into(),
        owner: owner.into(),
    };
    let out = schema::dedupe_skill_names(vec![mk("work", "a"), mk("work", "b"), mk("work", "c")]);
    let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["work", "work-2", "work-3"]);
}

// ─────────────────────────── preview / install report ───────────────────────────

#[tokio::test]
async fn preview_and_precheck_surface_skills_red_lines_and_conflicts() {
    let llm = Canned::happy(2);
    let (res, _) = run(&llm, 2).await;
    let pack = res.unwrap().pack;
    let home = tempfile::tempdir().unwrap();
    let state = eg::DraftState {
        draft_id: "abc-123".into(),
        request: request(2),
        rounds: 1,
        created_at: crate::expert_admin::now_iso(),
        updated_at: crate::expert_admin::now_iso(),
        last_generation: serde_json::to_string(&pack).unwrap(),
        notes: vec!["n".into()],
    };
    let dir = eg::draft_pack_dir(home.path(), "abc-123").unwrap();
    eg::materialize_draft(&dir, &pack).unwrap();
    // A same-name page already in the shared wiki is never overwritten.
    let taken = home.path().join("shared/wiki/shop-team/faq.md");
    std::fs::create_dir_all(taken.parent().unwrap()).unwrap();
    std::fs::write(&taken, "# 舊頁面\n").unwrap();

    let preview = eg::draft_preview_json(home.path(), &state);
    let agents = preview["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 2);
    for a in agents {
        assert!(!a["skills"].as_array().unwrap().is_empty());
        assert!(!a["red_lines"].as_array().unwrap().is_empty());
        assert_eq!(a["placeholder"], false);
    }
    assert_eq!(preview["wiki_pages"].as_array().unwrap().len(), 5);
    assert_eq!(preview["notes"], json!(["n"]));
    let warnings = preview["install_warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w["kind"] == "wiki" && w["reason"] == "conflict" && w["name"] == "shop-team/faq.md"),
        "{warnings:?}"
    );

    // Post-install report: pretend the CLI installed the agents and every
    // page except the conflicting one.
    let pre = report::existing_wiki_targets(home.path(), &dir);
    for a in &pack.agents {
        let agent = home.path().join("agents").join(&a.name);
        for s in pack.skills.iter().filter(|s| s.owner == a.name) {
            let p = agent.join("SKILLS").join(&s.name);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("SKILL.md"), "x").unwrap();
        }
        std::fs::write(agent.join("CONTRACT.toml"), "[boundaries]\n").unwrap();
    }
    let agents: Vec<String> = pack.agents.iter().map(|a| a.name.clone()).collect();
    let wiki: Vec<String> = pack
        .wiki
        .iter()
        .filter(|w| w.file != "faq.md")
        .map(|w| format!("shop-team/{}", w.file))
        .collect();
    let placement = report::placement_report(home.path(), &dir, &agents, &wiki, &pre);
    assert_eq!(placement["complete"], false);
    let faq = placement["wiki"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["file"] == "shop-team/faq.md")
        .unwrap();
    assert_eq!(faq["status"], "conflict");
    for a in placement["agents"].as_array().unwrap() {
        assert_eq!(a["installed"], true);
        assert_eq!(a["contract"], true);
    }
}

#[test]
fn job_status_terminality() {
    assert!(!JobStatus::Running.is_terminal());
    for s in [JobStatus::Done, JobStatus::Partial, JobStatus::Failed, JobStatus::Cancelled] {
        assert!(s.is_terminal());
    }
}
