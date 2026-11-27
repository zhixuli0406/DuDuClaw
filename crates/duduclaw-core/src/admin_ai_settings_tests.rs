use super::*;
use serde_json::json;

fn bounds() -> Bounds {
    Bounds { timeout_max: 3600, turn_limit: 100, idle_timeout_min: 60,
        idle_timeout_max: 3600, idle_timeout_allow_zero: true }
}

fn field_item(field: AdminAiField, value: Value) -> Item {
    Item::AgentField { agent_id: "alice".into(), field, value }
}

fn accepts(field: AdminAiField, value: Value) {
    assert!(validate_item(&field_item(field, value.clone()), &bounds()).is_ok(), "{field:?}: {value}");
}

fn refuses(field: AdminAiField, value: Value) -> AdminAiRefusal {
    let error = validate_item(&field_item(field, value), &bounds()).unwrap_err();
    assert!(error.message.contains(field.label("zh-TW")));
    assert!(!error.message.contains(field.toml_path()));
    error
}

#[test]
fn admin_ai_whitelist_metadata_and_authority_exceptions() {
    let expected = [
        ("display_name", "agent.display_name", "顯示名稱"),
        ("icon", "agent.icon", "圖示"), ("trigger", "agent.trigger", "呼叫名稱"),
        ("status", "agent.status", "狀態"), ("reply_language", "prompt.reply_language", "回覆語言"),
        ("model", "model.preferred", "主要模型"), ("fallback_model", "model.fallback", "備援模型"),
        ("effort", "model.effort", "思考強度"),
        ("timeout_secs", "runtime.timeout_secs", "單次執行時間上限"),
        ("max_turns", "runtime.max_turns", "每次最多步數"),
        ("idle_timeout_secs", "runtime.idle_timeout_secs", "閒置逾時"),
        ("heartbeat_enabled", "heartbeat.enabled", "定時喚醒"),
        ("heartbeat_cron", "heartbeat.cron", "定時喚醒時間"),
        ("heartbeat_interval", "heartbeat.interval_seconds", "定時喚醒間隔"),
        ("heartbeat_cron_timezone", "heartbeat.cron_timezone", "時區"),
    ];
    assert_eq!(AdminAiField::ALL.len(), expected.len());
    for (field, (name, path, label)) in AdminAiField::ALL.iter().zip(expected) {
        assert_eq!(field.tool_name(), name);
        assert_eq!(field.toml_path(), path);
        assert_eq!(field.label("zh-TW"), label);
        assert!(!field.label("en").is_empty());
        assert!(!field.label("ja").is_empty());
        assert_eq!(serde_json::to_value(field).unwrap(), json!(name));
        assert_eq!(serde_json::from_value::<AdminAiField>(json!(name)).unwrap(), *field);
        for leaf in field.paths() {
            for authority in crate::agent_toml::authority::AUTHORITY {
                if leaf == authority.toml_key || leaf.strip_prefix(authority.toml_key).is_some_and(|s| s.starts_with('.')) {
                    assert!(["prompt.reply_language", "runtime.timeout_secs", "runtime.max_turns", "runtime.idle_timeout_secs"].contains(&leaf));
                }
            }
        }
        assert_eq!(field.paths().len(), if *field == AdminAiField::DisplayName { 2 } else { 1 });
    }
    assert!(AdminAiField::DisplayName.paths().contains("agent.trigger"));
}

#[test]
fn admin_ai_every_field_rejects_wrong_types() {
    use AdminAiField::*;
    for &field in AdminAiField::ALL {
        let wrong = match field {
            HeartbeatEnabled => vec![json!("true"), json!(0), json!(1)],
            TimeoutSecs | MaxTurns | IdleTimeoutSecs | HeartbeatInterval =>
                vec![json!("300"), json!(true), json!(300.0), json!(3.5)],
            _ => vec![json!(true), json!(123)],
        };
        for value in wrong.into_iter().chain([Value::Null, json!([]), json!({})]) {
            assert_eq!(refuses(field, value).code, "wrong_type");
        }
    }
}

