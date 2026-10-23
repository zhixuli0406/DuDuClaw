//! Which Google identity an AI employee uses (per-employee Google identity,
//! phase B2a: domain-wide delegation).
//!
//! `agent.toml` carries only a non-secret selector:
//!
//! ```toml
//! [google]
//! identity = "delegate:alice@customer.com"   # home | none | delegate:<email>
//! fallback = "none"                          # none (default) | home
//! requesters = "bound_people"                # bound_people (default) | owner_only | anyone
//! ```
//!
//! and `config.toml` the home-wide defaults:
//!
//! ```toml
//! [integrations.google_identity]
//! default_identity = "home"                  # home (default) | none
//! allowed_subject_domains = ["customer.com"] # delegate:<email> only in these domains
//! ```
//!
//! This module is the single parse point, shared by the MCP server (which
//! decides "home path in-process" vs "ask the gateway") and the gateway
//! (which decides again from its own read of the same files). Every parse
//! failure is reported as [`Effective::Invalid`]: a selector that cannot be
//! read refuses Google for that employee and never falls back to the home
//! account. `owner` and `user:<id>` resolve through the gateway's personal
//! credential vault and require the selected person's explicit consent.

use std::path::Path;

#[path = "google_identity/tools.rs"]
mod tools;
pub use tools::{NATIVE_GOOGLE_TOOLS, NativeGoogleScope, native_google_scope};
#[path = "google_identity/credentials.rs"]
mod credentials;
pub use credentials::{CredentialKind, CredentialRef};

/// The `agent.toml` table.
pub const AGENT_SECTION: &str = "google";
/// The `config.toml [integrations]` sub-table.
pub const HOME_SECTION: &str = "google_identity";
/// Longest accepted email (RFC 5321 path limit).
const MAX_EMAIL_LEN: usize = 254;

/// The value of `[google] identity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// The home-wide Google account (today's behaviour).
    Home,
    /// No Google for this employee.
    None,
    /// Act as this Workspace user through the delegation service account.
    /// Always lower-case.
    Delegate(String),
    /// The employee's owner's own connected account (B2b).
    Owner,
    /// A named colleague's connected account (B2b).
    User(String),
}

impl Selector {
    /// Parse a selector value. Case-insensitive keywords; surrounding
    /// whitespace ignored; anything else is an error naming the problem.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let v = raw.trim();
        if v.chars().any(char::is_control) {
            return Err("identity 含有控制字元".into());
        }
        let lower = v.to_ascii_lowercase();
        match lower.as_str() {
            "home" => return Ok(Self::Home),
            "none" => return Ok(Self::None),
            "owner" => return Ok(Self::Owner),
            _ => {}
        }
        if let Some(email) = strip_prefix_ci(v, "delegate:") {
            return normalize_email(email).map(Self::Delegate);
        }
        if let Some(user) = strip_prefix_ci(v, "user:") {
            let user = user.trim();
            let ok = !user.is_empty()
                && user.len() <= 128
                && user
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            return if ok {
                Ok(Self::User(user.to_string()))
            } else {
                Err("user: 後面必須是帳號代號".into())
            };
        }
        Err(format!(
            "不認得的 identity 值「{}」（可用：home、none、delegate:<email>）",
            crate::truncate_chars(v, 80)
        ))
    }

    /// The canonical config value (what a write puts in `agent.toml`).
    pub fn to_config_value(&self) -> String {
        match self {
            Self::Home => "home".into(),
            Self::None => "none".into(),
            Self::Delegate(e) => format!("delegate:{e}"),
            Self::Owner => "owner".into(),
            Self::User(u) => format!("user:{u}"),
        }
    }

    /// Closed kind name used in audit rows and the dashboard.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::None => "none",
            Self::Delegate(_) => "delegate",
            Self::Owner => "owner",
            Self::User(_) => "user",
        }
    }
}

fn strip_prefix_ci<'a>(v: &'a str, prefix: &str) -> Option<&'a str> {
    // `get` returns None off a char boundary, so no raw byte slicing.
    let head = v.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        v.get(prefix.len()..)
    } else {
        None
    }
}

