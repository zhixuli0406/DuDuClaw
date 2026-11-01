//! The persona slot writer shared by `duduclaw pack install` (agent-config
//! formats) and `duduclaw migrate from openclaw|hermes`. Runs after the
//! employee directory exists (SOUL.md is written by the scaffold) and fills
//! every other slot:
//!
//! | draft field        | lands in                                              |
//! |--------------------|-------------------------------------------------------|
//! | `identity`         | `IDENTITY.md` (+ `agent.toml` display name / icon)    |
//! | `instructions`     | `INSTRUCTIONS.md` (prompt slot, employee read-only)   |
//! | `user_profile`     | `USER.md` (data-only prompt slot, 4,000 characters)         |
//! | `tools_notes`      | wiki page「工具備註」(notes only, grants nothing)      |
//! | `heartbeat`        | wiki page「定期檢查清單」(not scheduled automatically) |
//! | `knowledge`        | wiki pages (searched on demand)                       |
//! | `memory`           | memory facts, `import` origin + source lineage        |
//!
//! Every text is scanned first; a blocked file is not written (and is
//! reported). Nothing here can widen what the employee may do.

use std::path::Path;

use duduclaw_core::types::{MemoryEntry, MemoryLayer};

use super::ir::{AgentDraft, Doc};
use super::md;
use super::outcome::Outcome;
use crate::expert::{ScanVerdict, scan_external};

/// Where memory lineage points: `<prefix><doc.source>` is the source id of
/// each imported memory file, so `duduclaw memory forget-source --session
/// "import:<that id>"` revokes everything one file contributed. The pack
/// importer uses `agent-import:<slug>/` (no host path ever reaches a report
/// or the dashboard); `migrate from` uses the absolute source folder.
pub struct SlotCtx<'a> {
    pub home: &'a Path,
    pub agent_id: &'a str,
    pub dry_run: bool,
    pub lineage_prefix: &'a str,
    /// Short source code for wiki paths and tags (`doubao`, `openclaw`, …).
    pub format_code: &'a str,
    /// Patch `agent.toml` display name / icon from IDENTITY.md.
    pub apply_identity_fields: bool,
}

pub async fn write_slots(ctx: &SlotCtx<'_>, draft: &AgentDraft) -> Vec<Outcome> {
    let mut out = Vec::new();
    if !crate::is_valid_agent_id(ctx.agent_id) {
        out.push(Outcome::skipped("agent", ctx.agent_id, "無效的員工代號"));
        return out;
    }
    if let Some(doc) = &draft.user_profile {
        if doc.text.chars().count() > 4_000 {
            out.push(Outcome::skipped("user-profile", &doc.source, "服務對象資料超過 4,000 字，未寫入"));
        } else {
            write_file_slot(ctx, doc, "USER.md", "user-profile", "服務對象資料（USER.md）", &mut out);
        }
    }
    if let Some(doc) = &draft.contract {
        write_file_slot(ctx, doc, "CONTRACT.toml", "contract", "工作契約（CONTRACT.toml）", &mut out);
    }
    if let Some(doc) = &draft.identity {
        write_file_slot(
            ctx,
            doc,
            "IDENTITY.md",
            "identity",
            "身分設定（IDENTITY.md）",
            &mut out,
        );
        if ctx.apply_identity_fields && !ctx.dry_run {
            patch_identity_fields(ctx, draft, &mut out);
        }
    }
    if let Some(doc) = &draft.instructions {
        let len = doc.text.chars().count();
        write_file_slot(
            ctx,
            doc,
            duduclaw_core::INSTRUCTIONS_FILE,
            "instructions",
            "工作規則（INSTRUCTIONS.md，AI 員工只能讀）",
            &mut out,
        );
        if len > 20_000 {
            out.push(
                Outcome::warning(
                    "instructions",
                    &doc.source,
                    format!("工作規則有 {len} 字，較長；精簡模式只會放入前段"),
                )
                .with("slot.instructions_long", serde_json::json!({ "chars": len })),
            );
        }
    }
    let pages = [
        WikiSlot {
            doc: &draft.tools_notes,
            kind: "tools-notes",
            title: "工具備註",
            layer: duduclaw_memory::WikiLayer::Context,
            banner: "> 這頁只是工具使用說明，不會開放任何工具權限。\n\n",
        },
        WikiSlot {
            doc: &draft.heartbeat,
            kind: "heartbeat",
            title: "定期檢查清單",
            layer: duduclaw_memory::WikiLayer::Context,
            banner: "> 原平台的定期檢查清單。DuDuClaw 不會自動照表執行；需要時請在儀表板建立排程。\n\n",
        },
    ];
    for slot in pages {
        if let Some(doc) = slot.doc {
            let path = format!("imported/{}/{}.md", ctx.format_code, slot.kind);
            write_wiki(
                ctx,
                doc,
                &path,
                slot.title,
                slot.layer,
                slot.banner,
                slot.kind,
                &mut out,
            );
        }
    }
    for (i, doc) in draft.knowledge.iter().enumerate() {
        let path = format!(
            "imported/{}/knowledge/{}.md",
            ctx.format_code,
            knowledge_slug(&doc.source, i)
        );
        let title = doc
            .text
            .lines()
            .find(|l| l.starts_with("# "))
            .map(|l| l.trim_start_matches("# ").trim().to_string())
            .unwrap_or_else(|| doc.source.clone());
        write_wiki(
            ctx,
            doc,
            &path,
            &title,
            duduclaw_memory::WikiLayer::Deep,
            "",
            "knowledge",
            &mut out,
        );
    }
    if !draft.memory.is_empty() {
        write_memory(ctx, &draft.memory, &mut out).await;
    }
    out
}