#[test]
fn admin_ai_numeric_lower_and_upper_bounds() {
    use AdminAiField::*;
    for (field, min, max) in [(TimeoutSecs, 30, 3600), (MaxTurns, 1, 100),
        (IdleTimeoutSecs, 60, 3600), (HeartbeatInterval, 300, 86400)] {
        accepts(field, json!(min));
        accepts(field, json!(max));
        refuses(field, json!(min - 1));
        refuses(field, json!(max + 1));
        refuses(field, json!(-1));
        refuses(field, json!(u64::MAX));
    }
    accepts(IdleTimeoutSecs, json!(0));
    let mut b = bounds();
    b.idle_timeout_allow_zero = false;
    assert!(validate_item(&field_item(IdleTimeoutSecs, json!(0)), &b).is_err());
    b.timeout_max = 29;
    b.turn_limit = 0;
    b.idle_timeout_min = 3601;
    for (field, value) in [(TimeoutSecs, 30), (MaxTurns, 1), (IdleTimeoutSecs, 3600)] {
        assert!(validate_item(&field_item(field, json!(value)), &b).is_err());
    }
    accepts(HeartbeatEnabled, json!(false));
    accepts(HeartbeatEnabled, json!(true));
}

#[test]
fn admin_ai_profile_lengths_and_trigger_format() {
    use AdminAiField::*;
    for (field, max) in [(DisplayName, 64), (Icon, 8)] {
        accepts(field, json!("員"));
        accepts(field, json!("員".repeat(max)));
        refuses(field, json!(""));
        refuses(field, json!("員".repeat(max + 1)));
    }
    accepts(Icon, json!("🐾"));
    refuses(DisplayName, json!("  "));
    for markup in ["<name>", "[name]", "{name}", "`name`"] { refuses(DisplayName, json!(markup)); }
    accepts(Trigger, json!("@員工"));
    accepts(Trigger, json!(format!("@{}", "員".repeat(32))));
    for bad in ["", "name", "@", "@a", "@a b", "@a\u{3000}b", " @ab"] { refuses(Trigger, json!(bad)); }
    refuses(Trigger, json!(format!("@{}", "員".repeat(33))));
    let normalized = validate_item(&field_item(DisplayName, json!(" 員工 ")), &bounds()).unwrap();
    assert_eq!(normalized.into_item(), field_item(DisplayName, json!("員工")));
}

#[test]
fn admin_ai_rejects_control_bidi_zero_width_and_unassigned() {
    for c in ['\0', '\n', '\t', '\u{85}', '\u{61c}', '\u{202e}', '\u{2066}',
        '\u{200b}', '\u{200d}', '\u{feff}', '\u{ad}', '\u{378}', '\u{10ffff}'] {
        for field in [AdminAiField::DisplayName, AdminAiField::Icon, AdminAiField::Trigger] {
            let text = if field == AdminAiField::Trigger { format!("@ab{c}") } else { format!("員{c}") };
            assert_eq!(refuses(field, json!(text)).code, "unsafe_character", "{field:?} {c:?}");
        }
    }
}

#[test]
fn admin_ai_enum_languages_and_model_values() {
    use AdminAiField::*;
    for status in ["active", "paused"] { accepts(Status, json!(status)); }
    for status in ["terminated", "", "ACTIVE", "pending"] { refuses(Status, json!(status)); }
    for language in crate::profile_sections::REPLY_LANGUAGE_CHOICES { accepts(ReplyLanguage, json!(language)); }
    accepts(ReplyLanguage, json!(""));
    for bad in ["fr", "ja", "zh", "English", " en"] { refuses(ReplyLanguage, json!(bad)); }
    for effort in ["", "low", "medium", "high", "xhigh", "max"] { accepts(Effort, json!(effort)); }
    for bad in ["auto", "ultra", "HIGH", " high"] { refuses(Effort, json!(bad)); }
    accepts(Model, json!("catalog-model"));
    accepts(FallbackModel, json!("catalog-model"));
    accepts(FallbackModel, json!(""));
    refuses(Model, json!(""));
    refuses(Model, json!("  "));
    // Model compatibility and catalog membership cannot be checked without live state.
}

