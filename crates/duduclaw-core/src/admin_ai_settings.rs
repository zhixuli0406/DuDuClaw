//! Stateless admission for administrator AI settings proposals.
//!
//! This is not authorization. The gateway must still resolve schedule owners,
//! check live target/skill/model eligibility, name collisions, rename safety,
//! prompt screening, and the final changed leaves before applying a proposal.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use cron::TimeUnitSpec;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub const MAX_ITEMS: usize = 300;
pub const MAX_AGENTS: usize = 100;
pub const MAX_REASON_CHARS: usize = 200;

/// User-facing explanations, in the order of SPEC section 6, item 9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DashboardOnly {
    pub label: &'static str,
    pub reason: &'static str,
}

pub const DASHBOARD_ONLY: &[DashboardOnly] = &[
    DashboardOnly { label: "工具與資料存取權限", reason: "決定 AI 能做哪些事，須由管理員在儀表板設定。" },
    DashboardOnly { label: "員工操作權限與可用通道", reason: "這些限制保護委派、建立員工、排程及技能操作，AI 不能自行解除。" },
    DashboardOnly { label: "沙箱、網路隔離與檔案防護", reason: "影響執行隔離與檔案保護，須在儀表板確認。" },
    DashboardOnly { label: "執行引擎、備援引擎與背景服務", reason: "切換引擎會改變工具及隔離方式，也可能將對話送到另一家服務。" },
    DashboardOnly { label: "模型帳號與預算放寬", reason: "涉及費用與付款限制，須由管理員在儀表板決定。" },
    DashboardOnly { label: "服務身分、通道憑證與公開呼叫", reason: "涉及代表同事存取服務，以及對外開放的範圍。" },
    DashboardOnly { label: "主管、部門、員工識別碼與主要角色", reason: "影響組織與委派權限，須經組織異動流程。" },
    DashboardOnly { label: "委派範圍、外部代理連線與身分驗證", reason: "AI 不能自行放寬委派範圍或解除身分驗證。" },
    DashboardOnly { label: "外部工具伺服器與連線設定", reason: "員工不能自行修改外部工具或執行程式的連線設定。" },
    DashboardOnly { label: "員工人格與工作合約", reason: "這類變更已有只能在儀表板確認的核准流程。" },
    DashboardOnly { label: "封存與刪除員工", reason: "可能無法復原，須由管理員在儀表板處理。" },
    DashboardOnly { label: "從外部安裝新技能", reason: "外部程式有供應鏈風險；管理員 AI 只能啟用或停用已安裝的技能。" },
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdminAiRefusal {
    pub code: &'static str,
    pub message: String,
}

impl AdminAiRefusal {
    fn new(code: &'static str, label: &str, detail: &str) -> Self {
        Self { code, message: format!("{label}：{detail}") }
    }

    pub fn unknown_field() -> Self {
        let list = DASHBOARD_ONLY.iter()
            .map(|entry| format!("{}（{}）", entry.label, entry.reason))
            .collect::<Vec<_>>().join("；");
        Self::new("unknown_field", "設定項目", &format!("不在管理員 AI 可修改的清單中。以下設定請到儀表板處理：{list}"))
    }
}

impl fmt::Display for AdminAiRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.message) }
}
impl std::error::Error for AdminAiRefusal {}

macro_rules! fields {
    ($( $variant:ident => ($name:literal, $path:literal, $zh:literal, $en:literal, $ja:literal) ),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        pub enum AdminAiField {
            $(#[serde(rename = $name)] $variant),+
        }
        impl AdminAiField {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const fn tool_name(self) -> &'static str {
                match self { $(Self::$variant => $name),+ }
            }
            pub const fn toml_path(self) -> &'static str {
                match self { $(Self::$variant => $path),+ }
            }
            /// Unknown locales use English; these are interface locales, not reply languages.
            pub fn label(self, locale: &str) -> &'static str {
                match locale {
                    "zh-TW" => match self { $(Self::$variant => $zh),+ },
                    "ja" | "ja-JP" => match self { $(Self::$variant => $ja),+ },
                    _ => match self { $(Self::$variant => $en),+ },
                }
            }
            pub fn from_tool_name(name: &str) -> Result<Self, AdminAiRefusal> {
                match name { $($name => Ok(Self::$variant)),+, _ => Err(AdminAiRefusal::unknown_field()) }
            }
        }
    }
}

