//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

#[test]
fn trigger_event_known_values_pass() {
    for ev in [
        "task_created",
        "task_updated",
        "task_status_changed",
        "activity_new",
        "channel_message",
        "agent_idle",
        "cron_tick",
        "run_at_risk",
        "os_file",
        "os_frontmost",
    ] {
        assert!(
            validate_autopilot_trigger_event(ev).is_ok(),
            "should accept {ev}"
        );
    }
}

#[test]
fn trigger_event_accepts_foresight_run_at_risk() {
    // 2026-07 MED regression: `run_at_risk` is emitted by the engine
    // (`Event::RunAtRisk.event_name()`) but was rejected at rule-write
    // time, so no rule could ever subscribe to the foresight event.
    assert!(validate_autopilot_trigger_event("run_at_risk").is_ok());
}

#[test]
fn trigger_event_accepts_os_native_events() {
    // P1 leftover gap fixed alongside P3-4: `os_file` (P1) / `os_frontmost`
    // (P2-4) have been emitted by the engine since their respective work
    // packages, but a dashboard-authored rule could never subscribe to
    // either — same class of bug as the `run_at_risk` regression above.
    assert!(validate_autopilot_trigger_event("os_file").is_ok());
    assert!(validate_autopilot_trigger_event("os_frontmost").is_ok());
}

#[test]
fn trigger_event_accepts_resident_sensing_tick() {
    // WP2: without this a `[[tick.sources]]` feed could be configured but
    // never subscribed to from the dashboard.
    assert!(validate_autopilot_trigger_event("tick").is_ok());
    // The engine's own spelling is the contract — keep them in lockstep.
    assert_eq!(
        crate::autopilot_engine::AutopilotEvent::Tick {
            source: "s1".into(),
            ts: "2026-08-11T09:00:00+00:00".into(),
            fields: serde_json::Value::Null,
        }
        .event_name(),
        "tick"
    );
}

#[test]
fn trigger_event_accepts_odoo_events() {
    // G4: without this entry the Odoo bridge could publish onto the bus
    // while no dashboard-authored rule could ever subscribe to it.
    assert!(validate_autopilot_trigger_event("odoo_event").is_ok());
    // The engine's own spelling is the contract — keep them in lockstep.
    assert_eq!(
        crate::autopilot_engine::AutopilotEvent::OdooEvent {
            event_type: "odoo.crm.lead_created".into(),
            model: "crm.lead".into(),
            record_id: 1,
            record: serde_json::Value::Null,
        }
        .event_name(),
        "odoo_event"
    );
    // ...and it is a legal `first`/`then` step for a CEP sequence rule
    // ("order confirmed, then no payment within 7 days").
    assert!(crate::cep_matcher::KNOWN_EVENT_NAMES.contains(&"odoo_event"));
}

#[test]
fn create_refuses_cron_tick_but_update_keeps_accepting_it() {
    // Nothing emits `cron_tick`; a new rule on it would never fire.
    let err = validate_autopilot_trigger_event_for_create("cron_tick").unwrap_err();
    assert!(err.contains("never emitted"), "{err}");
    assert!(err.contains("scheduled task"), "{err}");
    // Stored rules keep loading and updating.
    assert!(validate_autopilot_trigger_event("cron_tick").is_ok());
    for ev in AUTOPILOT_CREATE_TRIGGER_EVENTS {
        assert!(validate_autopilot_trigger_event_for_create(ev).is_ok(), "{ev}");
    }
    assert!(validate_autopilot_trigger_event_for_create("randomEvent").is_err());
    // The unknown-event message no longer advertises the dead name.
    let unknown = validate_autopilot_trigger_event("randomEvent").unwrap_err();
    assert!(!unknown.contains("cron_tick"), "{unknown}");
}