#[test]
fn admin_ai_cron_timezone_and_cadence() {
    use AdminAiField::*;
    for cron in ["*/15 * * * *", "0 0 * * *", "0 0 9 * * MON-FRI", "0 0 9 * * 1-5", "0 0 9 * * * 2099"] {
        accepts(HeartbeatCron, json!(cron));
        assert_eq!(validate_item(&field_item(HeartbeatCron, json!(cron)), &bounds()).unwrap().into_item(),
            field_item(HeartbeatCron, json!(cron)), "must preserve Unix weekday semantics");
    }
    for bad in ["", "bad", "0 25 * * *", "* * * * *", "0 0,14 * * * *", "0 0,50 * * * *", "* * * * * * 2099"] {
        refuses(HeartbeatCron, json!(bad));
    }
    // Exact parser size limit, without changing cadence.
    let prefix = "0 0 * * *";
    accepts(HeartbeatCron, json!(format!("{prefix}{}", " ".repeat(256 - prefix.len()))));
    refuses(HeartbeatCron, json!(format!("{prefix}{}", " ".repeat(257 - prefix.len()))));
    for zone in ["UTC", "Asia/Taipei", "America/New_York"] { accepts(HeartbeatCronTimezone, json!(zone)); }
    for bad in ["", "Taipei", "UTC+8", "Invalid/Zone"] { refuses(HeartbeatCronTimezone, json!(bad)); }
    assert_eq!(validate_item(&field_item(HeartbeatCronTimezone, json!(" Asia/Taipei ")), &bounds()).unwrap().into_item(),
        field_item(HeartbeatCronTimezone, json!("Asia/Taipei")));
}

#[test]
fn admin_ai_unknown_fields_and_dashboard_only_refusal() {
    for name in ["runtime.provider", "provider", "capabilities", "memory.language", "DISPLAY_NAME", "", "budget"] {
        let error = AdminAiField::from_tool_name(name).unwrap_err();
        assert_eq!(error.code, "unknown_field");
        for entry in DASHBOARD_ONLY {
            assert!(error.message.contains(entry.label));
            assert!(error.message.contains(entry.reason));
        }
        assert!(serde_json::from_value::<Item>(json!({"kind":"agent_field","agent_id":"alice","field":name,"value":true})).is_err());
    }
    assert_eq!(DASHBOARD_ONLY.len(), 12);
    let error = AdminAiRefusal::unknown_field();
    for internal in [".toml", ".mcp.json", "runtime.provider", "[capabilities]", "SOUL.md"] {
        assert!(!error.message.contains(internal));
    }
    for value in [
        json!({"kind":"agent_field","agent_id":"alice","field":"icon","value":"🐾","extra":true}),
        json!({"kind":"skill","agent_id":"alice","skill":"report","enabled":true,"extra":true}),
        json!({"kind":"skill","agent_id":"alice","skill":"report","enabled":"true"}),
        json!({"kind":"unknown"}),
    ] { assert!(serde_json::from_value::<Item>(value).is_err()); }
}

fn create_schedule() -> Value {
    json!({"kind":"schedule","op":"create","agent_id":"alice","name":"每日報告","cron":"0 9 * * *","task":"整理今日摘要"})
}

