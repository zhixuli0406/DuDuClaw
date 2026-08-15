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

// ── WP-K: dashboard "credential hygiene" friendly cleanup surface ──────────
//
// `has_plaintext_secret` above answers one boolean ("is anything in
// config.toml plaintext-looking?") for the posture score. The dashboard needs
// something an operator can *act* on: which field, and is it safe to
// auto-remove. That safety question is exactly the 2026-08-15 incident
// (commercial/docs/DESIGN-credentials-doctrine-2026-08.md §1.5): a plaintext
// `[[accounts]].oauth_token` sat next to its already-authoritative
// `oauth_token_enc` twin with nothing ever cleaning it up, and — separately —
// `system.config`'s array-of-tables masking gap meant it could be read back
// verbatim. The functions below split "plaintext with a confirmed encrypted
// twin" (pure residue, safe to delete — the twin is what every read path
// already uses) from "plaintext with no twin" (an unencrypted secret this
// pass reports but never touches, since guessing how to encrypt an unfamiliar
// field risks writing a corrupt config.toml).

/// One plaintext-credential finding. Carries a TOML *path* only — never the
/// value, never even a masked fragment of one (coding convention #4 fail-closed
/// security gates + this endpoint's whole reason to exist is to be safe to
/// render on the dashboard).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CredentialFinding {
    /// Dotted/bracket TOML path to the plaintext field, e.g.
    /// `"accounts[0].oauth_token"`.
    pub path: String,
    /// True when a `<field>_enc` twin already exists alongside it — pure
    /// residue, safe for `strip_twin_residue` to remove. False means this
    /// plaintext value has no encrypted counterpart yet; it is reported for
    /// manual handling only and `strip_twin_residue` will not touch it.
    pub has_enc_twin: bool,
    pub severity: Severity,
}

/// A value under a `*key*`/`*token*`/`*secret*`/`*password*` field name that
/// looks like a raw provider secret rather than DuDuClaw's own AES/base64
/// ciphertext. Deliberately the same conservative prefix heuristic as
/// [`has_plaintext_secret`] (kept as a separate, untouched function so its
/// existing behavior/tests stay byte-identical) — used here only for the
/// no-twin case, where twin-existence isn't available as a stronger signal.
fn looks_like_raw_secret(s: &str) -> bool {
    s.starts_with("sk-") || s.starts_with("ghp_") || s.starts_with("xoxb-") || s.starts_with("AKIA")
}

/// Field-name heuristic shared by [`find_plaintext_secrets`] and
/// [`strip_twin_residue`]: secret-shaped key, excluding `_enc` fields
/// themselves (those ARE the ciphertext, never a finding).
fn is_secret_key_name(k: &str) -> bool {
    let lk = k.to_lowercase();
    (lk.contains("key") || lk.contains("token") || lk.contains("secret") || lk.contains("password"))
        && !lk.ends_with("_enc")
}

