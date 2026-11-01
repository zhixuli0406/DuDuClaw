//! Recipient policy uses host-provided identities, never process environment.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Off,
    Shadow,
    Ask,
}

impl Mode {
    pub fn parse(value: &str) -> Self {
        match value {
            "off" => Self::Off,
            "shadow" => Self::Shadow,
            "ask" => Self::Ask,
            _ => {
                tracing::warn!("unknown recipient_guard mode; requiring approval");
                Self::Ask
            }
        }
    }
}

pub fn effective_mode(global: Mode, employee: Option<&str>) -> Mode {
    global.max(employee.map(Mode::parse).unwrap_or(Mode::Off))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    WouldAsk,
    Ask,
}

/// A transport destination must already be canonical; origin aliases are not addresses.
pub fn channel_recipient(channel: &str, chat: &str) -> Option<String> {
    if channel != channel.trim() || chat != chat.trim()
        || chat.chars().any(|ch| ch.is_control() || matches!(ch, '#' | ':'))
    {
        return None;
    }
    let target = format!("{channel}:{chat}");
    (normalize_recipient(&target).as_deref() == Some(target.as_str())).then_some(target)
}

/// Match the origin conversation grammar, including employee buckets and threads.
pub fn normalize_recipient(value: &str) -> Option<String> {
    let value = crate::session_conversation(value.trim());
    let (channel, rest) = value.split_once(':')?;
    let channel = channel.trim();
    if !matches!(channel,
        "telegram" | "slack" | "discord" | "line" | "whatsapp" | "feishu"
        | "googlechat" | "teams" | "wecom" | "dingtalk" | "mail"
    ) {
        return None;
    }
    let recipient = if channel == "mail" {
        rest.trim()
    } else if let Some(thread) = rest.strip_prefix("thread:") {
        thread.trim()
    } else {
        rest.split(':').next()?.trim()
    };
    if recipient.is_empty() || recipient.chars().any(|ch| ch.is_control() || ch == '#') {
        return None;
    }
    if channel == "mail" {
        let (local, domain) = recipient.split_once('@')?;
        if local.is_empty() || domain.is_empty()
            || recipient.chars().any(|ch| ch.is_whitespace() || matches!(ch, ',' | ';' | '<' | '>' | ':'))
            || domain.chars().any(|ch| ch == '@')
        {
            return None;
        }
    }
    Some(format!("{channel}:{recipient}"))
}

pub fn evaluate(
    mode: Mode,
    targets: &[String],
    source: Option<&str>,
    owners: &[String],
    configured: &[String],
) -> Decision {
    if mode == Mode::Off {
        return Decision::Allow;
    }
    let allowed: Vec<String> = source.into_iter()
        .chain(owners.iter().map(String::as_str))
        .chain(configured.iter().map(String::as_str))
        .filter_map(normalize_recipient)
        .collect();
    let permitted = targets.iter().all(|target| {
        normalize_recipient(target).is_some_and(|target| allowed.iter().any(|item| item == &target))
    });
    if permitted {
        Decision::Allow
    } else if mode == Mode::Shadow {
        Decision::WouldAsk
    } else {
        Decision::Ask
    }
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub mode: Mode,
    pub recipients: Vec<String>,
    pub valid: bool,
}

impl Policy {
    pub fn evaluate(&self, targets: &[String], source: Option<&str>, owners: &[String]) -> Decision {
        if !self.valid {
            return Decision::Ask;
        }
        evaluate(self.mode, targets, source, owners, &self.recipients)
    }
}

fn section(path: &Path, name: &str) -> Result<toml::Table, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(toml::Table::new()),
        Err(error) => return Err(error.to_string()),
    };
    let document = text.parse::<toml::Table>().map_err(|error| error.to_string())?;
    match document.get(name) {
        None => Ok(toml::Table::new()),
        Some(value) => value.as_table().cloned().ok_or_else(|| format!("{name} is not a table")),
    }
}

fn mode(table: &toml::Table, absent: Mode) -> Result<Mode, String> {
    match table.get("recipient_guard") {
        None => Ok(absent),
        Some(value) => value.as_str().map(Mode::parse)
            .ok_or_else(|| "recipient_guard is not a string".into()),
    }
}