#[test]
fn admin_ai_schedule_op_field_sets_are_closed() {
    let valid = [create_schedule(),
        json!({"kind":"schedule","op":"update","schedule_id":"s1","name":"更新","cron":"0 10 * * *","timezone":"Asia/Taipei","task":"新提示"}),
        json!({"kind":"schedule","op":"update","schedule_id":"s1"}),
        json!({"kind":"schedule","op":"pause","schedule_id":"s1"}),
        json!({"kind":"schedule","op":"resume","schedule_id":"s1"}),
        json!({"kind":"schedule","op":"delete","schedule_id":"s1"})];
    for value in valid {
        let item: Item = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&item).unwrap(), value);
        assert!(validate_item(&item, &bounds()).is_ok());
        let mut extra = value.clone();
        extra["extra"] = json!(true);
        assert!(serde_json::from_value::<Item>(extra).is_err());
        let mut cross_op = value.clone();
        let op = value["op"].as_str().unwrap();
        if op == "create" { cross_op["schedule_id"] = json!("s1"); }
        else { cross_op["agent_id"] = json!("alice"); }
        assert!(serde_json::from_value::<Item>(cross_op).is_err());
        if matches!(op, "pause" | "resume" | "delete") {
            for field in ["name", "cron", "timezone", "task"] {
                let mut extra = value.clone();
                extra[field] = json!("extra");
                assert!(serde_json::from_value::<Item>(extra).is_err());
            }
        }
    }
    for field in ["agent_id", "name", "cron", "task"] {
        let mut missing = create_schedule();
        missing.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<Item>(missing).is_err());
    }
    for op in ["run_now", "install", "", "CREATE"] {
        assert!(serde_json::from_value::<Item>(json!({"kind":"schedule","op":op,"schedule_id":"s1"})).is_err());
    }
    for field in ["condition", "command", "notify", "workflow_id"] {
        let mut extra = create_schedule();
        extra[field] = json!("x");
        assert!(serde_json::from_value::<Item>(extra).is_err());
    }
}

#[test]
fn admin_ai_schedule_content_bounds() {
    for base in [create_schedule(), json!({"kind":"schedule","op":"update","schedule_id":"s1"})] {
        for (field, max) in [("name", 80), ("task", 4000)] {
            for n in [1, max] {
                let mut value = base.clone();
                value[field] = json!("字".repeat(n));
                assert!(validate_item(&serde_json::from_value(value).unwrap(), &bounds()).is_ok());
            }
            for n in [0, max + 1] {
                let mut value = base.clone();
                value[field] = json!("字".repeat(n));
                assert!(validate_item(&serde_json::from_value(value).unwrap(), &bounds()).is_err());
            }
        }
        for (field, bad) in [("cron", "invalid"), ("cron", "* * * * *"), ("timezone", "bad"), ("name", "bad\u{202e}")] {
            let mut value = base.clone();
            value[field] = json!(bad);
            assert!(validate_item(&serde_json::from_value(value).unwrap(), &bounds()).is_err());
        }
        for field in ["name", "cron", "timezone", "task"] {
            let mut value = base.clone();
            value[field] = json!(1);
            assert!(serde_json::from_value::<Item>(value).is_err());
        }
    }
    for op in ["update", "pause", "resume", "delete"] {
        for id in ["", " ", "s\u{200b}1"] {
            let item = serde_json::from_value(json!({"kind":"schedule","op":op,"schedule_id":id})).unwrap();
            assert!(validate_item(&item, &bounds()).is_err());
        }
    }
}

#[test]
fn admin_ai_agent_ids_and_skill_names() {
    for id in ["", "../a", "a/b", "dashboard", "cron", "default", "__internal", "a b"] {
        let items = [
            Item::AgentField { agent_id: id.into(), field: AdminAiField::Icon, value: json!("🐾") },
            Item::Skill { agent_id: id.into(), skill: "report".into(), enabled: true },
            Item::Schedule(Schedule::Create { agent_id: id.into(), name: "n".into(), cron: "0 9 * * *".into(), timezone: None, task: "t".into() }),
        ];
        for item in items { assert_eq!(validate_item(&item, &bounds()).unwrap_err().code, "invalid_agent_id"); }
    }
    for skill in ["a".to_owned(), "日報".to_owned(), "a".repeat(200)] {
        for enabled in [true, false] {
            assert!(validate_item(&Item::Skill { agent_id: "alice".into(), skill: skill.clone(), enabled }, &bounds()).is_ok());
        }
    }
    for skill in ["".to_owned(), "a".repeat(201), "..".into(), ".hidden".into(), "a/b".into(), "a\\b".into(), " a".into(), "a ".into(), "a\0b".into()] {
        assert_eq!(validate_item(&Item::Skill { agent_id: "alice".into(), skill, enabled: true }, &bounds()).unwrap_err().code, "invalid_skill_name");
    }
}

