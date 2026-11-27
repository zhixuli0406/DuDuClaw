//! Closed event-field vocabulary for automation skill parameters.

pub struct FieldContext<'a> {
    pub cep_then_event: Option<&'a str>,
    pub tick_fields: &'a [String],
}

const TASK_FIELDS: &[&str] = &[
    "task.id", "task.title", "task.description", "task.status", "task.priority",
    "task.assigned_to", "task.created_by", "task.deadline_at", "task.tags",
];

/// The same data drives validation and the future dashboard field picker.
const EVENT_FIELDS: &[(&str, &[&str])] = &[
    ("task_created", TASK_FIELDS),
    ("task_updated", TASK_FIELDS),
    ("task_status_changed", &[
        "task.id", "task.title", "task.description", "task.status", "task.priority",
        "task.assigned_to", "task.created_by", "task.deadline_at", "task.tags",
        "task_id", "from", "to",
    ]),
    ("channel_message", &["channel", "agent_id", "text"]),
    ("agent_idle", &["agent_id", "idle_minutes"]),
    ("run_at_risk", &["agent_id", "level", "score"]),
    ("os_file", &["agent_id", "file_name", "extension", "kind"]),
    ("os_frontmost", &["app", "window_title"]),
    ("tick", &["source", "ts"]),
    ("odoo_event", &["event_type", "model", "record_id", "record.*"]),
    ("mcp_event", &["name", "server", "event_id", "data.*"]),
    ("security_event", &["severity", "event_type", "agent_id", "source"]),
    ("activity_new", &["activity.event_type", "activity.agent_id", "activity.summary"]),
];

pub(crate) fn valid_segment(segment: &str) -> bool {
    (1..=64).contains(&segment.len())
        && segment.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn effective_event<'a>(event: &'a str, ctx: &'a FieldContext<'_>) -> &'a str {
    if event == "cep_trigger" {
        // Do not recurse: a sequence cannot authorize another CEP trigger.
        ctx.cep_then_event.unwrap_or("")
    } else {
        event
    }
}

pub fn placeholder_allowed(trigger_event: &str, path: &[&str], ctx: &FieldContext<'_>) -> bool {
    if !(1..=4).contains(&path.len()) || !path.iter().all(|s| valid_segment(s)) {
        return false;
    }
    let event = effective_event(trigger_event, ctx);
    let Some((_, fixed)) = EVENT_FIELDS.iter().find(|(name, _)| *name == event) else {
        return false;
    };
    fixed.iter().any(|entry| entry.split('.').eq(path.iter().copied()))
        || matches!((event, path), ("odoo_event", ["record", _]) | ("mcp_event", ["data", _]))
        || (event == "tick" && path.len() == 1 && ctx.tick_fields.iter().any(|f| f == path[0]))
}

pub fn placeholders_for(trigger_event: &str, ctx: &FieldContext<'_>) -> Vec<String> {
    let event = effective_event(trigger_event, ctx);
    let Some((_, fixed)) = EVENT_FIELDS.iter().find(|(name, _)| *name == event) else {
        return Vec::new();
    };
    let mut fields: Vec<String> = fixed.iter().map(|s| (*s).to_string()).collect();
    if event == "tick" {
        for field in ctx.tick_fields {
            if valid_segment(field) && !fields.contains(field) {
                fields.push(field.clone());
            }
        }
    }
    fields
}

pub(crate) fn scalar_only(trigger_event: &str, path: &[&str], ctx: &FieldContext<'_>) -> bool {
    matches!(
        (effective_event(trigger_event, ctx), path),
        ("odoo_event", ["record", _]) | ("mcp_event", ["data", _])
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_table_is_closed_and_exact() {
        let ctx = FieldContext { cep_then_event: None, tick_fields: &[] };
        for (event, entries) in EVENT_FIELDS {
            assert_eq!(placeholders_for(event, &ctx), entries.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            for entry in *entries {
                let concrete = entry.replace('*', "name");
                assert!(placeholder_allowed(event, &concrete.split('.').collect::<Vec<_>>(), &ctx));
            }
        }
        for event in ["future_event", "cron_tick", "channel_message_extra", "cep_trigger"] {
            assert!(placeholders_for(event, &ctx).is_empty());
            assert!(!placeholder_allowed(event, &["text"], &ctx));
        }
        for (event, path) in [
            ("task_created", "task.lease_expires_at"), ("task_updated", "task"),
            ("odoo_event", "record.partner.name"), ("mcp_event", "data.key.nested"),
            ("odoo_event", "record.Bad"), ("os_frontmost", "agent_id"),
        ] {
            assert!(!placeholder_allowed(event, &path.split('.').collect::<Vec<_>>(), &ctx));
        }
    }

    #[test]
    fn cep_and_tick_contexts_only_narrow_the_vocabulary() {
        let declared = vec!["price".into(), "Bad".into(), "nested.key".into(), "source".into()];
        let ctx = FieldContext { cep_then_event: Some("tick"), tick_fields: &declared };
        assert_eq!(placeholders_for("cep_trigger", &ctx), vec!["source", "ts", "price"]);
        assert!(placeholder_allowed("cep_trigger", &["price"], &ctx));
        assert!(!placeholder_allowed("tick", &["pct_price"], &ctx));
        assert!(!placeholder_allowed("tick", &["nested", "key"], &ctx));
        assert!(!placeholder_allowed("channel_message", &["price"], &ctx));
        let recursive = FieldContext { cep_then_event: Some("cep_trigger"), tick_fields: &declared };
        assert!(placeholders_for("cep_trigger", &recursive).is_empty());
        let odoo = FieldContext { cep_then_event: Some("odoo_event"), tick_fields: &[] };
        assert!(scalar_only("cep_trigger", &["record", "name"], &odoo));
    }
}