/// Validate and lower-case a Workspace user email. A shape check (catches a
/// project id or a bare name pasted by mistake); Google is the authority on
/// whether the account exists.
pub fn normalize_email(raw: &str) -> Result<String, String> {
    let e = raw.trim().to_ascii_lowercase();
    if e.is_empty()
        || e.len() > MAX_EMAIL_LEN
        || e.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("email 格式不正確".into());
    }
    let mut parts = e.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err("email 格式不正確".into());
    };
    if local.is_empty() || !is_domain(domain) {
        return Err("email 格式不正確".into());
    }
    Ok(e)
}

/// `customer.com`-shaped: lower-case letters, digits, `-`, at least one dot,
/// no empty label.
pub fn is_domain(d: &str) -> bool {
    d.len() <= 253
        && d.contains('.')
        && d.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

/// The domain part of a normalized email.
pub fn email_domain(email: &str) -> Option<&str> {
    email.rsplit_once('@').map(|(_, d)| d)
}

/// `[google] fallback`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fallback {
    #[default]
    None,
    Home,
}

impl Fallback {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Home => "home",
        }
    }
}

/// `[google] requesters`: who may, through this employee, use the person's
/// account. Ordered widest to narrowest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Requesters {
    Anyone,
    #[default]
    BoundPeople,
    OwnerOnly,
}

impl Requesters {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anyone => "anyone",
            Self::BoundPeople => "bound_people",
            Self::OwnerOnly => "owner_only",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "anyone" => Ok(Self::Anyone),
            "bound_people" => Ok(Self::BoundPeople),
            "owner_only" => Ok(Self::OwnerOnly),
            _ => Err("requesters 只能是 bound_people、owner_only 或 anyone".into()),
        }
    }
}

/// A parsed `[google]` table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentGoogleSection {
    /// `None` when the table exists but has no `identity` key (the home
    /// default applies).
    pub identity: Option<Selector>,
    pub fallback: Fallback,
    pub requesters: Requesters,
}

/// Result of reading an `agent.toml` for its `[google]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionRead {
    Absent,
    Present(AgentGoogleSection),
    /// Unreadable: the employee's Google tools are refused.
    Invalid(String),
}

impl Default for SectionRead {
    fn default() -> Self {
        Self::Absent
    }
}

impl<'de> serde::Deserialize<'de> for SectionRead {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <toml::Value as serde::Deserialize>::deserialize(deserializer)?;
        Ok(match value.as_table() {
            Some(section) => match section_from_table(section) {
                Ok(section) => Self::Present(section),
                Err(error) => Self::Invalid(error),
            },
            None => Self::Invalid("[google] 必須是表格".into()),
        })
    }
}

const AGENT_KEYS: &[&str] = &["identity", "fallback", "requesters"];

/// Read `[google]` out of an `agent.toml` body. Strict: a file that is not
/// TOML, a non-table `google`, an unknown key or a wrong-typed value is
/// `Invalid` (a typo must not silently become the home account).
pub fn parse_agent_section(text: &str) -> SectionRead {
    match toml::from_str::<crate::agent_toml::AgentTomlSections>(text) {
        Ok(sections) => sections.google,
        Err(_) => return SectionRead::Invalid("agent.toml 不是有效的 TOML".into()),
    }
}

/// Validate one `[google]` table (also used by the dashboard write path).
pub fn section_from_table(section: &toml::Table) -> Result<AgentGoogleSection, String> {
    if let Some(unknown) = section.keys().find(|k| !AGENT_KEYS.contains(&k.as_str())) {
        return Err(format!(
            "[google] 不認得的欄位「{}」",
            crate::truncate_chars(unknown, 40)
        ));
    }
    let str_field = |key: &str| -> Result<Option<&str>, String> {
        match section.get(key) {
            None => Ok(None),
            Some(toml::Value::String(s)) => Ok(Some(s.as_str())),
            Some(_) => Err(format!("[google] {key} 必須是字串")),
        }
    };
    let identity = str_field("identity")?.map(Selector::parse).transpose()?;
    let fallback = match str_field("fallback")?.map(|s| s.trim().to_ascii_lowercase()) {
        None => Fallback::None,
        Some(v) if v == "none" => Fallback::None,
        Some(v) if v == "home" => Fallback::Home,
        Some(_) => return Err("[google] fallback 只能是 none 或 home".into()),
    };
    let requesters = str_field("requesters")?
        .map(Requesters::parse)
        .transpose()?
        .unwrap_or_default();
    Ok(AgentGoogleSection {
        identity,
        fallback,
        requesters,
    })
}