#[test]
fn admin_ai_proposal_300_items_100_agents_200_cjk_chars() {
    let item = field_item(AdminAiField::Icon, json!("🐾"));
    let mut proposal = Proposal { items: vec![item.clone(); MAX_ITEMS], reason: "中".repeat(200) };
    assert!(proposal.validate(&bounds()).is_ok());
    assert!(serde_json::from_value::<Proposal>(serde_json::to_value(&proposal).unwrap()).is_ok());
    proposal.items.push(item.clone());
    assert_eq!(proposal.validate_limits().unwrap_err().code, "item_limit");
    assert!(serde_json::from_value::<Proposal>(serde_json::to_value(&proposal).unwrap()).is_err());
    proposal.items = vec![item];
    proposal.reason.push('文');
    assert_eq!(proposal.validate_limits().unwrap_err().code, "reason_limit");
    assert!(serde_json::from_value::<Proposal>(serde_json::to_value(&proposal).unwrap()).is_err());
    proposal.reason.clear();
    assert!(proposal.validate_limits().is_ok());
    proposal.items.clear();
    assert_eq!(proposal.validate_limits().unwrap_err().code, "item_limit");
    for i in 0..100 {
        proposal.items.push(Item::Skill { agent_id: format!("agent-{i}"), skill: "report".into(), enabled: true });
    }
    assert!(proposal.validate_limits().is_ok());
    proposal.items.push(proposal.items[0].clone());
    assert!(proposal.validate_limits().is_ok(), "count distinct agents, not items");
    proposal.items.push(Item::Skill { agent_id: "another".into(), skill: "report".into(), enabled: true });
    assert_eq!(proposal.validate_limits().unwrap_err().code, "agent_limit");
    assert!(serde_json::from_value::<Proposal>(serde_json::to_value(&proposal).unwrap()).is_err());
    assert!(serde_json::from_value::<Proposal>(json!({"items":[], "reason":""})).is_err());
    assert!(serde_json::from_value::<Proposal>(json!({"items":[{"kind":"skill","agent_id":"alice","skill":"a","enabled":true}], "reason":"", "extra":true})).is_err());
}

#[test]
fn admin_ai_resolved_schedule_owners_count_towards_limit() {
    let mut proposal = Proposal { items: Vec::new(), reason: String::new() };
    let mut owners = BTreeMap::new();
    for i in 0..100 {
        let schedule_id = format!("schedule-{i}");
        proposal.items.push(Item::Schedule(Schedule::Pause { schedule_id: schedule_id.clone() }));
        owners.insert(schedule_id, format!("agent-{i}"));
    }
    assert!(proposal.validate_resolved_agents(&owners).is_ok());
    assert_eq!(proposal.validate_resolved_agents(&BTreeMap::new()).unwrap_err().code, "unresolved_schedule");
    proposal.items.push(field_item(AdminAiField::Icon, json!("🐾")));
    assert_eq!(proposal.validate_resolved_agents(&owners).unwrap_err().code, "agent_limit");
    owners.insert("schedule-0".into(), "alice".into());
    assert!(proposal.validate_resolved_agents(&owners).is_ok());
    owners.insert("schedule-0".into(), "cron".into());
    assert_eq!(proposal.validate_resolved_agents(&owners).unwrap_err().code, "invalid_agent_id");
}
