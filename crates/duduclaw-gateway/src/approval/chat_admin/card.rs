//! Content only: no delivery, identity resolution or decision side effects.
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;
use duduclaw_security::credential_protection::sanitize_output_text;

use crate::approval::ApprovalRecord;
use crate::decision_action::{encode, DecisionAct, DecisionSource};
use crate::trigger_route::texts::Lang;
use super::{chat_tier, kind_key, Tier};

#[derive(Debug)]
pub(crate) struct ChatCard {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) approve_data: String,
    pub(crate) deny_data: String,
}

fn words(lang: Lang, zh: &'static str, en: &'static str, ja: &'static str) -> &'static str {
    match lang { Lang::ZhTw => zh, Lang::En => en, Lang::Ja => ja }
}

/// Neutralize syntax before any adapter sees it. Sanitize credentials before
/// punctuation changes could hide their shape; cap again after sanitization.
fn field(text: &str, max: usize) -> String {
    let clean = sanitize_output_text(text);
    let preview = crate::markdown_render::untrusted_preview("telegram", &clean, max);
    duduclaw_core::plain_line(&preview, max)
}

fn add(lines: &mut Vec<String>, label: &str, value: Option<&str>, max: usize) {
    if let Some(value) = value {
        let value = field(value, max);
        if !value.is_empty() { lines.push(format!("{label}：{value}")); }
    }
}

pub(crate) fn kind_label(rec: &ApprovalRecord, lang: Lang) -> &'static str {
    kind_label_for_key(kind_key(rec).unwrap_or(""), lang)
}

pub(crate) fn kind_label_for_key(key: &str, lang: Lang) -> &'static str {
    match key {
        "knowledge_quarantine" => words(lang, "知識審核", "Knowledge review", "知識の確認"),
        "knowledge_add_source" => words(lang, "加入知識庫", "Add to knowledge base", "ナレッジへの追加"),
        "expert_skills_enable" => words(lang, "啟用技能", "Enable skills", "スキルの有効化"),
        "workflow_activation" => words(lang, "啟用工作流", "Activate workflow", "ワークフローの有効化"),
        "agent_file_change" => words(lang, "修改員工設定", "Change employee settings", "従業員設定の変更"),
        "org_change" => words(lang, "調整組織", "Change organization", "組織の変更"),
        "autopilot_action" => words(lang, "自動化操作", "Automation action", "自動操作"),
        "cli_egress" | "egress_guard" => words(lang, "對外操作", "Outbound operation", "外部への操作"),
        _ => words(lang, "待核准事項", "Approval request", "承認依頼"),
    }
}