/// `config.toml [integrations.google_identity]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeIdentityConfig {
    /// `Home` or `None` only.
    pub default_identity: Selector,
    /// Lower-case domains a `delegate:` email may be in. Empty ⇒ only the
    /// home service account subject's domain (when one is configured).
    pub allowed_subject_domains: Vec<String>,
    /// Domains accepted for a person's own OAuth connection; empty allows
    /// any verified Google account, including a consumer Gmail account.
    pub allowed_domains: Vec<String>,
}

impl Default for HomeIdentityConfig {
    fn default() -> Self {
        Self {
            default_identity: Selector::Home,
            allowed_subject_domains: Vec::new(),
            allowed_domains: Vec::new(),
        }
    }
}

const HOME_KEYS: &[&str] = &["default_identity", "allowed_subject_domains", "allowed_domains"];

/// Parse the home section out of a `config.toml` body. Absent ⇒ defaults
/// (today's behaviour); present but wrong ⇒ error.
pub fn parse_home_config(text: &str) -> Result<HomeIdentityConfig, String> {
    let table = text
        .parse::<toml::Table>()
        .map_err(|_| "config.toml 不是有效的 TOML".to_string())?;
    let Some(value) = table
        .get("integrations")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get(HOME_SECTION))
    else {
        return Ok(HomeIdentityConfig::default());
    };
    let section = value
        .as_table()
        .ok_or("[integrations.google_identity] 必須是表格")?;
    if let Some(unknown) = section.keys().find(|k| !HOME_KEYS.contains(&k.as_str())) {
        return Err(format!(
            "[integrations.google_identity] 不認得的欄位「{}」",
            crate::truncate_chars(unknown, 40)
        ));
    }
    let default_identity = match section.get("default_identity") {
        None => Selector::Home,
        Some(toml::Value::String(s)) => match Selector::parse(s)? {
            sel @ (Selector::Home | Selector::None) => sel,
            _ => return Err("default_identity 只能是 home 或 none".into()),
        },
        Some(_) => return Err("default_identity 必須是字串".into()),
    };
    let allowed_subject_domains = match section.get("allowed_subject_domains") {
        None => Vec::new(),
        Some(toml::Value::Array(items)) => items
            .iter()
            .map(|v| {
                let d = v
                    .as_str()
                    .ok_or("allowed_subject_domains 必須是字串陣列")?
                    .trim()
                    .to_ascii_lowercase();
                if is_domain(&d) {
                    Ok(d)
                } else {
                    Err(format!("「{}」不是網域", crate::truncate_chars(&d, 80)))
                }
            })
            .collect::<Result<Vec<_>, String>>()?,
        Some(_) => return Err("allowed_subject_domains 必須是字串陣列".into()),
    };
    Ok(HomeIdentityConfig {
        default_identity,
        allowed_subject_domains,
        allowed_domains: match section.get("allowed_domains") {
            None => Vec::new(),
            Some(toml::Value::Array(values)) => values.iter().map(|value| {
                let domain = value.as_str().ok_or("allowed_domains 必須是字串陣列")?.trim().to_ascii_lowercase();
                if is_domain(&domain) { Ok(domain) } else { Err("allowed_domains 含有無效網域".to_string()) }
            }).collect::<Result<Vec<_>, _>>()?,
            Some(_) => return Err("allowed_domains 必須是字串陣列".into()),
        },
    })
}

/// [`parse_home_config`] over `<home>/config.toml`. A missing file is the
/// defaults; an unreadable one is an error (fail closed).
pub fn read_home_config(home: &Path) -> Result<HomeIdentityConfig, String> {
    match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => parse_home_config(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HomeIdentityConfig::default()),
        Err(_) => Err("無法讀取 config.toml".into()),
    }
}