/// Resolved employee configuration wins over its unmerged agent.toml.
pub fn load(home: &Path, agent_dir: Option<&Path>) -> Policy {
    let loaded = (|| -> Result<Policy, String> {
        let global = mode(&section(&home.join("config.toml"), "security")?, Mode::Shadow)?;
        let employee = if let Some(dir) = agent_dir {
            let resolved = crate::preset::agent_home_dir(dir).and_then(|home| {
                dir.file_name().and_then(|id| id.to_str())
                    .map(|id| crate::preset::agent_resolved_path(&home, id))
            });
            let path = resolved.filter(|path| match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                _ => true,
            }).unwrap_or_else(|| dir.join("agent.toml"));
            section(&path, "capabilities")?
        } else {
            toml::Table::new()
        };
        let employee_mode = mode(&employee, Mode::Off)?;
        let recipients = match employee.get("recipients") {
            None => Vec::new(),
            Some(value) => value.as_array().ok_or("recipients is not an array")?.iter()
                .map(|value| value.as_str().and_then(normalize_recipient)
                    .ok_or_else(|| "invalid recipient entry".to_string()))
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(Policy { mode: global.max(employee_mode), recipients, valid: true })
    })();
    loaded.unwrap_or_else(|reason| {
        tracing::warn!(%reason, "recipient guard config unreadable; requiring approval");
        Policy { mode: Mode::Ask, recipients: Vec::new(), valid: false }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn decision_table_and_shadow_never_blocks() {
        for mode in [Mode::Off, Mode::Shadow, Mode::Ask] {
            for target in ["line:source", "line:owner", "line:configured", "line:stranger", "invalid"] {
                let allowed = matches!(target, "line:source" | "line:owner" | "line:configured");
                let decision = evaluate(mode, &strings(&[target]), Some("line:source"),
                    &strings(&["line:owner"]), &strings(&["line:configured"]));
                assert_eq!(decision, if allowed || mode == Mode::Off { Decision::Allow }
                    else if mode == Mode::Shadow { Decision::WouldAsk } else { Decision::Ask });
            }
        }
        assert_eq!(evaluate(Mode::Ask, &[], None, &[], &[]), Decision::Allow);
        assert_eq!(evaluate(Mode::Ask, &strings(&["line:owner", "line:stranger"]), None,
            &strings(&["line:owner"]), &[]), Decision::Ask);
    }

    #[test]
    fn employee_modes_only_narrow() {
        for global in [Mode::Off, Mode::Shadow, Mode::Ask] {
            for employee in [None, Some("off"), Some("shadow"), Some("ask"), Some("typo")] {
                assert!(effective_mode(global, employee) >= global);
            }
        }
        assert_eq!(effective_mode(Mode::Off, None), Mode::Off);
        assert_eq!(Mode::parse("typo"), Mode::Ask);
    }

    #[test]
    fn normalized_conversations_are_exact() {
        for (input, expected) in [
            ("telegram:-100:9#agent:alice", "telegram:-100"),
            ("discord:thread:123#agent:alice", "discord:123"),
            (" line:U123 ", "line:U123"),
            ("googlechat:spaces/room", "googlechat:spaces/room"),
            ("mail:ops@example.com", "mail:ops@example.com"),
        ] { assert_eq!(normalize_recipient(input).as_deref(), Some(expected)); }
        for invalid in ["", "line:", "webchat:u#agent:a", "line:U#other", "mail:a@b,c@d", "line:U\nX"] {
            assert_eq!(normalize_recipient(invalid), None, "{invalid}");
        }
        assert_eq!(evaluate(Mode::Ask, &strings(&["line:U1234"]), Some("line:U123"), &[], &[]), Decision::Ask);
        assert_eq!(evaluate(Mode::Ask, &strings(&["line:U123"]), None, &[], &[]), Decision::Ask);
    }

    #[test]
    fn raw_transport_targets_never_inherit_origin_aliases() {
        for (channel, chat) in [
            ("line", "U123:other"),
            ("line", "U123#agent:evil"),
            ("discord", "thread:123"),
            ("telegram", "-100:9"),
            ("line", " U123"),
            ("line ", "U123"),
            ("line", "U123\nother"),
        ] {
            assert_eq!(channel_recipient(channel, chat), None, "{channel}:{chat}");
        }
        assert_eq!(channel_recipient("telegram", "-100"), Some("telegram:-100".into()));
        assert_eq!(channel_recipient("googlechat", "spaces/room"), Some("googlechat:spaces/room".into()));
        assert_eq!(normalize_recipient("telegram:-100:9#agent:alice"), Some("telegram:-100".into()));
    }

    #[test]
    fn config_defaults_resolved_narrowing_and_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let dir = home.join("agents/alice");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load(home, Some(&dir)).mode, Mode::Shadow);
        std::fs::write(home.join("config.toml"), "[security]\nrecipient_guard='ask'\n").unwrap();
        std::fs::write(dir.join("agent.toml"), "[capabilities]\nrecipient_guard='off'\nrecipients=['line:U123']\n").unwrap();
        let policy = load(home, Some(&dir));
        assert_eq!(policy.mode, Mode::Ask);
        assert_eq!(policy.recipients, strings(&["line:U123"]));
        let resolved = crate::preset::agent_resolved_path(home, "alice");
        std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
        std::fs::write(&resolved, "[capabilities]\nrecipients=['mail:ops@example.com']\n").unwrap();
        assert_eq!(load(home, Some(&dir)).recipients, strings(&["mail:ops@example.com"]));
        for bad in ["[", "capabilities=1", "[capabilities]\nrecipient_guard=1", "[capabilities]\nrecipients=[1]"] {
            std::fs::write(&resolved, bad).unwrap();
            let policy = load(home, Some(&dir));
            assert!(!policy.valid);
            assert_eq!(policy.evaluate(&strings(&["line:source"]), Some("line:source"), &[]), Decision::Ask);
        }
    }

    #[test]
    fn proactive_targets_do_not_enter_the_allowlist() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("agents/alice");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[capabilities]\nrecipient_guard='ask'\n[proactive]\nnotify_channel='line'\nnotify_chat_id='U123'\n").unwrap();
        let policy = load(temp.path(), Some(&dir));
        assert_eq!(policy.evaluate(&strings(&["line:U123"]), None, &[]), Decision::Ask);
    }

    #[test]
    fn unreadable_config_requires_approval() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("config.toml")).unwrap();
        let policy = load(temp.path(), None);
        assert!(!policy.valid);
        assert_eq!(policy.mode, Mode::Ask);
    }

    #[test]
    fn invalid_capability_values_survive_round_trip() {
        let caps: crate::types::CapabilitiesConfig = toml::from_str(
            "recipient_guard=1\nrecipients=['line:U123', 5]",
        ).unwrap();
        let encoded = toml::to_string(&caps).unwrap();
        let decoded: crate::types::CapabilitiesConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(caps.recipient_guard, decoded.recipient_guard);
        assert_eq!(caps.recipients, decoded.recipients);
    }
}