fields! {
    DisplayName => ("display_name", "agent.display_name", "顯示名稱", "Display name", "表示名"),
    Icon => ("icon", "agent.icon", "圖示", "Icon", "アイコン"),
    Trigger => ("trigger", "agent.trigger", "呼叫名稱", "Mention name", "呼び出し名"),
    Status => ("status", "agent.status", "狀態", "Status", "状態"),
    ReplyLanguage => ("reply_language", "prompt.reply_language", "回覆語言", "Reply language", "返信言語"),
    Model => ("model", "model.preferred", "主要模型", "Primary model", "メインモデル"),
    FallbackModel => ("fallback_model", "model.fallback", "備援模型", "Fallback model", "予備モデル"),
    Effort => ("effort", "model.effort", "思考強度", "Reasoning effort", "思考の深さ"),
    TimeoutSecs => ("timeout_secs", "runtime.timeout_secs", "單次執行時間上限", "Time limit per run", "1 回の実行時間の上限"),
    MaxTurns => ("max_turns", "runtime.max_turns", "每次最多步數", "Maximum steps per run", "1 回の最大ステップ数"),
    IdleTimeoutSecs => ("idle_timeout_secs", "runtime.idle_timeout_secs", "閒置逾時", "Idle timeout", "無操作時のタイムアウト"),
    HeartbeatEnabled => ("heartbeat_enabled", "heartbeat.enabled", "定時喚醒", "Scheduled wake-up", "定期起動"),
    HeartbeatCron => ("heartbeat_cron", "heartbeat.cron", "定時喚醒時間", "Wake-up schedule", "定期起動の時刻"),
    HeartbeatInterval => ("heartbeat_interval", "heartbeat.interval_seconds", "定時喚醒間隔", "Wake-up interval", "定期起動の間隔"),
    HeartbeatCronTimezone => ("heartbeat_cron_timezone", "heartbeat.cron_timezone", "時區", "Time zone", "タイムゾーン"),
}

impl AdminAiField {
    /// Allowed changed leaves for this field, including the rename-trigger sync.
    /// The gateway must separately validate any derived trigger against live state.
    pub fn paths(self) -> BTreeSet<&'static str> {
        let mut paths = BTreeSet::from([self.toml_path()]);
        if self == Self::DisplayName { paths.insert("agent.trigger"); }
        paths
    }
}

impl<'de> Deserialize<'de> for AdminAiField {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::from_tool_name(&name).map_err(serde::de::Error::custom)
    }
}