/// Exact imported page payloads for confirmation. Import timestamps belong to
/// the wiki audit log; source documents do not assert a creation/update date.
pub(super) fn plan_wiki_files(draft: &AgentDraft, format: &str) -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
    use duduclaw_memory::WikiLayer;
    let mut pages = Vec::new();
    for (doc, kind, title, banner) in [
        (&draft.tools_notes, "tools-notes", "工具備註", "> 這頁只是工具使用說明，不會開放任何工具權限。\n\n"),
        (&draft.heartbeat, "heartbeat", "定期檢查清單", "> 原平台的定期檢查清單。DuDuClaw 不會自動照表執行；需要時請在儀表板建立排程。\n\n"),
    ] {
        if let Some(doc) = doc {
            pages.push((doc, format!("imported/{format}/{kind}.md"), title.to_string(), WikiLayer::Context, banner));
        }
    }
    for (i, doc) in draft.knowledge.iter().enumerate() {
        let path = format!("imported/{format}/knowledge/{}.md", knowledge_slug(&doc.source, i));
        let title = doc.text.lines().find(|line| line.starts_with("# "))
            .map(|line| line.trim_start_matches("# ").trim().to_string()).unwrap_or_else(|| doc.source.clone());
        pages.push((doc, path, title, WikiLayer::Deep, ""));
    }
    let mut files = std::collections::BTreeMap::new();
    for (doc, path, title, layer, banner) in pages {
        let scan = duduclaw_security::input_guard::scan_input(&doc.text, duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD);
        if scan.blocked { return Err("匯入知識頁未通過安全掃描".into()); }
        let page = duduclaw_memory::WikiPage {
            path: path.clone(), title, created: chrono::DateTime::UNIX_EPOCH, updated: chrono::DateTime::UNIX_EPOCH,
            tags: vec![format!("imported-from-{format}")], related: Vec::new(), sources: vec![doc.source.clone()],
            author: Some("import".into()), layer, trust: 0.5, source_type: duduclaw_memory::SourceType::Unknown,
            last_verified: None, citation_count: 0, error_signal_count: 0, success_signal_count: 0,
            do_not_inject: false, body: format!("{banner}{}", doc.text.trim_end()),
        };
        let serialized = duduclaw_memory::serialize_page(&page);
        let (header, body) = serialized.split_once("\n---\n")
            .ok_or_else(|| "無法產生知識頁".to_string())?;
        let header = header.lines().filter(|line| !line.starts_with("created:") && !line.starts_with("updated:"))
            .collect::<Vec<_>>().join("\n");
        let content = format!("{header}\n---\n{body}");
        if files.insert(path, content.into_bytes()).is_some() { return Err("匯入知識頁路徑重複".into()); }
    }
    Ok(files)
}

/// One persona file that becomes a wiki page.
struct WikiSlot<'a> {
    doc: &'a Option<Doc>,
    /// Report kind and page file name.
    kind: &'static str,
    title: &'static str,
    layer: duduclaw_memory::WikiLayer,
    banner: &'static str,
}

