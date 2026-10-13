use super::*;

fn rule(name: &str, agent: &str) -> AudienceRule {
    AudienceRule {
        name: name.into(),
        agent: agent.into(),
        ..Default::default()
    }
}

fn visitor() -> VisitorProfile {
    VisitorProfile {
        livechat_channel_id: Some(1),
        chatbot_script_id: None,
        country_code: Some("TW".into()),
        lang: Some("zh_TW".into()),
        logged_in: false,
        partner_tags: vec![],
        visitor_text: "你好，請問營業時間？".into(),
    }
}

#[test]
fn default_config_is_off_and_parses_from_empty_table() {
    let cfg = LivechatConfig::from_toml(&toml::Table::new());
    assert!(!cfg.enabled);
    assert_eq!(cfg.effective_poll_interval(), POLL_INTERVAL_DEFAULT);
}

#[test]
fn malformed_section_falls_back_to_disabled() {
    let table: toml::Table = "[odoo_livechat]\nenabled = \"yes\"\n".parse().unwrap();
    assert!(!LivechatConfig::from_toml(&table).enabled);
}

#[test]
fn parses_rules_from_toml() {
    let table: toml::Table = r#"
[odoo_livechat]
enabled = true
default_agent = "frontdesk"
poll_interval_seconds = 1

[[odoo_livechat.audience_rules]]
name = "日本訪客"
agent = "jp-support"
country_codes = ["JP"]
visitor = "anonymous"
"#
    .parse()
    .unwrap();
    let cfg = LivechatConfig::from_toml(&table);
    assert!(cfg.enabled);
    assert_eq!(cfg.effective_poll_interval(), POLL_INTERVAL_MIN);
    assert_eq!(cfg.audience_rules.len(), 1);
    let r = &cfg.audience_rules[0];
    assert!(r.enabled, "enabled defaults to true when omitted");
    assert_eq!(r.visitor, VisitorKind::Anonymous);
}

#[test]
fn first_matching_rule_wins_else_default() {
    let mut jp = rule("日本", "jp");
    jp.country_codes = vec!["jp".into()];
    let mut tw = rule("台灣", "tw");
    tw.country_codes = vec!["TW".into()];
    let catch_all = rule("全部", "all");
    let cfg = LivechatConfig {
        default_agent: "front".into(),
        audience_rules: vec![jp, tw, catch_all],
        ..Default::default()
    };
    let d = route(&cfg, &visitor());
    assert_eq!(d.agent, "tw");
    assert_eq!(d.rule.as_deref(), Some("台灣"));

    let no_rules = LivechatConfig {
        default_agent: "front".into(),
        ..Default::default()
    };
    let d = route(&no_rules, &visitor());
    assert_eq!(d, RouteDecision { agent: "front".into(), rule: None });
}

#[test]
fn disabled_rule_never_matches() {
    let mut r = rule("off", "x");
    r.enabled = false;
    assert!(!r.matches(&visitor()));
}

#[test]
fn criteria_are_anded_and_values_ored() {
    let mut r = rule("vip-tw", "vip");
    r.country_codes = vec!["JP".into(), "TW".into()];
    r.visitor = VisitorKind::LoggedIn;
    r.partner_tags = vec!["VIP".into()];
    let mut v = visitor();
    assert!(!r.matches(&v), "anonymous visitor fails logged_in");
    v.logged_in = true;
    assert!(!r.matches(&v), "no tags yet");
    v.partner_tags = vec![" vip ".into()];
    assert!(r.matches(&v), "tag compare is trimmed and case-insensitive");
    v.country_code = Some("US".into());
    assert!(!r.matches(&v));
}

#[test]
fn unknown_fact_fails_a_rule_that_needs_it() {
    let mut r = rule("jp", "jp");
    r.country_codes = vec!["JP".into()];
    let mut v = visitor();
    v.country_code = None;
    assert!(!r.matches(&v));
    let mut l = rule("en", "en");
    l.langs = vec!["en".into()];
    v.lang = None;
    assert!(!l.matches(&v));
}

#[test]
fn lang_base_code_matches_regional_variants() {
    assert!(lang_matches("zh", "zh_TW"));
    assert!(lang_matches("zh_TW", "zh_TW"));
    assert!(lang_matches("zh-tw", "zh_TW"));
    assert!(!lang_matches("zh_CN", "zh_TW"));
    assert!(!lang_matches("z", "zh_TW"));
    assert!(!lang_matches("", "zh_TW"));
}