/// Blocking for profile-file sidecar reads; async delivery callers should use
/// spawn_blocking. `lang` is supplied by trigger_route::texts::lang_for(home).
pub(crate) fn build(home: &Path, rec: &ApprovalRecord, valid_until: DateTime<Utc>, lang: Lang) -> Option<ChatCard> {
    if chat_tier(Some(home), rec) == Tier::T3 { return None; }
    // Legacy/deserialized store keys are not guaranteed to be UUIDs. Refuse
    // malformed IDs rather than truncate the displayed ID or callback.
    uuid::Uuid::parse_str(rec.id.as_str()).ok()?;
    if rec.id.as_str().len() != 36 { return None; }
    let key = kind_key(rec)?;
    let p = &rec.payload;
    let mut lines = vec![kind_label(rec, lang).to_owned()];
    let employee = words(lang, "員工", "Employee", "従業員");
    match key {
        // This branch must precede all content reads, including sidecars.
        "cli_egress" | "egress_guard" => add(&mut lines, employee, Some(&rec.agent_id), 80),
        "knowledge_quarantine" => {
            add(&mut lines, employee, Some(&rec.agent_id), 80);
            let state = if p["disposition"].as_str() == Some(crate::wiki_ingest::DISPOSITION_TRUST_HELD) {
                words(lang, "新說法與目前內容不一致，尚未採用", "Conflicting claim, not adopted", "既存の内容と異なるため未採用")
            } else {
                words(lang, "同一來源短時間寫入大量知識，已暫時隔離", "Bulk knowledge from one source, quarantined", "同じ情報源からの大量の知識を一時隔離中")
            };
            add(&mut lines, words(lang, "狀況", "State", "状況"), Some(state), 120);
            add(&mut lines, words(lang, "說法預覽", "Claim preview", "記述のプレビュー"), p["snippet"].as_str(), 300);
        }
        "knowledge_add_source" => {
            let labels = [words(lang, "標題", "Title", "タイトル"),
                words(lang, "文件／網址", "Document / URL", "文書／URL"),
                words(lang, "預覽", "Preview", "プレビュー")];
            for ((key, max), label) in crate::knowledge::add_source_chat::card_fields(rec).into_iter().zip(labels) {
                add(&mut lines, label, p[key].as_str(), max);
            }
        }
        "expert_skills_enable" => {
            add(&mut lines, words(lang, "套件", "Package", "パッケージ"), p["slug"].as_str(), 120);
            if let Some(skills) = p["skills"].as_array() {
                for skill in skills.iter().take(5) {
                    add(&mut lines, words(lang, "技能", "Skill", "スキル"), skill.as_str(), 80);
                }
            }
            // request_enable stores no scan result. Do not infer a pass.
        }
        "workflow_activation" => {
            let definition = &p["request"]["revision"]["definition"];
            if let Some(steps) = definition["steps"].as_array() {
                lines.push(format!("{}：{}", words(lang, "步驟數", "Steps", "ステップ数"), steps.len()));
            }
            // ActivationRequest has no display name or estimated cost.
            // workflow_id is an identifier; a budget is not an estimate.
        }
        "agent_file_change" => {
            add(&mut lines, employee, Some(&rec.agent_id), 80);
            if let Some((_, key)) = crate::agent_files::approval::target(rec) {
                use crate::agent_files::FileKey;
                let name = match key {
                    FileKey::Soul => words(lang, "人格設定", "Personality", "人格設定"),
                    FileKey::Identity => words(lang, "自我介紹", "Introduction", "自己紹介"),
                    FileKey::User => words(lang, "服務對象", "People served", "サービス対象"),
                    FileKey::Instructions => words(lang, "工作規則", "Work rules", "業務ルール"),
                    FileKey::Contract => words(lang, "行為界線", "Behavior boundaries", "行動の制約"),
                    FileKey::AgentToml => words(lang, "基本設定", "Basic settings", "基本設定"),
                    FileKey::Skill(_) => words(lang, "技能", "Skill", "スキル"),
                };
                add(&mut lines, words(lang, "修改項目", "Settings", "変更対象"), Some(name), 80);
            }
            // pending_json validates the sidecar content hash. A stale base
            // must not be presented as the originally proposed diff.
            if let Ok(pending) = crate::agent_files::approval::pending_json(home, rec) {
                if pending["stale"] == false && pending["content_available"] == true {
                    let diff = &pending["diff"];
                    for (key, label) in [("added", words(lang, "新增行數", "Added lines", "追加行数")),
                        ("removed", words(lang, "刪除行數", "Removed lines", "削除行数"))] {
                        if let Some(n) = diff[key].as_u64() { lines.push(format!("{label}：{n}")); }
                    }
                    if let Some(hunks) = diff["hunks"].as_array() {
                        let mut preview = String::new();
                        for hunk in hunks {
                            let prefix = match hunk["op"].as_str() { Some("add") => "+", Some("remove") => "-", _ => " " };
                            if let Some(rows) = hunk["lines"].as_array() {
                                for row in rows.iter().filter_map(Value::as_str) {
                                    if preview.chars().count() >= 300 { break; }
                                    preview.push_str(prefix);
                                    preview.push_str(&duduclaw_core::truncate_chars(row, 300 - preview.chars().count()));
                                    preview.push(' ');
                                }
                            }
                            if preview.chars().count() >= 300 { break; }
                        }
                        add(&mut lines, words(lang, "差異", "Diff", "差分"), Some(&preview), 300);
                    }
                }
            }
        }
        "org_change" => {
            add(&mut lines, employee, Some(&rec.agent_id), 80);
            for (key, label) in [("reports_to", words(lang, "上級", "Manager", "上司")),
                ("department", words(lang, "部門", "Department", "部署"))] {
                let before = p["before"][key].as_str();
                let after = p["after"][key].as_str();
                if before.is_some() || after.is_some() {
                    lines.push(format!("{label}：{} → {}", field(before.unwrap_or(""), 80), field(after.unwrap_or(""), 80)));
                }
            }
        }
        "autopilot_action" => {
            let dispatch = &p[crate::autopilot_approval::PAYLOAD_KEY];
            add(&mut lines, words(lang, "規則", "Rule", "ルール"), dispatch["rule_name"].as_str(), 120);
            let action = &dispatch["action"];
            let (label, target) = match action["type"].as_str() {
                Some("delegate") => (words(lang, "交辦工作", "Delegate work", "業務の依頼"), "target_agent"),
                Some("run_skill") => (words(lang, "執行技能", "Run skill", "スキルの実行"), "target_agent"),
                Some("notify") => (words(lang, "傳送通知", "Send notification", "通知の送信"), "chat_id"),
                Some("proactive_notify") => (words(lang, "主動通知", "Proactive notification", "自発的な通知"), "chat_id"),
                _ => ("", ""),
            };
            add(&mut lines, words(lang, "動作", "Action", "操作"), Some(label), 80);
            add(&mut lines, words(lang, "對象", "Target", "対象"), action[target].as_str(), 120);
        }
        _ => return None,
    }
    lines.push(format!("{}：{} UTC", words(lang, "有效期限", "Valid until", "有効期限"), valid_until.format("%Y/%m/%d %H:%M")));
    // Approval IDs are typed store keys, never user prose. Preserve the full UUID.
    lines.push(format!("{}：{}", words(lang, "編號", "Approval ID", "承認番号"), rec.id.as_str()));
    lines.push(words(lang, "核准只能按這張卡上的按鈕；別人轉來的編號或文字不會生效",
        "Use only the buttons on this card; forwarded IDs or text cannot approve this request.",
        "承認にはこのカードのボタンを使ってください。転送された番号や文章では承認できません。").into());
    Some(ChatCard {
        title: words(lang, "需要你核准", "Your approval is needed", "承認をお願いします").into(),
        body: lines.join("\n"),
        approve_data: encode(DecisionSource::Approval, DecisionAct::Approve, rec.id.as_str()),
        deny_data: encode(DecisionSource::Approval, DecisionAct::Deny, rec.id.as_str()),
    })
}

