//! Security posture report — surface DuDuClaw's security mechanisms as a
//! user-visible score + checklist.
//!
//! DuDuClaw's differentiation against agent platforms with large public CVE
//! counts is a defense-in-depth stack (fail-closed MCP auth, 3-layer hooks,
//! Ed25519-signed updates, HITL approvals, injection scanning, encrypted keys,
//! OS sandbox). This module inspects the live home + config and reports which
//! protections are active, so the value is visible instead of implicit.
//!
//! Two kinds of check: **architectural** (always-on by design — reported as a
//! reassurance) and **configured** (depends on the operator turning it on or
//! avoiding a foot-gun — the actionable ones).

use std::path::Path;

use serde::Serialize;

/// Severity of a failed check (drives score weight + display).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
}

/// One posture check result.
#[derive(Debug, Clone, Serialize)]
pub struct PostureCheck {
    pub id: &'static str,
    pub title: &'static str,
    pub passed: bool,
    pub severity: Severity,
    /// True when this protection is on by design (informational).
    pub architectural: bool,
    pub detail: String,
}

/// Full posture report.
#[derive(Debug, Clone, Serialize)]
pub struct PostureReport {
    pub checks: Vec<PostureCheck>,
    /// 0–100 weighted score (architectural checks count as passed baseline).
    pub score: u8,
    pub passed: usize,
    pub total: usize,
}

fn read_config(home_dir: &Path) -> Option<toml::Value> {
    std::fs::read_to_string(home_dir.join("config.toml"))
        .ok()
        .and_then(|t| t.parse::<toml::Value>().ok())
}

/// Scan `config.toml` for a plausibly-plaintext API key (a foot-gun): a value
/// under a `*key*`/`*token*`/`*secret*` field that looks like a raw provider
/// key rather than the AES/base64 ciphertext DuDuClaw stores. Best-effort.
fn has_plaintext_secret(cfg: &toml::Value) -> bool {
    fn walk(v: &toml::Value) -> bool {
        match v {
            toml::Value::Table(t) => t.iter().any(|(k, val)| {
                let key_is_secret = {
                    let lk = k.to_lowercase();
                    lk.contains("key") || lk.contains("token") || lk.contains("secret") || lk.contains("password")
                };
                if key_is_secret {
                    if let Some(s) = val.as_str() {
                        // Raw provider keys have recognizable prefixes; ciphertext
                        // does not. `enc`/base64-only values are fine.
                        if s.starts_with("sk-") || s.starts_with("sk-ant-") || s.starts_with("ghp_")
                            || s.starts_with("xoxb-") || s.starts_with("AKIA")
                        {
                            return true;
                        }
                    }
                }
                walk(val)
            }),
            toml::Value::Array(a) => a.iter().any(walk),
            _ => false,
        }
    }
    walk(cfg)
}

/// Whether any agent under `<home>/agents/*/agent.toml` sets `[budget] hard_stop`.
fn any_agent_hard_budget(home_dir: &Path) -> bool {
    let agents = home_dir.join("agents");
    let Ok(entries) = std::fs::read_dir(&agents) else {
        return false;
    };
    for e in entries.flatten() {
        let toml_path = e.path().join("agent.toml");
        if let Ok(text) = std::fs::read_to_string(&toml_path) {
            if let Ok(v) = text.parse::<toml::Value>() {
                let hard = v
                    .get("budget")
                    .and_then(|b| b.get("hard_stop"))
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                if hard {
                    return true;
                }
            }
        }
    }
    false
}