/// Is `email` in a domain a delegate may use? Exact domain equality only
/// (no subdomain match). `home_subject` is the home service account's
/// `subject`, used when no list is configured.
pub fn delegate_domain_allowed(
    email: &str,
    cfg: &HomeIdentityConfig,
    home_subject: Option<&str>,
) -> bool {
    let Some(domain) = email_domain(email) else {
        return false;
    };
    if !cfg.allowed_subject_domains.is_empty() {
        return cfg.allowed_subject_domains.iter().any(|d| d == domain);
    }
    home_subject
        .and_then(|s| normalize_email(s).ok())
        .is_some_and(|s| email_domain(&s) == Some(domain))
}

/// The identity that takes effect for one caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effective {
    /// The home-wide account, in-process (unchanged path).
    Home,
    /// Google is off for this employee: tools hidden and refused.
    Disabled,
    /// Act as this Workspace user; only the gateway mints it.
    Delegate { email: String },
    /// A human's own connected Google account. The gateway enforces the
    /// Enterprise, active Owner, self-consent and per-call scope gates.
    Personal { selector: Selector },
    /// The configuration cannot be read: refused, never home.
    Invalid(String),
}

impl Effective {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Disabled => "none",
            Self::Delegate { .. } => "delegate",
            Self::Personal { selector } => selector.kind(),
            Self::Invalid(_) => "invalid",
        }
    }

    /// Whether the Google tools are listed (discoverable ⇔ callable: only an
    /// effective `none` hides them; a broken setting stays listed so the call
    /// can say what is wrong).
    pub fn tools_listed(&self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

/// The resolved setting of one employee (or of an anonymous caller).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveIdentity {
    pub effective: Effective,
    pub fallback: Fallback,
    pub requesters: Requesters,
    /// The employee's `agent.toml` names an identity (vs the home default).
    pub explicit: bool,
}

impl EffectiveIdentity {
    fn plain(effective: Effective) -> Self {
        Self {
            effective,
            fallback: Fallback::None,
            requesters: Requesters::default(),
            explicit: false,
        }
    }
}

fn effective_of(sel: &Selector) -> Effective {
    match sel {
        Selector::Home => Effective::Home,
        Selector::None => Effective::Disabled,
        Selector::Delegate(email) => Effective::Delegate {
            email: email.clone(),
        },
        Selector::Owner | Selector::User(_) => Effective::Personal { selector: sel.clone() },
    }
}

/// Combine an employee's `[google]` read with the home config. Pure.
pub fn combine(
    section: &SectionRead,
    home: &Result<HomeIdentityConfig, String>,
) -> EffectiveIdentity {
    let home = match home {
        Ok(h) => h,
        Err(e) => return EffectiveIdentity::plain(Effective::Invalid(e.clone())),
    };
    match section {
        SectionRead::Invalid(e) => EffectiveIdentity::plain(Effective::Invalid(e.clone())),
        SectionRead::Absent => EffectiveIdentity::plain(effective_of(&home.default_identity)),
        SectionRead::Present(s) => EffectiveIdentity {
            effective: effective_of(s.identity.as_ref().unwrap_or(&home.default_identity)),
            fallback: s.fallback,
            requesters: s.requesters,
            explicit: s.identity.is_some(),
        },
    }
}

/// Read `<home>/agents/<agent>/agent.toml`'s `[google]`. A missing file is
/// `Absent` (the home default); an unreadable one is `Invalid`.
pub fn read_agent_section(home: &Path, agent: &str) -> SectionRead {
    if !crate::is_valid_agent_id(agent) {
        return SectionRead::Invalid("員工代號不正確".into());
    }
    match crate::agent_toml::read_effective_text(&home.join("agents").join(agent)) {
        Ok(text) => parse_agent_section(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SectionRead::Absent,
        Err(_) => SectionRead::Invalid("無法讀取 agent.toml".into()),
    }
}

/// The identity for `agent` (`None` = an anonymous caller, which only ever
/// gets the home default).
pub fn effective_for(home: &Path, agent: Option<&str>) -> EffectiveIdentity {
    let home_cfg = read_home_config(home);
    match agent {
        Some(a) => combine(&read_agent_section(home, a), &home_cfg),
        None => combine(&SectionRead::Absent, &home_cfg),
    }
}

#[cfg(test)]
#[path = "google_identity_tests.rs"]
mod tests;