/// HTML adapters escape even trusted scaffolding; untrusted fields have already
/// been neutralized. Never run these strings through a Markdown renderer.
pub(crate) fn render_telegram_html(card: &ChatCard) -> String {
    format!("<b>{}</b>\n{}", crate::goal_state::xml_escape(&card.title), crate::goal_state::xml_escape(&card.body))
}

impl ChatCard {
    pub(crate) fn button_labels(&self) -> (&'static str, &'static str) {
        match self.title.as_str() {
            "Your approval is needed" => ("Approve", "Deny"),
            "承認をお願いします" => ("承認", "拒否"),
            _ => ("核准", "拒絕"),
        }
    }
}

pub(crate) fn result_text(kind_label: &str, approve: bool, decider_name: &str, at: DateTime<Utc>, channel_label: &str, lang: Lang) -> String {
    let kind = field(kind_label, 80);
    let decider = field(decider_name, 80);
    let channel = field(channel_label, 40);
    let time = at.format("%m/%d %H:%M UTC");
    let outcome = if approve { words(lang, "✓ 已核准", "✓ Approved", "✓ 承認済み") }
        else { words(lang, "✗ 已拒絕", "✗ Denied", "✗ 拒否済み") };
    format!("{outcome}：{kind}（{decider}，{time}，{channel}）")
}

pub(crate) fn expired_text(kind_label: &str, lang: Lang) -> String {
    format!("{}：{}", words(lang, "已失效", "Expired", "期限切れ"), field(kind_label, 80))
}

pub(crate) fn feature_off_text(lang: Lang) -> &'static str {
    words(lang, "聊天核准已關閉，請到儀表板處理", "Chat approvals are off. Please use the dashboard.", "チャットでの承認は無効です。ダッシュボードで対応してください。")
}

pub(crate) fn step_up_needed_text(lang: Lang) -> &'static str {
    words(lang, "需要再確認一次", "One more confirmation is needed", "追加の確認が必要です")
}

