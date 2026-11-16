//! Shared bounded per-run step policy for CLI and gateway callers.
use std::{path::Path, sync::Once};

pub const DEFAULT_MAX_TURNS: u64 = 50;
pub const DEFAULT_MAX_TURNS_LIMIT: u64 = 200;
static WARNING: Once = Once::new();
pub fn warn_invalid() {
    WARNING.call_once(|| tracing::warn!("invalid or unreadable runtime step limit; using conservative step limits (or clamping to ceiling)"));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnPolicy { pub default: u64, pub limit: u64, degraded: bool }
impl Default for TurnPolicy {
    fn default() -> Self { Self { default: DEFAULT_MAX_TURNS, limit: DEFAULT_MAX_TURNS_LIMIT, degraded: false } }
}
impl TurnPolicy {
    /// Strict validation for dashboard writes; execution uses bounded recovery.
    pub fn validate(table: &toml::Table) -> Result<Self, String> {
        let mut policy = Self::read(table)?;
        if policy.default > policy.limit {
            if table.get("runtime").and_then(|r| r.get("default_max_turns")).is_some() {
                return Err("default_max_turns must not exceed max_turns_limit".into());
            }
            policy.default = policy.limit;
        }
        Ok(policy)
    }
    fn read(table: &toml::Table) -> Result<Self, String> {
        let Some(runtime) = table.get("runtime") else { return Ok(Self::default()); };
        let runtime = runtime.as_table().ok_or("[runtime] must be a table")?;
        let number = |key: &str, fallback| -> Result<u64, String> {
            runtime.get(key).map(|v| v.as_integer().and_then(|n| u64::try_from(n).ok())
                .filter(|n| (1..=10000).contains(n))
                .ok_or_else(|| format!("runtime.{key} must be an integer between 1 and 10000")))
                .unwrap_or(Ok(fallback))
        };
        Ok(Self { default: number("default_max_turns", DEFAULT_MAX_TURNS)?, limit: number("max_turns_limit", DEFAULT_MAX_TURNS_LIMIT)?, degraded: false })
    }
    pub fn from_table(table: &toml::Table) -> Self {
        let runtime = table.get("runtime").and_then(toml::Value::as_table);
        let invalid = std::cell::Cell::new(table.contains_key("runtime") && runtime.is_none());
        let number = |key: &str, fallback, max| {
            match runtime.and_then(|r| r.get(key)) {
                None => fallback,
                Some(v) => v.as_integer().and_then(|n| u64::try_from(n).ok())
                    .filter(|n| (1..=max).contains(n)).unwrap_or_else(|| { invalid.set(true); fallback }),
            }
        };
        let mut policy = Self { default: number("default_max_turns", DEFAULT_MAX_TURNS, i64::MAX as u64),
            limit: number("max_turns_limit", DEFAULT_MAX_TURNS_LIMIT, 10000), degraded: invalid.get() };
        if policy.degraded { warn_invalid(); }
        if policy.default > policy.limit { warn_invalid(); policy.default = policy.limit; }
        policy
    }
    /// Authorization cannot recover from an unreadable or invalid policy.
    pub fn load_checked(home: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(home.join("config.toml")) {
            Ok(text) => Self::validate(&text.parse::<toml::Table>().map_err(|e| e.to_string())?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("cannot read runtime step policy: {e}")),
        }
    }
    fn conservative() -> Self {
        warn_invalid();
        Self { degraded: true, ..Self::default() }
    }
    pub fn load(home: &Path) -> Self {
        match std::fs::read_to_string(home.join("config.toml")) {
            Ok(text) => text.parse::<toml::Table>().map(|t| Self::from_table(&t))
                .unwrap_or_else(|_| Self::conservative()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(_) => Self::conservative()
        }
    }
    pub fn resolve(self, raw: Option<i64>) -> u64 {
        let requested = match raw {
            Some(n) if n > 0 => n as u64,
            Some(_) => { warn_invalid(); self.default.min(DEFAULT_MAX_TURNS) },
            None => self.default,
        };
        if requested > self.limit { warn_invalid(); }
        requested.min(self.limit).min(if self.degraded { DEFAULT_MAX_TURNS } else { u64::MAX })
    }
}

fn checked_agent_max_turns(dir: &Path) -> Result<Option<i64>, ()> {
    let text = match crate::agent_toml::read_effective_text(dir) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let table = text.parse::<toml::Table>().map_err(|_| ())?;
    let Some(runtime) = table.get("runtime") else { return Ok(None) };
    let runtime = runtime.as_table().ok_or(())?;
    match runtime.get("max_turns") {
        None => Ok(None),
        Some(value) => value.as_integer().filter(|n| *n > 0).map(Some).ok_or(()),
    }
}
pub fn raw_agent_max_turns(dir: &Path) -> Option<i64> {
    checked_agent_max_turns(dir).unwrap_or_else(|_| { warn_invalid(); None })
}
fn resolve_runtime_turns(home: &Path, agent_dir: Option<&Path>, tool_loop: bool) -> u64 {
    let policy = TurnPolicy::load(home);
    let raw = agent_dir.map(checked_agent_max_turns).unwrap_or(Ok(None));
    let degraded = policy.degraded || raw.is_err();
    if degraded { warn_invalid(); }
    let raw = raw.ok().flatten();
    let resolved = if tool_loop && raw.is_none() { 10 } else { policy.resolve(raw) };
    if degraded { resolved.min(policy.limit).min(if tool_loop { 10 } else { DEFAULT_MAX_TURNS }) } else { resolved }
}
pub fn max_turns(home: &Path, agent_dir: Option<&Path>) -> u64 {
    resolve_runtime_turns(home, agent_dir, false)
}
pub fn tool_loop_max_turns(home: &Path, agent_dir: Option<&Path>) -> usize {
    resolve_runtime_turns(home, agent_dir, true) as usize
}
/// Static per-agent hint appended without moving existing cache split markers.
pub fn budget_prompt(prompt: &str, turns: u64) -> String {
    format!("{prompt}\nStep budget for this turn: {turns}; when close to this limit, summarise your progress and stop cleanly.")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn precedence_defaults_invalid_and_clamping() {
        let p = TurnPolicy::from_table(&"[runtime]\ndefault_max_turns=80\nmax_turns_limit=100".parse().unwrap());
        assert_eq!(p.resolve(None), 80);
        assert_eq!(p.resolve(Some(25)), 25);
        assert_eq!(p.resolve(Some(10001)), 100);
        assert_eq!(p.resolve(Some(0)), 50);
        assert_eq!(p.resolve(Some(-1)), 50);
        let p = TurnPolicy::from_table(&"[runtime]\ndefault_max_turns=80\nmax_turns_limit=20".parse().unwrap());
        assert_eq!(p.resolve(None), 20);
        assert!(TurnPolicy::validate(&"[runtime]\nmax_turns_limit=0".parse().unwrap()).is_err());
        assert_eq!(TurnPolicy::default().resolve(None), 50);
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;
    #[test]
    fn employee_override_and_api_legacy_default_follow_live_files() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/anna");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(home.path().join("config.toml"), "[runtime]\ndefault_max_turns=70\nmax_turns_limit=90\n").unwrap();
        std::fs::write(dir.join("agent.toml"), "[runtime]\nprovider='claude'\n").unwrap();
        assert_eq!(max_turns(home.path(), Some(&dir)), 70);
        assert_eq!(tool_loop_max_turns(home.path(), Some(&dir)), 10);
        for (literal, expected) in [("80", 80), ("1000", 90), ("0", 50), ("-1", 50), ("'oops'", 50)] {
            std::fs::write(dir.join("agent.toml"), format!("[runtime]\nmax_turns={literal}\n")).unwrap();
            assert_eq!(max_turns(home.path(), Some(&dir)), expected);
        }
        std::fs::write(dir.join("agent.toml"), "[runtime]\nmax_turns=81\n").unwrap();
        assert_eq!(tool_loop_max_turns(home.path(), Some(&dir)), 81);
        std::fs::write(home.path().join("config.toml"), "[broken").unwrap();
        assert!(TurnPolicy::load_checked(home.path()).is_err());
        assert_eq!(max_turns(home.path(), Some(&dir)), 50);
        assert_eq!(tool_loop_max_turns(home.path(), Some(&dir)), 10);
        std::fs::write(dir.join("agent.toml"), "[broken").unwrap();
        for (default, expected) in [(100, 50), (10, 10)] {
            std::fs::write(home.path().join("config.toml"), format!("[runtime]\ndefault_max_turns={default}\n")).unwrap();
            assert_eq!(max_turns(home.path(), Some(&dir)), expected);
            assert_eq!(tool_loop_max_turns(home.path(), Some(&dir)), 10);
        }
    }
    #[test]
    fn missing_files_use_defaults_but_read_and_type_errors_are_conservative() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/anna");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(max_turns(home.path(), Some(&dir)), 50);
        std::fs::write(home.path().join("config.toml"), "[runtime]\ndefault_max_turns=100\n").unwrap();
        assert_eq!(max_turns(home.path(), Some(&dir)), 100);
        for text in ["runtime=false", "[runtime]\nmax_turns='oops'\n"] {
            std::fs::write(dir.join("agent.toml"), text).unwrap();
            assert_eq!(max_turns(home.path(), Some(&dir)), 50);
            assert_eq!(tool_loop_max_turns(home.path(), Some(&dir)), 10);
        }
        std::fs::remove_file(dir.join("agent.toml")).unwrap();
        std::fs::create_dir(dir.join("agent.toml")).unwrap();
        assert_eq!(max_turns(home.path(), Some(&dir)), 50);
        std::fs::remove_file(home.path().join("config.toml")).unwrap();
        std::fs::create_dir(home.path().join("config.toml")).unwrap();
        assert_eq!(max_turns(home.path(), None), 50);
        assert_eq!(tool_loop_max_turns(home.path(), None), 10);
        assert!(TurnPolicy::load_checked(home.path()).is_err());
        let p = TurnPolicy::from_table(&"[runtime]\ndefault_max_turns=100\nmax_turns_limit=false".parse().unwrap());
        assert_eq!(p.resolve(None), 50);
        assert_eq!(p.resolve(Some(25)), 25);
    }
    #[test]
    fn malformed_default_does_not_discard_a_valid_lower_ceiling() {
        let p = TurnPolicy::from_table(&"[runtime]\ndefault_max_turns=false\nmax_turns_limit=3".parse().unwrap());
        assert_eq!(p.resolve(None), 3);
        let p = TurnPolicy::from_table(&"[runtime]\ndefault_max_turns=99999\nmax_turns_limit=20".parse().unwrap());
        assert_eq!(p.resolve(None), 20);
    }
    #[test]
    fn budget_hint_preserves_cache_prefix_and_split_marker() {
        let prefix = "shared\n<CACHE_SPLIT_MARKER>\nagent";
        let prompt = budget_prompt(prefix, 75);
        assert!(prompt.starts_with(prefix));
        assert!(prompt.ends_with("Step budget for this turn: 75; when close to this limit, summarise your progress and stop cleanly."));
    }
}