fn write_file_slot(
    ctx: &SlotCtx<'_>,
    doc: &Doc,
    file: &str,
    kind: &'static str,
    label: &str,
    out: &mut Vec<Outcome>,
) {
    if let ScanVerdict::Blocked(reason) = scan_external(&doc.text) {
        out.push(
            Outcome::skipped(kind, &doc.source, format!("內容安全檢查未通過，未寫入：{reason}"))
                .with("slot.blocked", serde_json::json!({ "slot": kind })),
        );
        return;
    }
    let done = |o: Outcome| o.with("slot.written", serde_json::json!({ "slot": kind }));
    if ctx.dry_run {
        out.push(done(Outcome::imported(kind, &doc.source, format!("→ {label}"))));
        return;
    }
    use duduclaw_gateway::agent_files::external::{self, ExternalFile, ExternalBaseline};
    let key = match file {
        "IDENTITY.md" => ExternalFile::Identity,
        "USER.md" => ExternalFile::User,
        "INSTRUCTIONS.md" => ExternalFile::Instructions,
        "CONTRACT.toml" => ExternalFile::Contract,
        _ => {
            out.push(Outcome::skipped(kind, &doc.source, "不支援的設定檔區段"));
            return;
        }
    };
    match external::write_file(ctx.home, ctx.agent_id, key, "import", "本機匯入（身分未驗證）",
        ExternalBaseline::Accept, |_| Ok(doc.text.clone())) {
        Ok(_) => out.push(done(Outcome::imported(kind, &doc.source, format!("→ {label}")))),
        Err(e) => out.push(
            Outcome::skipped(kind, &doc.source, format!("寫入失敗：{e}"))
                .with("slot.failed", serde_json::json!({ "slot": kind })),
        ),
    }
}

fn patch_identity_fields(ctx: &SlotCtx<'_>, draft: &AgentDraft, out: &mut Vec<Outcome>) {
    let ident = draft
        .identity
        .as_ref()
        .map(|d| md::parse_identity(&d.text))
        .unwrap_or_default();
    let display = ident
        .name
        .map(|n| clean_display_name(&n))
        .filter(|n| !n.is_empty());
    let icon = ident.emoji.or_else(|| draft.emoji.clone());
    if display.is_none() && icon.is_none() {
        return;
    }
    let res = crate::expert::patch_agent_toml(ctx.home, ctx.agent_id, |t| {
        if let Some(a) = t.get_mut("agent").and_then(|v| v.as_table_mut()) {
            if let Some(d) = &display {
                a.insert("display_name".into(), toml::Value::String(d.clone()));
            }
            if let Some(i) = &icon {
                a.insert("icon".into(), toml::Value::String(i.clone()));
            }
        }
    });
    if let Err(e) = res {
        out.push(
            Outcome::warning("identity", ctx.agent_id, format!("名稱／圖示未套用：{e}"))
                .with("slot.failed", serde_json::json!({ "slot": "identity" })),
        );
    }
}

/// A display name safe to put in `agent.toml` and a trigger: no quotes,
/// backslashes or control characters, ≤ 40 chars.
pub fn clean_display_name(raw: &str) -> String {
    let s: String = raw
        .chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\')
        .collect();
    duduclaw_core::truncate_chars(s.trim(), 40)
        .trim()
        .to_string()
}

fn knowledge_slug(source: &str, i: usize) -> String {
    let stem = source
        .rsplit('/')
        .next()
        .unwrap_or(source)
        .trim_end_matches(".md");
    let slug = super::skills::slugify(stem);
    if slug.is_empty() {
        format!("page-{}", i + 1)
    } else {
        slug
    }
}

#[allow(clippy::too_many_arguments)]
fn write_wiki(
    ctx: &SlotCtx<'_>,
    doc: &Doc,
    rel_path: &str,
    title: &str,
    layer: duduclaw_memory::WikiLayer,
    banner: &str,
    kind: &'static str,
    out: &mut Vec<Outcome>,
) {
    let scan = duduclaw_security::input_guard::scan_input(
        &doc.text,
        duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD,
    );
    if scan.blocked {
        out.push(
            Outcome::skipped(
                kind,
                &doc.source,
                format!("偵測到提示注入風險（規則 {}），未寫入", scan.matched_rules.join("/")),
            )
            .with("slot.blocked", serde_json::json!({ "slot": kind })),
        );
        return;
    }
    let where_ = format!("→ 知識頁「{title}」");
    let done = |o: Outcome| o.with("slot.wiki", serde_json::json!({ "slot": kind, "title": title }));
    if ctx.dry_run {
        out.push(done(Outcome::imported(kind, &doc.source, where_)));
        return;
    }
    let store =
        duduclaw_memory::WikiStore::new(ctx.home.join("agents").join(ctx.agent_id).join("wiki"));
    if let Err(e) = store.ensure_scaffold() {
        out.push(
            Outcome::skipped(kind, &doc.source, format!("建立知識庫失敗：{e}"))
                .with("slot.failed", serde_json::json!({ "slot": kind })),
        );
        return;
    }
    let now = chrono::Utc::now();
    let page = duduclaw_memory::WikiPage {
        path: rel_path.to_string(),
        title: title.to_string(),
        created: now,
        updated: now,
        tags: vec![format!("imported-from-{}", ctx.format_code)],
        related: Vec::new(),
        sources: vec![doc.source.clone()],
        author: Some("import".to_string()),
        layer,
        // An operator chose to import it; still below anything verified.
        trust: 0.5,
        source_type: duduclaw_memory::SourceType::Unknown,
        last_verified: None,
        citation_count: 0,
        error_signal_count: 0,
        success_signal_count: 0,
        do_not_inject: false,
        body: format!("{banner}{}", doc.text.trim_end()),
    };
    let content = duduclaw_memory::serialize_page(&page);
    match store.write_page_with_author(rel_path, &content, "import") {
        Ok(()) => out.push(done(Outcome::imported(kind, &doc.source, where_))),
        Err(e) => out.push(
            Outcome::skipped(kind, &doc.source, format!("寫入失敗：{e}"))
                .with("slot.failed", serde_json::json!({ "slot": kind })),
        ),
    }
}