/// Walk the entire config tree (any depth, including arrays-of-tables like
/// `[[accounts]]`) and report every plaintext credential field: either (a) it
/// has a confirmed `_enc` twin — pure residue — or (b) it has no twin but
/// looks like a raw provider key. Read-only; never mutates.
pub fn find_plaintext_secrets(table: &toml::Table) -> Vec<CredentialFinding> {
    fn walk(t: &toml::Table, path: &str, out: &mut Vec<CredentialFinding>) {
        for (k, v) in t.iter() {
            let field_path = if path.is_empty() {
                k.clone()
            } else {
                format!("{path}.{k}")
            };
            if is_secret_key_name(k) {
                if let Some(s) = v.as_str() {
                    if !s.is_empty() {
                        let twin_key = format!("{k}_enc");
                        let has_enc_twin = t
                            .get(&twin_key)
                            .and_then(|tv| tv.as_str())
                            .map(|tv| !tv.is_empty())
                            .unwrap_or(false);
                        if has_enc_twin || looks_like_raw_secret(s) {
                            out.push(CredentialFinding {
                                path: field_path.clone(),
                                has_enc_twin,
                                severity: Severity::High,
                            });
                        }
                    }
                }
            }
            match v {
                toml::Value::Table(sub) => walk(sub, &field_path, out),
                toml::Value::Array(arr) => {
                    for (i, item) in arr.iter().enumerate() {
                        if let toml::Value::Table(sub) = item {
                            walk(sub, &format!("{field_path}[{i}]"), out);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(table, "", &mut out);
    out
}

/// Remove plaintext keys that have a confirmed `_enc` twin — the ONLY case
/// this pass auto-cleans. Fields with no twin are left completely untouched
/// (see module docs above for why). Returns the TOML paths removed, for audit
/// logging — never the values. Idempotent: calling this again on the result
/// returns an empty vec.
pub fn strip_twin_residue(table: &mut toml::Table) -> Vec<String> {
    fn walk(t: &mut toml::Table, path: &str, removed: &mut Vec<String>) {
        let doomed: Vec<String> = t
            .iter()
            .filter_map(|(k, v)| {
                if !is_secret_key_name(k) {
                    return None;
                }
                let plaintext_present = v.as_str().map(|s| !s.is_empty()).unwrap_or(false);
                if !plaintext_present {
                    return None;
                }
                let twin_key = format!("{k}_enc");
                let has_twin = t
                    .get(&twin_key)
                    .and_then(|tv| tv.as_str())
                    .map(|tv| !tv.is_empty())
                    .unwrap_or(false);
                has_twin.then(|| k.clone())
            })
            .collect();
        for k in doomed {
            t.remove(&k);
            removed.push(if path.is_empty() {
                k
            } else {
                format!("{path}.{k}")
            });
        }
        // Recurse into whatever remains (nested tables + arrays-of-tables).
        let keys: Vec<String> = t.keys().cloned().collect();
        for k in keys {
            let child_path = if path.is_empty() {
                k.clone()
            } else {
                format!("{path}.{k}")
            };
            match t.get_mut(&k) {
                Some(toml::Value::Table(sub)) => walk(sub, &child_path, removed),
                Some(toml::Value::Array(arr)) => {
                    for (i, item) in arr.iter_mut().enumerate() {
                        if let toml::Value::Table(sub) = item {
                            walk(sub, &format!("{child_path}[{i}]"), removed);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut removed = Vec::new();
    walk(table, "", &mut removed);
    removed
}

// ── WP-H1 P1: the structured credential inventory ─────────────────────────
//
// `find_plaintext_secrets` answers "what is wrong". The inventory answers the
// prior question — "what credentials does this deployment have, and where does
// each one actually come from" — which is the design doc's §2.3 `describe()`
// contract applied to a whole config file. It is the reason `describe()` was
// built not to resolve: forty fields render for zero backend round-trips.
//
// Both walk the same tree with the same field-name heuristic, so a field can
// never appear in one and be invisible to the other.

/// One credential field and its non-secret status.
///
/// Serialises to exactly the fields of
/// [`duduclaw_security::secret_ref::SecretStatus`] plus a `path`. Never carries
/// a value, a ciphertext, or a masked fragment of either.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CredentialEntry {
    /// Dotted/bracket TOML path of the logical field, always the **base** name
    /// (`accounts[0].oauth_token`), never the `_enc` twin — the twin is a
    /// storage detail, reported through `source` instead.
    pub path: String,
    pub configured: bool,
    /// `unset` | `inline` | `legacy` | `env` | `keychain` | `file` | `vault` |
    /// `onepassword` | `infisical` | `local` | `ambiguous`.
    pub source: duduclaw_security::secret_ref::SourceKind,
    /// Non-secret human description, e.g. `"encrypted(keyfile)"`,
    /// `"env:TELEGRAM_BOT_TOKEN"`, `"keychain:duduclaw/telegram"`.
    pub source_label: String,
    /// Whether the dashboard may overwrite this field. External references are
    /// rotated in their own backend, so the UI must not offer to.
    pub writable: bool,
    /// A plaintext twin sits next to a `_enc` twin (design §1.5).
    pub residue: bool,
}

/// Walk a parsed `config.toml` and describe every credential field it holds.
///
/// Pairing rule, identical to the one every read path uses: `<field>` is the
/// logical credential and `<field>_enc` is its ciphertext twin, so a table
/// containing only `x_enc` still yields one entry at path `x`. Ordering is the
/// TOML map's own (`BTreeMap`, i.e. sorted), so the dashboard list is stable
/// across refreshes.
pub fn credential_inventory(table: &toml::Table) -> Vec<CredentialEntry> {
    use duduclaw_security::secret_ref::SecretRef;

    fn describe_field(t: &toml::Table, base: &str, path: String, out: &mut Vec<CredentialEntry>) {
        let enc_key = format!("{base}_enc");
        let st = SecretRef::classify(
            t.get(&enc_key).and_then(|v| v.as_str()),
            t.get(base).and_then(|v| v.as_str()),
        )
        .describe();
        out.push(CredentialEntry {
            path,
            configured: st.configured,
            source: st.source,
            source_label: st.source_label,
            writable: st.writable,
            residue: st.residue,
        });
    }

    fn walk(t: &toml::Table, path: &str, out: &mut Vec<CredentialEntry>) {
        // Collect logical base names first so `x` and `x_enc` produce exactly
        // one entry regardless of which the map yields first.
        let mut bases: Vec<String> = Vec::new();
        for k in t.keys() {
            let base = k.strip_suffix("_enc").unwrap_or(k);
            if !is_secret_key_name(base) {
                continue;
            }
            // Only string-valued fields are credentials; a `[tokens]` table
            // named like one is a container, handled by the recursion below.
            let is_stringy = t.get(base).is_some_and(|v| v.is_str())
                || t.get(&format!("{base}_enc")).is_some_and(|v| v.is_str());
            if is_stringy && !bases.iter().any(|b| b == base) {
                bases.push(base.to_string());
            }
        }
        bases.sort();
        for base in bases {
            let field_path = if path.is_empty() {
                base.clone()
            } else {
                format!("{path}.{base}")
            };
            describe_field(t, &base, field_path, out);
        }

        for (k, v) in t.iter() {
            // `[mcp_keys]` stores the API key as the *table name*
            // (design §1.4). An inventory renders paths, so descending here
            // would print the gateway's own admin key into the dashboard —
            // the exact leak this pass exists to close. The section is skipped
            // whole; `mcp_keys.list` already reports it, masked.
            if path.is_empty() && k == "mcp_keys" {
                continue;
            }
            let child = if path.is_empty() {
                k.clone()
            } else {
                format!("{path}.{k}")
            };
            match v {
                toml::Value::Table(sub) => walk(sub, &child, out),
                toml::Value::Array(arr) => {
                    for (i, item) in arr.iter().enumerate() {
                        if let toml::Value::Table(sub) = item {
                            walk(sub, &format!("{child}[{i}]"), out);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let mut out = Vec::new();
    walk(table, "", &mut out);
    out
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

    // ── WP-K: find_plaintext_secrets / strip_twin_residue ──────────────────

    #[test]
    fn hygiene_clean_config_has_no_findings() {
        let toml_str = r#"
            [gateway]
            auto_update = true

            [[accounts]]
            id = "a1"
            oauth_token_enc = "Y2lwaGVydGV4dA=="
        "#;
        let table: toml::Table = toml_str.parse().unwrap();
        let findings = find_plaintext_secrets(&table);
        assert!(
            findings.is_empty(),
            "encrypted-only account must not be flagged: {findings:?}"
        );
    }

    #[test]
    fn hygiene_detects_twin_residue_with_array_index_path() {
        let toml_str = r#"
            [[accounts]]
            id = "a1"
            oauth_token = "sk-ant-oat01-PLAINTEXTLEAK"
            oauth_token_enc = "Y2lwaGVydGV4dA=="
        "#;
        let table: toml::Table = toml_str.parse().unwrap();
        let findings = find_plaintext_secrets(&table);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, "accounts[0].oauth_token");
        assert!(findings[0].has_enc_twin);
        assert_eq!(findings[0].severity, Severity::High);
    }

    #[test]
    fn hygiene_flags_no_twin_secret_without_marking_it_cleanable() {
        let toml_str = r#"
            [providers]
            anthropic_api_key = "sk-ant-api03-nolongertwin"
        "#;
        let table: toml::Table = toml_str.parse().unwrap();
        let findings = find_plaintext_secrets(&table);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, "providers.anthropic_api_key");
        assert!(!findings[0].has_enc_twin);

        // Cleanup must never touch a no-twin field — this pass only removes
        // confirmed residue.
        let mut mutable = table.clone();
        let removed = strip_twin_residue(&mut mutable);
        assert!(removed.is_empty());
        assert!(
            mutable
                .get("providers")
                .and_then(|p| p.get("anthropic_api_key"))
                .is_some(),
            "no-twin field must survive strip_twin_residue untouched"
        );
    }

    #[test]
    fn strip_twin_residue_removes_plaintext_keeps_enc_and_is_idempotent() {
        let toml_str = r#"
            [[accounts]]
            id = "a1"
            oauth_token = "sk-ant-oat01-PLAINTEXTLEAK"
            oauth_token_enc = "Y2lwaGVydGV4dA=="
        "#;
        let mut table: toml::Table = toml_str.parse().unwrap();
        let removed = strip_twin_residue(&mut table);
        assert_eq!(removed, vec!["accounts[0].oauth_token".to_string()]);

        let accounts = table.get("accounts").unwrap().as_array().unwrap();
        let acct = accounts[0].as_table().unwrap();
        assert!(
            acct.get("oauth_token").is_none(),
            "plaintext key must be removed, not blanked"
        );
        assert_eq!(
            acct.get("oauth_token_enc").and_then(|v| v.as_str()),
            Some("Y2lwaGVydGV4dA==")
        );

        // Idempotent: nothing left to remove on a second pass.
        let removed_again = strip_twin_residue(&mut table);
        assert!(removed_again.is_empty());
    }

    // ── WP-H1 P1: credential_inventory ────────────────────────────────────

    use duduclaw_security::secret_ref::SourceKind;

    fn inv(toml_str: &str) -> Vec<CredentialEntry> {
        credential_inventory(&toml_str.parse::<toml::Table>().unwrap())
    }

    fn at<'a>(entries: &'a [CredentialEntry], path: &str) -> &'a CredentialEntry {
        entries
            .iter()
            .find(|e| e.path == path)
            .unwrap_or_else(|| panic!("no entry at {path}; got {:?}", entries.iter().map(|e| &e.path).collect::<Vec<_>>()))
    }

    #[test]
    fn inventory_reports_each_source_kind() {
        let entries = inv(r#"
            [channels]
            telegram_bot_token_enc = "Y2lwaGVy"
            discord_bot_token = "secret://env/DISCORD_TOKEN"
            slack_bot_token = "xoxb-plain-legacy"
            line_channel_token = "secret://keychain/duduclaw/line"
            teams_app_password = "secret://file//run/secrets/teams"
            wecom_secret = ""
        "#);
        assert_eq!(at(&entries, "channels.telegram_bot_token").source, SourceKind::Inline);
        assert_eq!(at(&entries, "channels.discord_bot_token").source, SourceKind::Env);
        assert_eq!(at(&entries, "channels.slack_bot_token").source, SourceKind::Legacy);
        assert_eq!(at(&entries, "channels.line_channel_token").source, SourceKind::Keychain);
        assert_eq!(at(&entries, "channels.teams_app_password").source, SourceKind::File);
        assert!(!at(&entries, "channels.wecom_secret").configured);
    }

    #[test]
    fn inventory_pairs_enc_with_its_base_and_never_lists_the_twin_separately() {
        let entries = inv(r#"
            [channels]
            telegram_bot_token = "plain-residue"
            telegram_bot_token_enc = "Y2lwaGVy"
        "#);
        assert_eq!(entries.len(), 1, "one logical field, not two: {entries:?}");
        let e = at(&entries, "channels.telegram_bot_token");
        assert_eq!(e.source, SourceKind::Inline, "ciphertext wins");
        assert!(e.residue, "the plaintext twin is residue");
        assert!(
            !entries.iter().any(|e| e.path.ends_with("_enc")),
            "the `_enc` twin is a storage detail, not its own credential"
        );
    }

    #[test]
    fn inventory_descends_arrays_of_tables() {
        let entries = inv(r#"
            [[accounts]]
            id = "a1"
            oauth_token_enc = "Y2lwaGVy"
            [[accounts]]
            id = "a2"
            api_key = "secret://vault/anthropic"
        "#);
        assert_eq!(at(&entries, "accounts[0].oauth_token").source, SourceKind::Inline);
        let a2 = at(&entries, "accounts[1].api_key");
        assert_eq!(a2.source, SourceKind::Vault);
        assert!(!a2.writable, "an external reference is not UI-writable");
    }

    /// §1.4's second leak: `[mcp_keys]` stores the key as the table *name*, so
    /// an inventory that renders paths would print it verbatim.
    #[test]
    fn inventory_never_renders_the_mcp_keys_table_names() {
        let entries = inv(r#"
            [mcp_keys."ddc_prod_a1b2c3d4e5f6"]
            scopes = ["admin"]
            label = "internal"
            [mcp_keys."ddc_prod_a1b2c3d4e5f6".meta]
            rotation_token = "should-not-surface"
        "#);
        let joined = entries
            .iter()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            !joined.contains("ddc_prod_a1b2c3d4e5f6"),
            "mcp_keys table names must never reach the inventory: {joined}"
        );
    }

    #[test]
    fn inventory_serializes_without_any_value_or_ciphertext() {
        let entries = inv(r#"
            [channels]
            telegram_bot_token = "plain-secret-value"
            telegram_bot_token_enc = "Y2lwaGVydGV4dHZhbHVl"
        "#);
        let json = serde_json::to_string(&entries).unwrap();
        assert!(!json.contains("plain-secret-value"), "{json}");
        assert!(!json.contains("Y2lwaGVydGV4dHZhbHVl"), "{json}");
    }

    /// Tick headers were the one pure-plaintext credential surface; the
    /// inventory is how an operator now sees which of them still are.
    #[test]
    fn inventory_covers_tick_source_headers() {
        let entries = inv(r#"
            [[tick.sources]]
            id = "feed"
            [tick.sources.headers]
            X-API-Key = "secret://env/FEED_KEY"
            Accept = "application/json"
        "#);
        assert_eq!(
            at(&entries, "tick.sources[0].headers.X-API-Key").source,
            SourceKind::Env
        );
        assert!(
            !entries.iter().any(|e| e.path.ends_with("Accept")),
            "a non-credential header must not be listed"
        );
    }

    /// The inventory and the hygiene scan must agree about which fields exist:
    /// a residue finding with no inventory row would be invisible in the list
    /// the operator actually reads.
    #[test]
    fn every_residue_finding_has_an_inventory_row() {
        let toml_str = r#"
            [[accounts]]
            oauth_token = "sk-ant-oat01-PLAINTEXTLEAK"
            oauth_token_enc = "Y2lwaGVy"
            [channels]
            telegram_bot_token = "plain"
            telegram_bot_token_enc = "Y2lwaGVy"
        "#;
        let table: toml::Table = toml_str.parse().unwrap();
        let entries = credential_inventory(&table);
        for f in find_plaintext_secrets(&table).iter().filter(|f| f.has_enc_twin) {
            let e = at(&entries, &f.path);
            assert!(e.residue, "{} must read as residue in the inventory", f.path);
        }
    }
}