pub(crate) fn already_decided_text(decider: &str, channel: &str, approve: bool, lang: Lang) -> String {
    let decider = field(decider, 80);
    let channel = field(channel, 40);
    match lang {
        Lang::ZhTw => format!("這筆先前已由 {decider} 在 {channel} {}", if approve { "核准" } else { "拒絕" }),
        Lang::En => format!("This was already {} by {decider} on {channel}", if approve { "approved" } else { "denied" }),
        Lang::Ja => format!("この依頼は {decider} が {channel} で{}しました", if approve { "承認" } else { "拒否" }),
    }
}

pub(crate) fn reminder_text(lang: Lang) -> &'static str {
    words(lang, "這張核准卡快到期了，請在期限內按原卡上的按鈕；逾時會自動拒絕",
        "This approval card expires soon. Use the original card's buttons before the deadline; expiry means denial.",
        "この承認カードの期限が近づいています。期限内に元のカードのボタンを押してください。期限切れは拒否として扱われます。")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(kind: &str, payload: Value) -> ApprovalRecord {
        serde_json::from_value(json!({
            "id": "12345678-1234-4234-8234-123456789abc", "agent_id": "anna",
            "action_kind": kind, "summary": "not card content", "payload": payload,
            "status": "pending", "created_at": "2026-11-27T00:00:00Z",
            "decided_at": null, "decided_by": null, "ttl_seconds": 3600
        })).unwrap()
    }

    fn deadline() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-27T14:03:00Z").unwrap().with_timezone(&Utc)
    }

    #[test]
    fn card_fields_escaped_and_bounded() {
        let home = tempfile::tempdir().unwrap();
        let attack = format!("<b>& [link](https://evil.test) @all\n\u{202e}\u{200b}\u{061c}{}", "界🦀".repeat(5000));
        let mut rec = record("knowledge_quarantine", json!({"disposition":"trust_held", "snippet":attack}));
        rec.agent_id = attack.clone();
        let card = build(home.path(), &rec, deadline(), Lang::ZhTw).unwrap();
        assert_eq!(card.body.lines().count(), 7);
        for forbidden in ["<b>", "[link]", "https://", "@all", "\u{202e}", "\u{200b}", "\u{061c}"] {
            assert!(!card.body.contains(forbidden), "{forbidden}");
        }
        let claim = card.body.lines().find_map(|l| l.strip_prefix("說法預覽：")).unwrap();
        assert!(claim.chars().count() <= 300);
        assert!(card.body.lines().find_map(|l| l.strip_prefix("員工：")).unwrap().chars().count() <= 80);
        assert_eq!(field(&"界🦀".repeat(5000), 301).chars().count(), 301);
        assert_eq!(field(&"界🦀".repeat(5000), 301).chars().last(), Some('界'));
        // Test escaping at the adapter boundary independently of neutralization.
        let literal = ChatCard { title: "<>&".into(), body: "<a href='x'>x</a>&".into(),
            approve_data: card.approve_data, deny_data: card.deny_data };
        let html = render_telegram_html(&literal);
        assert!(html.contains("&lt;&gt;&amp;"));
        assert!(!html.contains("<a "));
        let google = crate::channel_format::googlechat_approval_card(&literal);
        assert!(!google[0]["card"]["sections"][0]["widgets"][0]["textParagraph"]["text"].as_str().unwrap().contains("<a "));
    }

    #[test]
    fn t3_records_never_build() {
        let home = tempfile::tempdir().unwrap();
        for kind in ["unknown", "egress_guard", crate::isolation_background::PERSONAL_DELEGATION_KIND]
            .into_iter().chain(crate::approval::operator_cli_gate::OPERATOR_CLI_KINDS.iter().map(|s| s.kind)) {
            assert!(build(home.path(), &record(kind, json!({})), deadline(), Lang::ZhTw).is_none(), "{kind}");
        }
        let rec = record(crate::isolation_background::PERSONAL_DELEGATION_KIND, json!({"egress_guard":{}}));
        assert!(build(home.path(), &rec, deadline(), Lang::ZhTw).is_none());
        let mut malformed = record("knowledge_quarantine", json!({}));
        malformed.id = "<b>not-a-uuid</b>".to_string().into();
        assert!(build(home.path(), &malformed, deadline(), Lang::ZhTw).is_none());
    }

    #[test]
    fn egress_cards_never_include_outbound_content() {
        let home = tempfile::tempdir().unwrap();
        let secret = "secret-outbound-customer-record-987654";
        for kind in ["cli_egress", "knowledge_add_source", "agent_file_change", "unknown"] {
            let rec = record(kind, json!({"egress_guard": {"sources":[secret]},
                "arguments":{"text":secret}, "title":secret, "preview":secret, "snippet":secret}));
            let card = build(home.path(), &rec, deadline(), Lang::ZhTw).unwrap();
            assert_eq!(card.body.lines().count(), 5);
            assert!(card.body.starts_with("對外操作\n員工：anna\n有效期限："));
            assert!(!card.body.contains(secret));
            assert!(!crate::channel_format::googlechat_approval_card(&card).to_string().contains(secret));
        }
        let card = build(home.path(), &record("cli_egress", json!({"arguments":{"text":secret}})), deadline(), Lang::ZhTw).unwrap();
        assert!(!card.body.contains(secret));
    }

    #[test]
    fn callbacks_and_googlechat_buttons_round_trip() {
        let home = tempfile::tempdir().unwrap();
        let rec = record("knowledge_quarantine", json!({}));
        let card = build(home.path(), &rec, deadline(), Lang::ZhTw).unwrap();
        for (data, act) in [(&card.approve_data, DecisionAct::Approve), (&card.deny_data, DecisionAct::Deny)] {
            assert!(data.len() <= 64);
            let parsed = crate::decision_action::parse(data).unwrap();
            assert_eq!(parsed.source, DecisionSource::Approval);
            assert_eq!(parsed.act, act);
            assert_eq!(parsed.id, rec.id.as_str());
        }
        assert!(card.body.contains(rec.id.as_str()));
        let cards = crate::channel_format::googlechat_approval_card(&card);
        let widgets = cards[0]["card"]["sections"][0]["widgets"].as_array().unwrap();
        let buttons: Vec<_> = widgets.iter().filter_map(|w| w["buttonList"]["buttons"].as_array()).flatten().collect();
        assert_eq!(buttons.len(), 2);
        for (button, (label, data)) in buttons.iter().zip([("核准", &card.approve_data), ("拒絕", &card.deny_data)]) {
            assert_eq!(button["text"], label);
            let action = &button["onClick"]["action"];
            assert_eq!(action["function"], crate::channel_format::GOOGLECHAT_DECISION_FUNCTION);
            assert_eq!(action["parameters"], json!([{"key":crate::channel_format::GOOGLECHAT_DECISION_PARAM,"value":data}]));
        }
    }

    #[test]
    fn results_sanitize_display_names_in_every_language() {
        let attack = format!("王小明\n<b>@all</b>\u{202e}\u{200b}{}", "界".repeat(10000));
        for lang in [Lang::ZhTw, Lang::En, Lang::Ja] {
            for approve in [true, false] {
                for text in [result_text("知識審核", approve, &attack, deadline(), "Telegram", lang),
                    already_decided_text(&attack, &attack, approve, lang), expired_text(&attack, lang)] {
                    assert_eq!(text.lines().count(), 1);
                    assert!(text.chars().count() < 250);
                    for forbidden in ["<b>", "@all", "\u{202e}", "\u{200b}"] { assert!(!text.contains(forbidden)); }
                }
            }
            assert!(!feature_off_text(lang).is_empty());
            assert!(!step_up_needed_text(lang).is_empty());
            assert!(!reminder_text(lang).is_empty());
        }
        assert_eq!(result_text("知識審核", true, "王小明", deadline(), "Telegram", Lang::ZhTw),
            "✓ 已核准：知識審核（王小明，11/27 14:03 UTC，Telegram）");
        assert_eq!(already_decided_text("王小明", "Telegram", true, Lang::ZhTw), "這筆先前已由 王小明 在 Telegram 核准");
    }

    #[test]
    fn all_t1_t2_payload_shapes_and_missing_fields() {
        let home = tempfile::tempdir().unwrap();
        // Shapes from the production constructors, also used by tier,
        // org_change_approval and autopilot_approval fixture tests.
        let cases = [
            ("knowledge_quarantine", json!({"agent_id":"anna", "disposition":crate::wiki_ingest::DISPOSITION_TRUST_HELD,
                "snippet":"退款期限為三十天", "quarantined_ids":["claim-1"]}), "退款期限為三十天"),
            ("knowledge_add_source", json!({"title":"新文件", "url":"https://example.org/paper", "preview":"文件摘要"}), "文件摘要"),
            ("expert_skills_enable", json!({"slug":"support", "action":"enable_imported_skills",
                "skills":["anna/one","anna/two","anna/three","anna/four","anna/five","NEVER_SIXTH"],
                "entries":[{"agent":"anna","name":"one","digest":"hash"}]}), "套件：support"),
            ("workflow_activation", json!({"kind":"workflow_activation", "request":{"revision":{"definition":{
                "workflow_id":"wf-1", "steps":[{"step_id":"one"},{"step_id":"two"}]}}}}), "步驟數：2"),
            ("agent_file_change", json!({"agent_id":"anna","file":"SOUL.md","base_hash":"base","content_sha256":"sha"}), "修改項目：人格設定"),
            ("org_change", json!({"agent_id":"anna", "before":{"reports_to":"amy","department":"業務"},
                "after":{"reports_to":"bob","department":"客服"}, "base":"hash"}), "上級：amy → bob"),
            ("autopilot_action", json!({"autopilot_dispatch":{"rule_name":"提醒", "action":{
                "type":"delegate","target_agent":"anna","prompt":"PRIVATE_PROMPT"}}}), "動作：交辦工作"),
            ("cli_egress", json!({"name":"send", "arguments":{"text":"PRIVATE_PROMPT"},
                "egress_guard":{"reason":"egress_guard","sources":[]}}), "對外操作"),
        ];
        for (kind, payload, expected) in cases {
            let card = build(home.path(), &record(kind, payload), deadline(), Lang::ZhTw).unwrap();
            assert!(card.body.contains(expected), "{kind}: {}", card.body);
            assert!(!card.body.contains("NEVER"));
            assert!(!card.body.contains("PRIVATE_PROMPT"));
            for lang in [Lang::ZhTw, Lang::En, Lang::Ja] {
                let missing = build(home.path(), &record(kind, json!({})), deadline(), lang).unwrap();
                assert!(!missing.body.contains(kind));
                assert!(!missing.title.is_empty());
            }
        }
    }

    #[test]
    fn profile_diff_requires_available_matching_sidecar_and_base() {
        let home = tempfile::tempdir().unwrap();
        let current = "old\n";
        let proposed = format!("new\n{}\n", "界".repeat(10000));
        let mut rec = record("agent_file_change", json!({"agent_id":"anna","file":"SOUL.md",
            "base_hash":crate::agent_files::hash_of(current),
            "content_sha256":crate::agent_files::hash_of(&proposed)}));
        std::fs::create_dir_all(home.path().join("agents/anna")).unwrap();
        let path = crate::agent_files::file_path(home.path(), "anna", &crate::agent_files::FileKey::Soul).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, current).unwrap();
        let pending = home.path().join("agent_file_pending");
        std::fs::create_dir_all(&pending).unwrap();
        let sidecar = pending.join(format!("{}.txt", rec.id.as_str()));
        std::fs::write(&sidecar, &proposed).unwrap();
        let card = build(home.path(), &rec, deadline(), Lang::ZhTw).unwrap();
        assert!(card.body.contains("新增行數：2"));
        assert!(card.body.contains("刪除行數：1"));
        assert!(card.body.lines().find_map(|l| l.strip_prefix("差異：")).unwrap().chars().count() <= 300);
        rec.payload["base_hash"] = json!("wrong");
        assert!(!build(home.path(), &rec, deadline(), Lang::ZhTw).unwrap().body.contains("差異："));
        rec.payload["base_hash"] = json!(crate::agent_files::hash_of(current));
        std::fs::write(sidecar, "tampered").unwrap();
        assert!(!build(home.path(), &rec, deadline(), Lang::ZhTw).unwrap().body.contains("差異："));
    }
}
