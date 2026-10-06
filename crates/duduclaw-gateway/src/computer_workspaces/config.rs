//! `config.toml [computer_use.workspaces]` (design §8). Parsed strictly: a
//! wrong type, an out-of-range value or an unknown key makes the whole
//! section invalid, which means **off** (never the defaults). Read on every
//! use (hot apply).

use std::path::Path;

/// The parsed section. [`Default`] is the "absent" value: everything off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacesConfig {
    pub enabled: bool,
    pub max_per_agent: u32,
    pub max_bytes: u64,
    pub max_files: u32,
    /// 0 = never expires.
    pub retention_days: u32,
    pub min_free_bytes: u64,
    /// How long an Admin's dashboard approval of a terminal regrant / renew
    /// / delete stays usable, in minutes (review M-6).
    pub admin_approval_minutes: u32,
}

impl Default for WorkspacesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_per_agent: 3,
            max_bytes: 64 * 1024 * 1024,
            max_files: 1000,
            retention_days: 30,
            min_free_bytes: 512 * 1024 * 1024,
            admin_approval_minutes: 30,
        }
    }
}

const KNOWN_KEYS: &[&str] = &[
    "enabled",
    "max_per_agent",
    "max_bytes",
    "max_files",
    "retention_days",
    "min_free_bytes",
    "admin_approval_minutes",
];

const MIB: i64 = 1024 * 1024;

fn int_in(
    table: &toml::Table,
    key: &str,
    range: std::ops::RangeInclusive<i64>,
) -> Result<Option<i64>, String> {
    match table.get(key) {
        None => Ok(None),
        Some(toml::Value::Integer(n)) if range.contains(n) => Ok(Some(*n)),
        Some(_) => Err(format!(
            "[computer_use.workspaces] {key} must be an integer in {}..={}",
            range.start(),
            range.end()
        )),
    }
}

/// Parse a whole `config.toml` table. A missing `[computer_use]` or
/// `[computer_use.workspaces]` is [`WorkspacesConfig::default`] (off).
pub fn parse(config: &toml::Table) -> Result<WorkspacesConfig, String> {
    let Some(cu) = config.get("computer_use") else {
        return Ok(WorkspacesConfig::default());
    };
    let Some(cu) = cu.as_table() else {
        return Err("[computer_use] must be a table".into());
    };
    let table = match cu.get("workspaces") {
        None => return Ok(WorkspacesConfig::default()),
        Some(toml::Value::Table(t)) => t,
        Some(_) => return Err("[computer_use.workspaces] must be a table".into()),
    };
    if let Some(unknown) = table.keys().find(|k| !KNOWN_KEYS.contains(&k.as_str())) {
        return Err(format!(
            "[computer_use.workspaces] has an unknown key `{unknown}`"
        ));
    }
    let mut out = WorkspacesConfig::default();
    match table.get("enabled") {
        None => {}
        Some(toml::Value::Boolean(b)) => out.enabled = *b,
        Some(_) => return Err("[computer_use.workspaces] enabled must be true or false".into()),
    }
    if let Some(n) = int_in(table, "max_per_agent", 1..=20)? {
        out.max_per_agent = n as u32;
    }
    if let Some(n) = int_in(table, "max_bytes", MIB..=1024 * MIB)? {
        out.max_bytes = n as u64;
    }
    if let Some(n) = int_in(table, "max_files", 1..=10_000)? {
        out.max_files = n as u32;
    }
    if let Some(n) = int_in(table, "retention_days", 0..=3650)? {
        out.retention_days = n as u32;
    }
    if let Some(n) = int_in(table, "min_free_bytes", 0..=1024 * 1024 * MIB)? {
        out.min_free_bytes = n as u64;
    }
    if let Some(n) = int_in(table, "admin_approval_minutes", 1..=1440)? {
        out.admin_approval_minutes = n as u32;
    }
    Ok(out)
}

/// Read `<home>/config.toml`. Missing file = off; unreadable / invalid TOML
/// / invalid section = `Err` (callers treat it as off and doctor fails).
pub fn load(home: &Path) -> Result<WorkspacesConfig, String> {
    match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => match text.parse::<toml::Table>() {
            Ok(table) => parse(&table),
            Err(_) => Err("config.toml is not valid TOML".into()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(WorkspacesConfig::default()),
        Err(_) => Err("config.toml could not be read".into()),
    }
}

/// The section when the feature may be used right now: parsed, enabled and
/// on a supported platform. Anything else is `None` (off, fail closed).
pub fn active(home: &Path) -> Option<WorkspacesConfig> {
    if !cfg!(unix) {
        return None;
    }
    load(home).ok().filter(|c| c.enabled)
}

/// The employee's own switch: `[capabilities.computer_use_config] workspace`.
pub fn agent_enabled(home: &Path, agent_id: &str) -> bool {
    let dir = crate::ephemeral::resolve_agent_dir(home, agent_id)
        .unwrap_or_else(|| home.join("agents").join(agent_id));
    let caps = duduclaw_core::agent_toml::load(&dir).capabilities;
    caps.computer_use && caps.computer_use_config.workspace
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Result<WorkspacesConfig, String> {
        parse(&text.parse::<toml::Table>().unwrap())
    }

    #[test]
    fn absent_is_off_and_values_parse() {
        assert_eq!(p("").unwrap(), WorkspacesConfig::default());
        assert!(!p("[computer_use]\nimage = \"x\"\n").unwrap().enabled);
        let c =
            p("[computer_use.workspaces]\nenabled = true\nmax_per_agent = 2\nretention_days = 0\n")
                .unwrap();
        assert!(c.enabled);
        assert_eq!((c.max_per_agent, c.retention_days), (2, 0));
    }

    #[test]
    fn anything_malformed_is_an_error_not_the_default() {
        for bad in [
            "[computer_use.workspaces]\nenabled = \"yes\"\n",
            "[computer_use.workspaces]\nenabled = true\nmax_per_agent = 0\n",
            "[computer_use.workspaces]\nenabled = true\nmax_bytes = 1\n",
            "[computer_use.workspaces]\nenabled = true\nmax_files = 99999\n",
            "[computer_use.workspaces]\nenabled = true\nroot = \"/etc\"\n",
            "[computer_use]\nworkspaces = 1\n",
        ] {
            assert!(p(bad).is_err(), "{bad}");
        }
    }
}