async fn write_memory(ctx: &SlotCtx<'_>, docs: &[Doc], out: &mut Vec<Outcome>) {
    let engine = if ctx.dry_run {
        None
    } else {
        // Same construction as every gateway write path, so the
        // supersession trust guard and the novelty gate apply to imports too.
        match duduclaw_gateway::memory_factory::build_memory_engine(&ctx.home.join("memory.db"), ctx.home) {
            Ok(e) => Some(e),
            Err(e) => {
                out.push(
                    Outcome::skipped("memory", ctx.agent_id, format!("開啟記憶資料庫失敗：{e}"))
                        .with("slot.failed", serde_json::json!({ "slot": "memory" })),
                );
                return;
            }
        }
    };
    for doc in docs {
        let facts = md::extract_facts(&doc.text);
        if facts.is_empty() {
            out.push(
                Outcome::skipped("memory", &doc.source, "沒有可匯入的條目")
                    .with("slot.memory_empty", serde_json::json!({})),
            );
            continue;
        }
        let source_id = duduclaw_memory::lineage::canonical_import_id(&format!(
            "{}{}",
            ctx.lineage_prefix, doc.source
        ));
        let (mut stored, mut blocked, mut kept) = (0usize, 0usize, 0usize);
        for fact in &facts {
            let scan = duduclaw_security::input_guard::scan_input(
                fact,
                duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD,
            );
            if scan.blocked {
                blocked += 1;
                continue;
            }
            let Some(eng) = engine.as_ref() else {
                stored += 1;
                continue;
            };
            match store_fact(eng, ctx, fact, &source_id).await {
                Ok(true) => stored += 1,
                Ok(false) => kept += 1,
                Err(_) => blocked += 1,
            }
        }
        let mut detail = format!("→ 記憶 {stored} 條（低可信度，可依來源撤回）");
        if blocked > 0 {
            detail.push_str(&format!("；{blocked} 條未通過安全檢查或寫入失敗"));
        }
        if kept > 0 {
            detail.push_str(&format!("；{kept} 條已有更可信的內容或先前已撤回，未覆寫"));
        }
        if !ctx.dry_run {
            detail.push_str(&format!("；撤回指令：duduclaw memory forget-source --agent {} --session \"import:{source_id}\"", ctx.agent_id));
        }
        let params = serde_json::json!({ "stored": stored, "blocked": blocked, "kept": kept });
        out.push(if stored > 0 {
            Outcome::imported("memory", &doc.source, detail).with("slot.memory", params)
        } else {
            Outcome::skipped("memory", &doc.source, detail).with("slot.memory_none", params)
        });
    }
}

/// `Ok(true)` stored, `Ok(false)` refused/fenced (kept existing), `Err` failed.
async fn store_fact(
    eng: &duduclaw_memory::SqliteMemoryEngine,
    ctx: &SlotCtx<'_>,
    fact: &str,
    source_id: &str,
) -> Result<bool, String> {
    let now = chrono::Utc::now();
    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: ctx.agent_id.to_string(),
        content: fact.to_string(),
        timestamp: now,
        tags: vec![format!("imported-from-{}", ctx.format_code)],
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: format!("agent-import-{}", ctx.format_code),
    };
    // The `import` origin caps trust (0.7) and lets the supersession trust
    // guard keep anything an operator or a verified run already set.
    let meta = duduclaw_memory::TemporalMeta {
        origin: Some("import".to_string()),
        ..Default::default()
    };
    let prov = duduclaw_memory::lineage::Provenance::source(
        duduclaw_memory::SourceRef::import_item(source_id, fact.as_bytes(), now),
    );
    match eng
        .store_temporal_outcome(ctx.agent_id, entry, meta, prov)
        .await
    {
        Ok(duduclaw_memory::TemporalWriteOutcome::Stored(_)) => Ok(true),
        Ok(_) => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}