#[test]
fn conditions_validator_accepts_no_conditions_and_rejects_malformed_leaves() {
    for ok in [
        Value::Null,
        json!({}),
        json!({ "all": [] }),
        json!({ "any": [] }),
        json!({ "field": "agent_id", "value": "a" }),
        json!({ "field": "agent_id", "op": "eq", "value": "a" }),
        json!({ "all": [ { "field": "task.priority", "op": "in", "value": ["high"] },
                         { "any": [ { "field": "text", "op": "contains", "value": "x" } ] } ] }),
        json!({ "all": [ {} ] }),
    ] {
        assert!(validate_autopilot_conditions(&ok).is_ok(), "{ok} must be accepted");
    }
    for bad in [
        json!({ "op": "eq", "value": "a" }),
        json!({ "field": "", "op": "eq" }),
        json!({ "field": "  " }),
        json!({ "field": 3 }),
        json!({ "field": "a", "op": "like" }),
        json!({ "field": "a", "op": 1 }),
        json!({ "all": {} }),
        json!({ "any": "x" }),
        json!({ "all": [ { "value": 1 } ] }),
        json!({ "any": [ { "field": "a" }, { "op": "eq" } ] }),
        json!("always"),
        json!([]),
        json!(1),
    ] {
        let err = validate_autopilot_conditions(&bad).expect_err(&format!("{bad} must be refused"));
        assert!(err.starts_with("conditions"), "message must name the path: {err}");
    }
    // Pathologically deep nesting is refused, not recursed without bound.
    let mut deep = json!({ "field": "a" });
    for _ in 0..40 {
        deep = json!({ "all": [deep] });
    }
    assert!(validate_autopilot_conditions(&deep).is_err());
}

/// The dashboard mirrors the server's trigger list by hand
/// (`web/src/lib/autopilot-rules.ts` `SERVER_TRIGGER_EVENTS`). Every name a new
/// rule may use must be offered there, and the dashboard may list nothing the
/// server does not accept (a legacy update-only name is tolerated).
#[test]
fn dashboard_trigger_list_mirrors_server() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../web/src/lib/autopilot-rules.ts");
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "SKIP dashboard_trigger_list_mirrors_server: {} absent (source tree without web/)",
                path.display()
            );
            return;
        }
        Err(e) => panic!("read {}: {e}", path.display()),
    };
    let ts = parse_ts_string_array(&src, "SERVER_TRIGGER_EVENTS")
        .unwrap_or_else(|| panic!("SERVER_TRIGGER_EVENTS array not found in {}", path.display()));
    let ts: std::collections::BTreeSet<String> = ts.into_iter().collect();
    let create: std::collections::BTreeSet<String> =
        AUTOPILOT_CREATE_TRIGGER_EVENTS.iter().map(|s| s.to_string()).collect();
    let legacy: std::collections::BTreeSet<String> =
        AUTOPILOT_LEGACY_TRIGGER_EVENTS.iter().map(|s| s.to_string()).collect();

    let missing: Vec<_> = create.difference(&ts).collect();
    assert!(missing.is_empty(), "server create triggers missing from the dashboard list: {missing:?}");
    let extra: Vec<_> = ts.iter().filter(|t| !create.contains(*t) && !legacy.contains(*t)).collect();
    assert!(extra.is_empty(), "dashboard lists triggers the server refuses: {extra:?}");
}

/// Extract the string literals of `<name> = [ ... ]` from TypeScript source,
/// tolerant of quotes style, trailing commas, line/block comments and layout.
fn parse_ts_string_array(src: &str, name: &str) -> Option<Vec<String>> {
    let decl = src.find(&format!("{name} ="))?;
    let rest = &src[decl..];
    let open = rest.find('[')?;
    let body = &rest[open + 1..];
    let mut out = Vec::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ']' => return Some(out),
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            '\'' | '"' | '`' => {
                let mut lit = String::new();
                for n in chars.by_ref() {
                    if n == c {
                        break;
                    }
                    lit.push(n);
                }
                out.push(lit);
            }
            _ => {}
        }
    }
    None
}

#[test]
fn ts_array_parser_handles_layout_and_comments() {
    let src = "export const X = [\n  'a', // note ']'\n  \"b\" /* c */,\n] as const;";
    assert_eq!(parse_ts_string_array(src, "X").unwrap(), vec!["a", "b"]);
    assert_eq!(parse_ts_string_array("const X = ['a','b']", "X").unwrap(), vec!["a", "b"]);
    assert!(parse_ts_string_array("const Y = []", "X").is_none());
}

#[test]
fn trigger_event_rejects_typos() {
    assert!(validate_autopilot_trigger_event("task.created").is_err());
    assert!(validate_autopilot_trigger_event("").is_err());
    assert!(validate_autopilot_trigger_event("randomEvent").is_err());
}

#[test]
fn action_delegate_requires_target_and_prompt() {
    let ok = json!({ "type": "delegate", "target_agent": "bruno", "prompt": "go" });
    assert!(validate_autopilot_action(&ok).is_ok());

    let missing_target = json!({ "type": "delegate", "prompt": "go" });
    assert!(validate_autopilot_action(&missing_target).is_err());

    let missing_prompt = json!({ "type": "delegate", "target_agent": "bruno" });
    assert!(validate_autopilot_action(&missing_prompt).is_err());
}