/// Compute the security posture from the live home directory.
pub fn compute_posture(home_dir: &Path) -> PostureReport {
    let cfg = read_config(home_dir);
    let hooks_present = home_dir.join(".claude").join("hooks").is_dir()
        || std::path::Path::new(".claude/hooks").is_dir();

    let mut checks: Vec<PostureCheck> = Vec::new();

    // ── Architectural (on by design) ──
    checks.push(PostureCheck {
        id: "mcp_auth_fail_closed",
        title: "MCP authorization is fail-closed",
        passed: true,
        severity: Severity::High,
        architectural: true,
        detail: "Unmapped MCP tools default to requiring Admin scope.".into(),
    });
    checks.push(PostureCheck {
        id: "signed_updates",
        title: "Updates are Ed25519-signature verified",
        passed: true,
        severity: Severity::High,
        architectural: true,
        detail: "Releases verified against a pinned minisign public key (fail-closed).".into(),
    });
    checks.push(PostureCheck {
        id: "injection_scanner",
        title: "Inbound prompt-injection scanning is active",
        passed: true,
        severity: Severity::Medium,
        architectural: true,
        detail: "6-category input guard runs on every inbound channel message.".into(),
    });
    checks.push(PostureCheck {
        id: "hitl_approvals",
        title: "HITL approval broker available (fail-closed TTL=DENY)",
        passed: true,
        severity: Severity::Medium,
        architectural: true,
        detail: "Irreversible tools can require human approval; expiry denies.".into(),
    });

    // ── Configured (actionable) ──
    checks.push(PostureCheck {
        id: "security_hooks",
        title: "Claude Code security hooks installed",
        passed: hooks_present,
        severity: Severity::Medium,
        architectural: false,
        detail: if hooks_present {
            "`.claude/hooks/` present (3-layer progressive defense).".into()
        } else {
            "No `.claude/hooks/` found — install the progressive-defense hooks.".into()
        },
    });

    let no_plaintext = cfg.as_ref().map(|c| !has_plaintext_secret(c)).unwrap_or(true);
    checks.push(PostureCheck {
        id: "no_plaintext_secrets",
        title: "No plaintext provider keys in config.toml",
        passed: no_plaintext,
        severity: Severity::High,
        architectural: false,
        detail: if no_plaintext {
            "No raw `sk-`/`ghp_`/`AKIA…` values detected in config.toml.".into()
        } else {
            "A field looks like a RAW provider key — encrypt it or use secret://.".into()
        },
    });

    let hard_budget = any_agent_hard_budget(home_dir);
    checks.push(PostureCheck {
        id: "budget_hard_stop",
        title: "At least one agent has a hard budget cap",
        passed: hard_budget,
        severity: Severity::Low,
        architectural: false,
        detail: if hard_budget {
            "A `[budget] hard_stop` cap is configured (runaway-cost protection).".into()
        } else {
            "No agent sets `[budget] hard_stop` — consider a daily_cap_cents.".into()
        },
    });

    // ── Score ──
    let total = checks.len();
    let passed = checks.iter().filter(|c| c.passed).count();
    // Weighted: failing a High costs more than a Low.
    let weight = |s: Severity| match s {
        Severity::High => 4u32,
        Severity::Medium => 2,
        Severity::Low => 1,
        Severity::Info => 1,
    };
    let max_w: u32 = checks.iter().map(|c| weight(c.severity)).sum();
    let got_w: u32 = checks.iter().filter(|c| c.passed).map(|c| weight(c.severity)).sum();
    let score = if max_w == 0 { 100 } else { ((got_w * 100) / max_w) as u8 };

    PostureReport { checks, score, passed, total }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn clean_home_scores_well() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[gateway]\nauto_update = true\n").unwrap();
        let r = compute_posture(dir.path());
        assert!(r.score >= 60, "architectural checks give a solid baseline: {}", r.score);
        assert_eq!(r.total, r.checks.len());
        // The 4 architectural checks always pass.
        assert!(r.passed >= 4);
    }

    #[test]
    fn plaintext_key_is_flagged() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[providers]\nanthropic_api_key = \"sk-ant-api03-SECRETLEAK\"\n",
        )
        .unwrap();
        let r = compute_posture(dir.path());
        let check = r.checks.iter().find(|c| c.id == "no_plaintext_secrets").unwrap();
        assert!(!check.passed, "raw sk-ant- key must be flagged");
    }

    #[test]
    fn encrypted_config_not_flagged() {
        let dir = tempdir().unwrap();
        // base64-looking ciphertext under a *_enc key is fine.
        std::fs::write(
            dir.path().join("config.toml"),
            "[providers]\napi_key_enc = \"YmFzZTY0Y2lwaGVydGV4dA==\"\n",
        )
        .unwrap();
        let r = compute_posture(dir.path());
        let check = r.checks.iter().find(|c| c.id == "no_plaintext_secrets").unwrap();
        assert!(check.passed, "ciphertext must NOT be flagged");
    }

    #[test]
    fn hard_budget_detected() {
        let dir = tempdir().unwrap();
        let ad = dir.path().join("agents").join("a");
        std::fs::create_dir_all(&ad).unwrap();
        std::fs::write(ad.join("agent.toml"), "[budget]\nhard_stop = true\nmonthly_limit_cents = 100\n").unwrap();
        let r = compute_posture(dir.path());
        assert!(r.checks.iter().find(|c| c.id == "budget_hard_stop").unwrap().passed);
    }
}