/// Nested internal tagging preserves the flat wire shape: kind=schedule, op=… .
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Item {
    AgentField { agent_id: String, field: AdminAiField, value: Value },
    Schedule(Schedule),
    Skill { agent_id: String, skill: String, enabled: bool },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Schedule {
    Create {
        agent_id: String,
        name: String,
        cron: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timezone: Option<String>,
        task: String,
    },
    Update {
        schedule_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cron: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timezone: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
    },
    Pause { schedule_id: String },
    Resume { schedule_id: String },
    Delete { schedule_id: String },
}

impl Item {
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Self::AgentField { agent_id, .. } | Self::Skill { agent_id, .. }
                | Self::Schedule(Schedule::Create { agent_id, .. }) => Some(agent_id),
            _ => None,
        }
    }
    pub fn schedule_id(&self) -> Option<&str> {
        match self {
            Self::Schedule(Schedule::Update { schedule_id, .. }
                | Schedule::Pause { schedule_id } | Schedule::Resume { schedule_id }
                | Schedule::Delete { schedule_id }) => Some(schedule_id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ProposalWire")]
pub struct Proposal {
    pub items: Vec<Item>,
    pub reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposalWire { items: Vec<Item>, reason: String }

impl TryFrom<ProposalWire> for Proposal {
    type Error = AdminAiRefusal;
    fn try_from(wire: ProposalWire) -> Result<Self, Self::Error> {
        let proposal = Self { items: wire.items, reason: wire.reason };
        proposal.validate_limits()?;
        Ok(proposal)
    }
}

impl Proposal {
    /// Counts explicit targets only. Existing schedule owners require resolution.
    pub fn validate_limits(&self) -> Result<(), AdminAiRefusal> {
        if self.items.is_empty() || self.items.len() > MAX_ITEMS {
            return Err(AdminAiRefusal::new("item_limit", "變更項目", "每批須有 1～300 項。"));
        }
        if self.reason.chars().count() > MAX_REASON_CHARS {
            return Err(AdminAiRefusal::new("reason_limit", "變更說明", "最多 200 字。"));
        }
        validate_agent_count(self.items.iter().filter_map(Item::agent_id))
    }

    pub fn validate(&self, bounds: &Bounds) -> Result<Vec<NormalizedItem>, AdminAiRefusal> {
        self.validate_limits()?;
        self.items.iter().map(|item| validate_item(item, bounds)).collect()
    }

    /// Required gateway step before admitting a batch containing existing schedules.
    /// Map keys are schedule IDs and values are their authoritative employee IDs.
    pub fn validate_resolved_agents(&self, schedule_owners: &BTreeMap<String, String>) -> Result<(), AdminAiRefusal> {
        self.validate_limits()?;
        let mut agents = Vec::new();
        for item in &self.items {
            let agent = match (item.agent_id(), item.schedule_id()) {
                (Some(agent), _) => agent,
                (_, Some(id)) => schedule_owners.get(id).map(String::as_str).ok_or_else(||
                    AdminAiRefusal::new("unresolved_schedule", "排程", "無法確認所屬員工，請重新讀取排程。"))?,
                _ => unreachable!("every item has an employee or schedule target"),
            };
            validate_agent(agent)?;
            agents.push(agent);
        }
        validate_agent_count(agents)
    }
}

fn validate_agent_count<'a>(agents: impl IntoIterator<Item = &'a str>) -> Result<(), AdminAiRefusal> {
    if agents.into_iter().collect::<BTreeSet<_>>().len() > MAX_AGENTS {
        Err(AdminAiRefusal::new("agent_limit", "目標員工", "每批最多 100 位。"))
    } else { Ok(()) }
}

/// Supplied from checked global policy; there is deliberately no permissive default.
/// Current gateway idle policy uses min=60, max=timeout_max, allow_zero=true.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub timeout_max: u64,
    pub turn_limit: u64,
    pub idle_timeout_min: u64,
    pub idle_timeout_max: u64,
    pub idle_timeout_allow_zero: bool,
}

/// Passed pure checks only; this must not be treated as a write authorization.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct NormalizedItem(Item);
impl NormalizedItem {
    pub fn as_item(&self) -> &Item { &self.0 }
    pub fn into_item(self) -> Item { self.0 }
}

fn validate_agent(id: &str) -> Result<(), AdminAiRefusal> {
    if !crate::is_valid_agent_id(id) || crate::delegation_policy::is_reserved_agent_id(id)
        || crate::is_internal_agent_dir(id) {
        Err(AdminAiRefusal::new("invalid_agent_id", "目標員工", "員工識別碼無效或為系統保留名稱。"))
    } else { Ok(()) }
}

fn safe_visible(text: &str, label: &str) -> Result<(), AdminAiRefusal> {
    // The existing diagnostic helper uses Rust's Unicode tables, including
    // unassigned code points. Reject changes instead of silently sanitizing.
    if text.chars().any(crate::text_utils::is_unsafe_inline_char)
        || crate::root_key::recovery::escape_diagnostic_text(text) != text {
        Err(AdminAiRefusal::new("unsafe_character", label, "不能含控制、雙向文字、零寬或未指派字元。"))
    } else { Ok(()) }
}

fn length(text: &str, min: usize, max: usize, label: &str) -> Result<(), AdminAiRefusal> {
    if !(min..=max).contains(&text.chars().count()) {
        Err(AdminAiRefusal::new("invalid_length", label, &format!("須為 {min}～{max} 字。")))
    } else { Ok(()) }
}

fn validate_cron(text: &str, label: &str) -> Result<(), AdminAiRefusal> {
    let error = || AdminAiRefusal::new("invalid_cron", label, "須為有效的排程，最多 256 位元組，間隔至少 15 分鐘。");
    if text.len() > 256 { return Err(error()); }
    let schedule = crate::cron_tz::normalise_cron(text).parse::<cron::Schedule>().map_err(|_| error())?;
    // Same conservative clock-slot proof as gateway cron_store::scoped::validate_cadence.
    let mut first = None;
    let mut previous = None;
    for hour in schedule.hours().iter() {
        for minute in schedule.minutes().iter() {
            for second in schedule.seconds().iter() {
                let slot = hour * 3600 + minute * 60 + second;
                if previous.is_some_and(|p| slot - p < 900) { return Err(error()); }
                first.get_or_insert(slot);
                previous = Some(slot);
            }
        }
    }
    match (first, previous) {
        (Some(first), Some(last)) if 86400 + first - last >= 900 => Ok(()),
        _ => Err(error()),
    }
}

fn validate_timezone(text: &str, label: &str) -> Result<(), AdminAiRefusal> {
    if crate::cron_tz::parse_timezone(text).is_none() {
        Err(AdminAiRefusal::new("invalid_timezone", label, "請使用有效的時區名稱，例如 Asia/Taipei。"))
    } else { Ok(()) }
}

fn validate_field(field: AdminAiField, value: &Value, bounds: &Bounds) -> Result<Value, AdminAiRefusal> {
    use AdminAiField::*;
    let label = field.label("zh-TW");
    let wrong_type = || AdminAiRefusal::new("wrong_type", label, "輸入的資料類型不正確。");
    let invalid = || AdminAiRefusal::new("invalid_value", label, "不接受這個值，請使用允許的選項。");
    if field == HeartbeatEnabled {
        return value.as_bool().map(Value::from).ok_or_else(wrong_type);
    }
    if matches!(field, TimeoutSecs | MaxTurns | IdleTimeoutSecs | HeartbeatInterval) {
        if !value.is_i64() && !value.is_u64() { return Err(wrong_type()); }
        let n = value.as_u64().filter(|n| *n <= i64::MAX as u64).ok_or_else(invalid)?;
        let allowed = match field {
            TimeoutSecs => (30..=bounds.timeout_max).contains(&n),
            MaxTurns => (1..=bounds.turn_limit).contains(&n),
            IdleTimeoutSecs => (n == 0 && bounds.idle_timeout_allow_zero)
                || (n > 0 && (bounds.idle_timeout_min..=bounds.idle_timeout_max).contains(&n)),
            HeartbeatInterval => (300..=86400).contains(&n),
            _ => unreachable!(),
        };
        return if allowed { Ok(Value::from(n)) } else {
            Err(AdminAiRefusal::new("out_of_bounds", label, "超出允許範圍。"))
        };
    }
    let text = value.as_str().ok_or_else(wrong_type)?;
    match field {
        DisplayName => {
            safe_visible(text, label)?;
            crate::agent_rename::validate_display_name(text).map_err(|_|
                AdminAiRefusal::new("invalid_display_name", label, "須為 1～64 字的純文字，不能含標記符號。"))?;
            return Ok(Value::from(text.trim()));
        }
        Icon => { safe_visible(text, label)?; length(text, 1, 8, label)?; }
        Trigger => {
            safe_visible(text, label)?;
            // The task contract counts the 2–32 character name after '@'.
            let name = text.strip_prefix('@').ok_or_else(invalid)?;
            length(name, 2, 32, label)?;
            if text.chars().any(char::is_whitespace) { return Err(invalid()); }
        }
        Status if !["active", "paused"].contains(&text) => return Err(invalid()),
        ReplyLanguage if !text.is_empty() && !crate::profile_sections::is_valid_reply_language(text) => return Err(invalid()),
        Model if text.trim().is_empty() => return Err(invalid()),
        Effort if !["", "low", "medium", "high", "xhigh", "max"].contains(&text) => return Err(invalid()),
        HeartbeatCron => validate_cron(text, label)?,
        HeartbeatCronTimezone => {
            validate_timezone(text, label)?;
            return Ok(Value::from(text.trim()));
        }
        _ => {}
    }
    // Keep cron in Unix notation: normalise_cron is not idempotent for weekdays.
    Ok(value.clone())
}

pub fn validate_item(item: &Item, bounds: &Bounds) -> Result<NormalizedItem, AdminAiRefusal> {
    if let Some(agent) = item.agent_id() { validate_agent(agent)?; }
    if let Some(id) = item.schedule_id() {
        if id.trim().is_empty() { return Err(AdminAiRefusal::new("invalid_schedule_id", "排程", "請指定排程識別碼。")); }
        safe_visible(id, "排程")?;
    }
    let mut normalized = item.clone();
    match &mut normalized {
        Item::AgentField { field, value, .. } => *value = validate_field(*field, value, bounds)?,
        Item::Skill { skill, .. } => {
            // Mirrors duduclaw-agent::skill_state::validate_skill_name, unavailable
            // as a core dependency. Installed-skill membership remains a live check.
            if skill.trim().is_empty() || skill.trim() != skill || skill.chars().count() > 200
                || skill.contains(['/', '\\', '\0']) || skill.starts_with('.') {
                return Err(AdminAiRefusal::new("invalid_skill_name", "技能名稱", "須為 1～200 字，不能含路徑、前後空白或以句點開頭。"));
            }
        }
        Item::Schedule(schedule) => {
            let (name, cron, timezone, task) = match schedule {
                Schedule::Create { name, cron, timezone, task, .. } =>
                    (Some(name), Some(cron), timezone.as_mut(), Some(task)),
                Schedule::Update { name, cron, timezone, task, .. } =>
                    (name.as_mut(), cron.as_mut(), timezone.as_mut(), task.as_mut()),
                _ => (None, None, None, None),
            };
            if let Some(name) = name { length(name, 1, 80, "排程名稱")?; safe_visible(name, "排程名稱")?; }
            if let Some(cron) = cron { validate_cron(cron, "排程時間")?; }
            if let Some(timezone) = timezone {
                validate_timezone(timezone, "時區")?;
                *timezone = timezone.trim().to_owned();
            }
            if let Some(task) = task { length(task, 1, 4000, "排程提示")?; }
        }
    }
    Ok(NormalizedItem(normalized))
}

#[cfg(test)]
#[path = "admin_ai_settings_tests.rs"]
mod tests;

/// Only an explicit TOML boolean true enables administrator AI settings.
pub fn enabled_from_table(table: &toml::Table) -> bool {
    table.get("admin_ai").and_then(toml::Value::as_table)
        .and_then(|section| section.get("enabled")).and_then(toml::Value::as_bool) == Some(true)
}

pub fn enabled_from_home(home: &std::path::Path) -> bool {
    std::fs::read_to_string(home.join("config.toml")).ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .is_some_and(|table| enabled_from_table(&table))
}

/// Compare actual leaves; arrays are atomic values and empty tables have no leaves.
pub fn changed_leaves(before: &toml::Table, after: &toml::Table) -> Vec<(String, Value, Value)> {
    fn flatten(table: &toml::Table, prefix: &str, out: &mut BTreeMap<String, Value>) {
        for (key, value) in table {
            let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
            if let Some(child) = value.as_table() { flatten(child, &path, out); }
            else { out.insert(path, serde_json::to_value(value).unwrap_or(Value::Null)); }
        }
    }
    let (mut b, mut a) = (BTreeMap::new(), BTreeMap::new());
    flatten(before, "", &mut b);
    flatten(after, "", &mut a);
    b.keys().chain(a.keys()).collect::<BTreeSet<_>>().into_iter().filter_map(|path| {
        let before = b.get(path).cloned().unwrap_or(Value::Null);
        let after = a.get(path).cloned().unwrap_or(Value::Null);
        (before != after).then(|| (path.clone(), before, after))
    }).collect()
}

pub const DIFF_REFUSED: &str = "admin_ai_diff_refused";

/// Validate the final artifact, including any automatic runtime alignment.
pub fn validate_changed_leaves(before: &toml::Table, after: &toml::Table, fields: &[AdminAiField]) -> Result<(), &'static str> {
    let allowed: BTreeSet<_> = fields.iter().flat_map(|field| field.paths()).collect();
    if changed_leaves(before, after).iter().any(|(path, _, _)| !allowed.contains(path.as_str())) {
        return Err(DIFF_REFUSED);
    }
    Ok(())
}

#[cfg(test)]
mod setting_tests {
    use super::*;

    #[test]
    fn admin_ai_enabled_is_strict_and_defaults_off() {
        for text in ["", "admin_ai=true", "[admin_ai]", "[admin_ai]\nenabled=false", "[admin_ai]\nenabled='true'", "[admin_ai]\nenabled=1"] {
            assert!(!enabled_from_table(&text.parse().unwrap()), "{text}");
        }
        assert!(enabled_from_table(&"[admin_ai]\nenabled=true".parse().unwrap()));
        let home = tempfile::tempdir().unwrap();
        assert!(!enabled_from_home(home.path()));
        std::fs::write(home.path().join("config.toml"), "[broken").unwrap();
        assert!(!enabled_from_home(home.path()));
    }

    #[test]
    fn admin_ai_diff_checks_requested_leaves_and_engine_alignment() {
        let before: toml::Table = "[model]\npreferred='sonnet'\n[runtime]\nprovider='claude'".parse().unwrap();
        let after: toml::Table = "[model]\npreferred='gpt-5.4'\n[runtime]\nprovider='codex'".parse().unwrap();
        assert_eq!(validate_changed_leaves(&before, &after, &[AdminAiField::Model]), Err(DIFF_REFUSED));
        let after: toml::Table = "[model]\npreferred='opus'\n[runtime]\nprovider='claude'".parse().unwrap();
        assert!(validate_changed_leaves(&before, &after, &[AdminAiField::Model]).is_ok());
        assert!(validate_changed_leaves(&before, &after, &[AdminAiField::Icon]).is_err());
        let before = "[agent]\ndisplay_name='A'\ntrigger='@A'".parse().unwrap();
        let after = "[agent]\ndisplay_name='B'\ntrigger='@B'\n[budget]".parse().unwrap();
        assert!(validate_changed_leaves(&before, &after, &[AdminAiField::DisplayName]).is_ok());
    }
}