#[test]
fn action_notify_requires_channel_chat_text() {
    let ok = json!({ "type": "notify", "channel": "telegram", "chat_id": "1", "text": "hi" });
    assert!(validate_autopilot_action(&ok).is_ok());

    let missing = json!({ "type": "notify", "channel": "telegram" });
    assert!(validate_autopilot_action(&missing).is_err());
}

#[test]
fn action_run_skill_requires_target_and_skill() {
    let ok = json!({ "type": "run_skill", "target_agent": "bruno", "skill_name": "audit" });
    assert!(validate_autopilot_action(&ok).is_ok());

    let missing = json!({ "type": "run_skill", "target_agent": "bruno" });
    assert!(validate_autopilot_action(&missing).is_err());
}

#[test]
fn action_rejects_unknown_type() {
    let bad = json!({ "type": "self_destruct" });
    assert!(validate_autopilot_action(&bad).is_err());
}

#[test]
fn action_rejects_non_object() {
    assert!(validate_autopilot_action(&Value::Null).is_err());
    assert!(validate_autopilot_action(&json!("delegate")).is_err());
}

// ── WP3: `action.screen` structural validation ────────────────

fn delegate_with_screen(screen: Value) -> Value {
    json!({
        "type": "delegate",
        "target_agent": "trader",
        "prompt": "p",
        "screen": screen,
    })
}

#[test]
fn action_accepts_a_well_formed_screen() {
    assert!(
        validate_autopilot_action(&delegate_with_screen(json!({
            "mode": "local",
            "prompt": "只有真的異常才回 YES",
            "on_unavailable": "drop",
            "timeout_secs": 15,
        })))
        .is_ok()
    );
    // Minimal form (defaults fill the rest).
    assert!(
        validate_autopilot_action(&delegate_with_screen(
            json!({ "mode": "local", "prompt": "p" })
        ))
        .is_ok()
    );
    // Absent / explicit null keeps every pre-WP3 rule valid.
    assert!(
        validate_autopilot_action(&json!({
            "type": "delegate", "target_agent": "t", "prompt": "p"
        }))
        .is_ok()
    );
    assert!(validate_autopilot_action(&delegate_with_screen(Value::Null)).is_ok());
}

#[test]
fn action_rejects_a_malformed_screen_at_write_time() {
    // Unknown mode (including a near-miss that a substring check would
    // have let through), missing/empty prompt, over-long prompt, unknown
    // policy, out-of-range timeout, non-object.
    let over_long = "x".repeat(crate::autopilot_screen::MAX_SCREEN_RULE_PROMPT_BYTES + 1);
    for bad in [
        json!({ "mode": "cloud", "prompt": "p" }),
        json!({ "mode": "local2", "prompt": "p" }),
        json!({ "prompt": "p" }),
        json!({ "mode": "local" }),
        json!({ "mode": "local", "prompt": "  " }),
        json!({ "mode": "local", "prompt": over_long }),
        json!({ "mode": "local", "prompt": "p", "on_unavailable": "ignore" }),
        json!({ "mode": "local", "prompt": "p", "timeout_secs": 0 }),
        json!({ "mode": "local", "prompt": "p", "timeout_secs": 600 }),
        json!({ "mode": "local", "prompt": "p", "timeout_secs": "10" }),
        json!("local"),
        json!([]),
    ] {
        assert!(
            validate_autopilot_action(&delegate_with_screen(bad.clone())).is_err(),
            "screen {bad} must be refused"
        );
    }
}

/// `autopilot.update`: `cron_tick` round-trips on a rule that already has it,
/// and changing any other rule to it is refused with the create message.
#[test]
fn update_accepts_cron_tick_only_when_unchanged() {
    assert!(validate_autopilot_trigger_event_for_update("cron_tick", "cron_tick").is_ok());
    let err = validate_autopilot_trigger_event_for_update("cron_tick", "task_created").unwrap_err();
    assert_eq!(err, validate_autopilot_trigger_event_for_create("cron_tick").unwrap_err());
    // Moving off the legacy trigger, and ordinary changes, are unaffected.
    assert!(validate_autopilot_trigger_event_for_update("task_created", "cron_tick").is_ok());
    assert!(validate_autopilot_trigger_event_for_update("tick", "task_created").is_ok());
    assert!(validate_autopilot_trigger_event_for_update("nope", "nope").is_err());
}
