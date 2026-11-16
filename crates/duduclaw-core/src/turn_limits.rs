//! Shared bounded per-run step policy for CLI and gateway callers.
use std::{path::Path, sync::Once};

pub const DEFAULT_MAX_TURNS: u64 = 50;
pub const DEFAULT_MAX_TURNS_LIMIT: u64 = 200;
static WARNING: Once = Once::new();
pub fn warn_invalid() {
    WARNING.call_once(|| tracing::warn!("invalid or unreadable runtime step limit; using bounded defaults (or clamping to ceiling)"));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnPolicy { pub default: u64, pub limit: u64 }
impl Default for TurnPolicy {
    fn default() -> Self { Self { default: DEFAULT_MAX_TURNS, limit: DEFAULT_MAX_TURNS_LIMIT } }
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
        Ok(Self { default: number("default_max_turns", DEFAULT_MAX_TURNS)?, limit: number("max_turns_limit", DEFAULT_MAX_TURNS_LIMIT)? })
    }
    pub fn from_table(table: &toml::Table) -> Self {
        let runtime = table.get("runtime").and_then(toml::Value::as_table);
        if table.contains_key("runtime") && runtime.is_none() { warn_invalid(); }
        let number = |key: &str, fallback, max| {
            match runtime.and_then(|r| r.get(key)) {
                None => fallback,
                Some(v) => v.as_integer().and_then(|n| u64::try_from(n).ok())
                    .filter(|n| (1..=max).contains(n)).unwrap_or_else(|| { warn_invalid(); fallback }),
            }
        };
        let mut policy = Self { default: number("default_max_turns", DEFAULT_MAX_TURNS, i64::MAX as u64),
            limit: number("max_turns_limit", DEFAULT_MAX_TURNS_LIMIT, 10000) };
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
    pub fn load(home: &Path) -> Self {
        match std::fs::read_to_string(home.join("config.toml")) {
            Ok(text) => text.parse::<toml::Table>().map(|t| Self::from_table(&t))
                .unwrap_or_else(|_| { warn_invalid(); Self::default() }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(_) => { warn_invalid(); Self::default() }
        }
    }
    pub fn resolve(self, raw: Option<i64>) -> u64 {
        let requested = match raw {
            Some(n) if n > 0 => n as u64,
            Some(_) => { warn_invalid(); self.default },
            None => self.default,
        };
        if requested > self.limit { warn_invalid(); }
        requested.min(self.limit)
    }
}

pub fn raw_agent_max_turns(dir: &Path) -> Option<i64> {
    match crate::agent_toml::read_effective_text(dir) {
        Ok(text) => match text.parse::<toml::Table>() {
            Ok(table) => match table.get("runtime") {
                None => None,
                Some(runtime) if runtime.is_table() => {
                    match runtime.clone().try_into::<crate::types::RuntimeSection>() {
                        Ok(runtime) => runtime.max_turns,
                        Err(_) => { warn_invalid(); None }
                    }
                }
                Some(_) => { warn_invalid(); None }
            },
            Err(_) => { warn_invalid(); None }
        },
        Err(_) => { warn_invalid(); None }
    }
}
pub fn max_turns(home: &Path, agent_dir: Option<&Path>) -> u64 {
    TurnPolicy::load(home).resolve(agent_dir.and_then(raw_agent_max_turns))
}
pub fn tool_loop_max_turns(home: &Path, agent_dir: Option<&Path>) -> usize {
    let policy = TurnPolicy::load(home);
    match agent_dir.and_then(raw_agent_max_turns) {
        Some(raw) => policy.resolve(Some(raw)) as usize,
        None => 10,
    }
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
        assert_eq!(p.resolve(Some(0)), 80);
        assert_eq!(p.resolve(Some(-1)), 80);
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
        for (literal, expected) in [("80", 80), ("1000", 90), ("0", 70), ("-1", 70), ("'oops'", 70)] {
            std::fs::write(dir.join("agent.toml"), format!("[runtime]\nmax_turns={literal}\n")).unwrap();
            assert_eq!(max_turns(home.path(), Some(&dir)), expected);
        }
        std::fs::write(dir.join("agent.toml"), "[runtime]\nmax_turns=81\n").unwrap();
        assert_eq!(tool_loop_max_turns(home.path(), Some(&dir)), 81);
        std::fs::write(home.path().join("config.toml"), "[broken").unwrap();
        assert!(TurnPolicy::load_checked(home.path()).is_err());
        assert_eq!(max_turns(home.path(), Some(&dir)), 81);
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