#[test]
fn id_criteria_match_livechat_channel_and_chatbot() {
    let mut r = rule("sales-bot", "sales");
    r.chatbot_script_ids = vec![7];
    let mut v = visitor();
    assert!(!r.matches(&v));
    v.chatbot_script_id = Some(7);
    assert!(r.matches(&v));
    r.livechat_channel_ids = vec![2];
    assert!(!r.matches(&v));
}

#[test]
fn keywords_match_cjk_substrings_and_english_words() {
    let mut r = rule("退貨", "rma");
    r.keywords = vec!["退貨".into(), "refund".into()];
    let mut v = visitor();
    v.visitor_text = "我想要退貨謝謝".into();
    assert!(r.matches(&v));
    v.visitor_text = "Refund please".into();
    assert!(r.matches(&v));
    v.visitor_text = "refunded already".into();
    assert!(!r.matches(&v), "word boundary for ASCII keywords");
}

#[test]
fn handoff_detection() {
    let kws = LivechatConfig::default().handoff_keywords;
    assert!(is_handoff_request("可以幫我轉真人嗎", &kws));
    assert!(is_handoff_request("I want a HUMAN", &kws));
    assert!(!is_handoff_request("humanity", &kws));
    assert!(!is_handoff_request("營業時間？", &kws));
    assert!(!is_handoff_request("anything", &["  ".to_string()]));
}

#[test]
fn validate_rejects_bad_rules() {
    let mut cfg = LivechatConfig::default();
    assert!(cfg.validate().is_ok());
    cfg.audience_rules = vec![rule("", "a")];
    assert!(cfg.validate().unwrap_err().contains("第 1 條"));
    cfg.audience_rules = vec![rule("n", " ")];
    assert!(cfg.validate().is_err());
    let mut r = rule("n", "a");
    r.keywords = vec!["  ".into()];
    cfg.audience_rules = vec![r];
    assert!(cfg.validate().is_err());
    cfg.audience_rules = (0..=MAX_RULES).map(|i| rule(&i.to_string(), "a")).collect();
    assert!(cfg.validate().is_err());
    cfg.audience_rules = vec![];
    cfg.greeting = "字".repeat(MAX_TEXT_CHARS + 1);
    assert!(cfg.validate().is_err());
}

#[test]
fn livechat_channel_filter() {
    let mut cfg = LivechatConfig::default();
    assert!(cfg.serves_livechat_channel(None));
    cfg.livechat_channel_ids = vec![3];
    assert!(cfg.serves_livechat_channel(Some(3)));
    assert!(!cfg.serves_livechat_channel(Some(4)));
    assert!(!cfg.serves_livechat_channel(None));
}

#[test]
fn html_to_text_handles_odoo_bodies() {
    assert_eq!(html_to_text("<p>你好，請問營業時間？</p>"), "你好，請問營業時間？");
    assert_eq!(html_to_text("<p>a<br>b</p><p>c</p>"), "a\nb\nc");
    assert_eq!(
        html_to_text("<p>&lt;b&gt;x&lt;/b&gt; &amp; &#39;q&#39; &#x4e2d;&nbsp;y</p>"),
        "<b>x</b> & 'q' 中 y"
    );
    assert_eq!(html_to_text("AT&T 中文&"), "AT&T 中文&");
    assert_eq!(html_to_text("&unknown; 測&試"), "&unknown; 測&試");
}

#[test]
fn reply_html_escapes_everything_and_keeps_line_breaks() {
    let html = reply_to_html("**營業時間**\n- 平日 9:00–18:00\n<script>alert(1)</script> & \"q\"");
    assert_eq!(
        html,
        "營業時間<br>• 平日 9:00–18:00<br>&lt;script&gt;alert(1)&lt;/script&gt; &amp; &quot;q&quot;"
    );
}

#[test]
fn markdown_to_plain_strips_headings_and_fences() {
    let out = markdown_to_plain("## 標題\n```\ncode\n```\n* 項目 `x`\n  縮排");
    assert_eq!(out, "標題\ncode\n• 項目 x\n  縮排");
}

#[test]
fn cursor_domain_gives_each_session_its_own_cursor() {
    use serde_json::json;
    let one = rpc::cursor_domain(&[(3, 10)]);
    assert_eq!(
        one,
        json!([
            ["model", "=", "discuss.channel"],
            ["message_type", "=", "comment"],
            "&", ["res_id", "=", 3], ["id", ">", 10]
        ])
    );
    let two = rpc::cursor_domain(&[(3, 10), (4, 0)]);
    assert_eq!(
        two,
        json!([
            ["model", "=", "discuss.channel"],
            ["message_type", "=", "comment"],
            "|",
            "&", ["res_id", "=", 3], ["id", ">", 10],
            "&", ["res_id", "=", 4], ["id", ">", 0]
        ])
    );
}
