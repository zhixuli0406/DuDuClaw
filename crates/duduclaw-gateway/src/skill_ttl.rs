//! TTL policy for newly created skill-related approval requests.

use std::path::Path;

/// Default expiry remains seven days; expiry always means denial.
pub const DEFAULT_SKILL_APPROVAL_TTL_SECS: i64 = 7 * 86_400;

/// Read the policy at request creation time. Existing rows keep their own TTL.
pub fn skill_approval_ttl_secs(home: &Path) -> i64 {
    let path = home.join("config.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => return DEFAULT_SKILL_APPROVAL_TTL_SECS,
    };
    let table = match text.parse::<toml::Table>() {
        Ok(table) => table,
        Err(_) => {
            tracing::warn!(path = %path.display(), "Invalid config TOML; using seven-day skill approval TTL");
            return DEFAULT_SKILL_APPROVAL_TTL_SECS;
        }
    };
    let Some(value) = table.get("approval").and_then(|v| v.get("skill_ttl_days")) else {
        return DEFAULT_SKILL_APPROVAL_TTL_SECS;
    };
    if let Some(days) = value.as_integer().filter(|days| (1..=30).contains(days)) {
        return days * 86_400;
    }
    tracing::warn!(path = %path.display(), "approval.skill_ttl_days must be an integer from 1 to 30; using seven days");
    DEFAULT_SKILL_APPROVAL_TTL_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_accepts_bounds_and_defaults_invalid_or_missing_config() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(skill_approval_ttl_secs(home.path()), 604_800);
        for (text, expected) in [
            ("[approval]\nskill_ttl_days = 1\n", 86_400),
            ("[approval]\nskill_ttl_days = 30\n", 2_592_000),
            ("[approval]\nskill_ttl_days = 0\n", 604_800),
            ("[approval]\nskill_ttl_days = 31\n", 604_800),
            ("[approval]\nskill_ttl_days = \"7\"\n", 604_800),
            ("[approval]\nskill_ttl_days = 1.5\n", 604_800),
            ("[approval]\nskill_ttl_days = true\n", 604_800),
            ("[approval]\n", 604_800),
            ("[general]\nname = 'test'\n", 604_800),
            ("[approval\nskill_ttl_days = 1", 604_800),
        ] {
            std::fs::write(home.path().join("config.toml"), text).unwrap();
            assert_eq!(skill_approval_ttl_secs(home.path()), expected, "{text}");
        }
    }
}
