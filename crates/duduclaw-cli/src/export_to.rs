//! `duduclaw export --format agentcompanies` — export DuDuClaw agents as a
//! portable **agentcompanies** package so the paperclip ecosystem (and any
//! other consumer of the vendor-neutral format) can "hire" DuDuClaw agents.
//!
//! Spec facts (verified 2026-07-11, first-hand, against
//! `paperclipai/paperclip` `docs/companies/companies-spec.md`, document
//! version `agentcompanies/v1-draft`):
//! - frontmatter `schema:` field value is `agentcompanies/v1`;
//! - layout: `COMPANY.md` (requires `name`/`description`/`slug`),
//!   `agents/<slug>/AGENTS.md` (`name`/`title`/`reportsTo`/`skills`/`docs`,
//!   body = default instructions), `skills/<slug>/SKILL.md`,
//!   `tasks/<slug>/TASK.md`, `teams/<slug>/TEAM.md`,
//!   `projects/<slug>/...`, optional `.paperclip.yaml` vendor sidecar
//!   (`schema: paperclip/v1`, `agents.<slug>.adapter.{type,config}`);
//! - skill references resolve `skills/<shortname>/SKILL.md` by convention;
//!   "Exporters should emit shortnames in agent definitions whenever
//!   possible";
//! - export rules: compliant exporters MUST omit machine-local identifiers,
//!   timestamps, secret values, and machine-specific paths.
//!
//! Mapping DuDuClaw → package:
//! - `agents/<id>/SOUL.md`      → AGENTS.md body, verbatim;
//! - `agent.toml [agent]`       → frontmatter (`reports_to` → `reportsTo`,
//!   kept only when the parent is inside the export set);
//! - `agents/<id>/SKILLS/<s>/`  → `skills/<shortname>/` (deduplicated across
//!   agents; divergent same-name skills get an `--<agent>` suffix);
//! - `CONTRACT.toml [boundaries]` → `agents/<slug>/docs/contract.md`,
//!   referenced from the `docs:` frontmatter slot (NOT appended to the body,
//!   so a re-import reproduces SOUL.md byte-stable);
//! - CLAUDE.md is runtime-generated boilerplate and is NOT exported (noted).
//!
//! Secrets (channel tokens / API keys / OAuth) are never read in the first
//! place — the exporter only touches `agent.toml` identity fields, SOUL.md,
//! CONTRACT.toml and SKILL dirs — and every emitted text file (SKILL.md,
//! sidecar scripts/docs, the rendered contract doc) additionally passes a
//! known-prefix **and shape-heuristic** secret scrub (fail-closed: matches
//! are replaced with `[REDACTED-SECRET]` and the item is reported PARTIAL,
//! never EXPORTED). Symlinks inside skill dirs are never followed — they are
//! skipped, reported PARTIAL, and named in a manifest note. COMPANY.md
//! carries the explicit exclusion note required by the spec's export rules.
//!
//! Output is deterministic: agents and skills are emitted in sorted order,
//! YAML is hand-rendered in a fixed field order, and no timestamps are
//! written — repeated exports diff cleanly.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use duduclaw_core::error::{DuDuClawError, Result};

pub(crate) const SCHEMA: &str = "agentcompanies/v1";
const COMPANY_SLUG: &str = "duduclaw-agents";
const REDACTED: &str = "[REDACTED-SECRET]";

// ─────────────────────────── Report ───────────────────────────

/// Per-item export outcome. Kept separate from the migrate-from `Report`
/// because that JSON shape is a frontend-locked *import* contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExportStatus {
    Exported,
    Partial(String),
    Skipped(String),
}

#[derive(Debug, Clone)]
pub(crate) struct ExportItem {
    pub kind: String,
    pub name: String,
    pub status: ExportStatus,
}

#[derive(Debug, Clone)]
pub(crate) struct ExportReport {
    pub out: String,
    pub items: Vec<ExportItem>,
    pub notes: Vec<String>,
}

impl ExportReport {
    fn new(out: &Path) -> Self {
        ExportReport {
            out: out.display().to_string(),
            items: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn exported(&mut self, kind: &str, name: &str) {
        self.items.push(ExportItem {
            kind: kind.into(),
            name: name.into(),
            status: ExportStatus::Exported,
        });
    }

    fn partial(&mut self, kind: &str, name: &str, reason: impl Into<String>) {
        self.items.push(ExportItem {
            kind: kind.into(),
            name: name.into(),
            status: ExportStatus::Partial(reason.into()),
        });
    }

    fn skipped(&mut self, kind: &str, name: &str, reason: impl Into<String>) {
        self.items.push(ExportItem {
            kind: kind.into(),
            name: name.into(),
            status: ExportStatus::Skipped(reason.into()),
        });
    }

    fn note(&mut self, n: impl Into<String>) {
        self.notes.push(n.into());
    }

    fn count(&self, want: fn(&ExportStatus) -> bool) -> usize {
        self.items.iter().filter(|i| want(&i.status)).count()
    }

    pub fn overall(&self) -> &'static str {
        let ok = self.count(|s| matches!(s, ExportStatus::Exported));
        let total = self.items.len();
        if total == 0 || ok == 0 {
            "PARTIAL"
        } else if ok == total {
            "COMPLETE"
        } else {
            "DEGRADED"
        }
    }

    /// Machine-readable summary printed on stdout under `--json`.
    pub fn to_json(&self) -> serde_json::Value {
        let items: Vec<serde_json::Value> = self
            .items
            .iter()
            .map(|i| {
                let (status, reason) = match &i.status {
                    ExportStatus::Exported => ("exported", None),
                    ExportStatus::Partial(r) => ("partial", Some(r.as_str())),
                    ExportStatus::Skipped(r) => ("skipped", Some(r.as_str())),
                };
                serde_json::json!({
                    "kind": i.kind, "name": i.name, "status": status, "reason": reason,
                })
            })
            .collect();
        serde_json::json!({
            "format": "agentcompanies",
            "schema": SCHEMA,
            "out": self.out,
            "items": items,
            "summary": {
                "exported": self.count(|s| matches!(s, ExportStatus::Exported)),
                "partial": self.count(|s| matches!(s, ExportStatus::Partial(_))),
                "skipped": self.count(|s| matches!(s, ExportStatus::Skipped(_))),
            },
            "verdict": self.overall(),
            "notes": self.notes,
        })
    }

    pub fn render_console(&self) {
        use console::style;
        println!();
        println!(
            "  {} 匯出 DuDuClaw agents → agentcompanies 套件",
            style("🐾").cyan()
        );
        println!("  {} 輸出: {}", style("→").cyan(), self.out);
        println!();
        for item in &self.items {
            let (icon, tag, reason) = match &item.status {
                ExportStatus::Exported => (style("✓").green().to_string(), "EXPORTED", None),
                ExportStatus::Partial(r) => {
                    (style("◐").yellow().to_string(), "PARTIAL", Some(r.as_str()))
                }
                ExportStatus::Skipped(r) => {
                    (style("−").dim().to_string(), "SKIPPED", Some(r.as_str()))
                }
            };
            let reason = reason.map(|r| format!(" — {r}")).unwrap_or_default();
            println!(
                "  {icon} [{}] {} {}{}",
                item.kind,
                item.name,
                style(tag).bold(),
                style(reason).dim()
            );
        }
        if !self.notes.is_empty() {
            println!();
            println!("  {}", style("備註").bold());
            for n in &self.notes {
                println!("    {} {}", style("•").dim(), n);
            }
        }
        println!();
        let verdict = self.overall();
        let styled = match verdict {
            "COMPLETE" => style(verdict).green().bold(),
            "DEGRADED" => style(verdict).yellow().bold(),
            _ => style(verdict).red().bold(),
        };
        println!("  {} 總結: {}", style("▶").cyan(), styled);
        println!();
    }
}

// ─────────────────────────── Agent model ───────────────────────────

/// One agent read from `<home>/agents/<id>/`, ready to render.
#[derive(Debug, Clone)]
pub(crate) struct ExportedAgent {
    pub slug: String,
    pub display_name: String,
    pub role: String,
    /// Raw `reports_to` from agent.toml (may point outside the export set).
    pub reports_to_raw: String,
    pub icon: String,
    pub trigger: String,
    pub model: String,
    pub soul: Option<String>,
    pub contract: Option<ContractSummary>,
    /// (skill dir name, absolute path) pairs, sorted by name.
    pub skill_dirs: Vec<(String, PathBuf)>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ContractSummary {
    pub must_not: Vec<String>,
    pub must_always: Vec<String>,
}

fn tstr(t: &toml::value::Table, key: &str) -> String {
    t.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Read one agent directory defensively (missing/odd fields never panic).
/// Returns `None` when `agent.toml` is absent or unparseable.
fn read_agent(home: &Path, id: &str) -> Option<ExportedAgent> {
    let dir = home.join("agents").join(id);
    let raw = std::fs::read_to_string(dir.join("agent.toml")).ok()?;
    let table: toml::value::Table = raw.parse::<toml::Table>().ok()?;
    let agent = table.get("agent").and_then(|v| v.as_table())?;
    let model = table
        .get("model")
        .and_then(|v| v.as_table())
        .map(|m| tstr(m, "preferred"))
        .unwrap_or_default();

    let soul = std::fs::read_to_string(dir.join("SOUL.md")).ok();
    let contract = read_contract(&dir.join("CONTRACT.toml"));

    let mut skill_dirs: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir.join("SKILLS")) {
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            // Guard: only plain directory names holding a SKILL.md; hidden
            // names and anything path-traversal-ish is skipped fail-closed.
            if p.is_dir()
                && !name.starts_with('.')
                && !name.contains("..")
                && p.join("SKILL.md").exists()
            {
                skill_dirs.push((name, p));
            }
        }
    }
    skill_dirs.sort();

    let display_name = {
        let d = tstr(agent, "display_name");
        if d.is_empty() { id.to_string() } else { d }
    };
    Some(ExportedAgent {
        slug: id.to_string(),
        display_name,
        role: {
            let r = tstr(agent, "role");
            if r.is_empty() { "specialist".into() } else { r }
        },
        reports_to_raw: tstr(agent, "reports_to"),
        icon: tstr(agent, "icon"),
        trigger: tstr(agent, "trigger"),
        model,
        soul,
        contract,
        skill_dirs,
    })
}

/// Parse `CONTRACT.toml [boundaries] must_not / must_always` (both optional).
fn read_contract(path: &Path) -> Option<ContractSummary> {
    let raw = std::fs::read_to_string(path).ok()?;
    let table: toml::value::Table = raw.parse::<toml::Table>().ok()?;
    let b = table.get("boundaries").and_then(|v| v.as_table())?;
    let list = |key: &str| -> Vec<String> {
        b.get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    let summary = ContractSummary {
        must_not: list("must_not"),
        must_always: list("must_always"),
    };
    if summary.must_not.is_empty() && summary.must_always.is_empty() {
        None
    } else {
        Some(summary)
    }
}

// ─────────────────────────── Pure renderers ───────────────────────────

/// Double-quoted YAML scalar (safe for emoji / CJK / quotes / backslashes).
pub(crate) fn yq(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Scrub well-known secret token shapes from text to be exported.
///
/// Prefix-based plus shape heuristics (no regex dependency). A prefix match
/// runs while the following chars are `[A-Za-z0-9_-]` and must reach the
/// per-prefix minimum tail length to trip — short false positives (e.g. the
/// literal string "sk-ant-") are left alone. Additional shapes:
///
/// - Telegram bot token `<6-12 digits>:<30+ base64ish>`;
/// - Discord bot token: three dot-joined base64ish segments (≥20 / ≥5 / ≥20);
/// - AWS secret access key heuristic: standalone 40-char `[A-Za-z0-9/+]` run
///   with mixed case + a digit — deliberately CONSERVATIVE (an export must
///   prefer a false redaction over a leaked credential);
/// - long (≥60 char, ≥75% alphanumeric) base64 runs — covers LINE channel
///   access tokens, WhatsApp-style blobs and our `_enc` AES-GCM ciphertext
///   shape; same conservative bias.
///
/// Returns the scrubbed text and how many redactions were made.
pub(crate) fn redact_secrets(text: &str) -> (String, usize) {
    /// `(prefix, minimum tail length)` — the tail keeps prose mentions of a
    /// bare prefix out; `EAA` (WhatsApp/Meta Graph tokens) needs a long tail
    /// because three uppercase letters alone are common English.
    const PREFIXES: &[(&str, usize)] = &[
        ("sk-ant-", 8),
        ("sk-or-", 8),   // OpenRouter
        ("xoxb-", 8),
        ("xoxp-", 8),
        ("xapp-", 8),
        ("ghp_", 8),
        ("github_pat_", 8),
        ("gsk_", 8),     // Groq
        ("AKIA", 8),
        ("EAA", 20),     // WhatsApp Cloud API / Meta Graph
    ];
    let token_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    // Broad candidate-run alphabet for the shape heuristics below. `=` is
    // deliberately EXCLUDED: it glues `KEY=value` assignments into one run
    // (hiding the 40-char AWS shape); base64 padding `=` merely trails the
    // redacted body, which is harmless.
    let run_char =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '.');
    let mut out = String::with_capacity(text.len());
    let mut count = 0usize;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        // Known prefixes.
        let rest: String = chars[i..].iter().take(16).collect();
        let mut matched = false;
        for (p, min_tail) in PREFIXES {
            if rest.starts_with(p) {
                let mut j = i + p.chars().count();
                let start_tail = j;
                while j < chars.len() && token_char(chars[j]) {
                    j += 1;
                }
                if j - start_tail >= *min_tail {
                    out.push_str(REDACTED);
                    count += 1;
                    i = j;
                    matched = true;
                }
                break;
            }
        }
        if matched {
            continue;
        }
        // Telegram bot token: digits ':' long base64ish tail.
        if chars[i].is_ascii_digit() && (i == 0 || !token_char(chars[i - 1])) {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            let ndigits = j - i;
            if (6..=12).contains(&ndigits) && j < chars.len() && chars[j] == ':' {
                let mut k = j + 1;
                while k < chars.len() && token_char(chars[k]) {
                    k += 1;
                }
                if k - (j + 1) >= 30 {
                    out.push_str(REDACTED);
                    count += 1;
                    i = k;
                    continue;
                }
            }
        }
        // Shape heuristics on a maximal candidate run, evaluated only at a
        // run boundary (previous char outside the run alphabet).
        if run_char(chars[i]) && (i == 0 || !run_char(chars[i - 1])) {
            let mut j = i;
            while j < chars.len() && run_char(chars[j]) {
                j += 1;
            }
            let run: String = chars[i..j].iter().collect();
            if run_is_secret_shape(&run) {
                out.push_str(REDACTED);
                count += 1;
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    (out, count)
}

/// Shape classification for a maximal candidate run — see [`redact_secrets`].
/// Conservative by design: in an EXPORT context a false positive costs a
/// mangled doc line; a false negative leaks a credential.
fn run_is_secret_shape(run: &str) -> bool {
    // Discord bot token: `base64ish.base64ish.base64ish` (≥20 / ≥5 / ≥20).
    let parts: Vec<&str> = run.split('.').collect();
    if parts.len() == 3 {
        let seg_ok = |s: &str, min: usize| {
            s.len() >= min
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        if seg_ok(parts[0], 20) && seg_ok(parts[1], 5) && seg_ok(parts[2], 20) {
            return true;
        }
    }
    // AWS secret access key heuristic: exactly 40 chars of [A-Za-z0-9/+]
    // with mixed case and a digit (rules out plain words and lone hex SHAs).
    if run.len() == 40
        && run
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '+')
        && run.chars().any(|c| c.is_ascii_uppercase())
        && run.chars().any(|c| c.is_ascii_lowercase())
        && run.chars().any(|c| c.is_ascii_digit())
    {
        return true;
    }
    // Long base64 run (LINE tokens, `_enc` ciphertext): ≥60 chars of
    // [A-Za-z0-9+/=], at least 75% alphanumeric (rules out `====`/`----`
    // markdown rules and similar punctuation art).
    if run.len() >= 60
        && run
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
    {
        let alnum = run.chars().filter(|c| c.is_ascii_alphanumeric()).count();
        if alnum * 4 >= run.len() * 3 {
            return true;
        }
    }
    false
}

/// Render `agents/<slug>/AGENTS.md`. `reports_to` must already be filtered to
/// the export set; `skills` are final shortnames. Deterministic field order.
pub(crate) fn render_agents_md(
    a: &ExportedAgent,
    reports_to: Option<&str>,
    skills: &[String],
    has_contract_doc: bool,
) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "---");
    let _ = writeln!(md, "schema: {SCHEMA}");
    let _ = writeln!(md, "kind: agent");
    let _ = writeln!(md, "slug: {}", a.slug);
    let _ = writeln!(md, "name: {}", yq(&a.display_name));
    let _ = writeln!(md, "title: {}", yq(&a.role));
    if let Some(rt) = reports_to {
        let _ = writeln!(md, "reportsTo: {rt}");
    }
    if !skills.is_empty() {
        let _ = writeln!(md, "skills:");
        for s in skills {
            let _ = writeln!(md, "  - {s}");
        }
    }
    if has_contract_doc {
        let _ = writeln!(md, "docs:");
        let _ = writeln!(md, "  - docs/contract.md");
    }
    let _ = writeln!(md, "metadata:");
    let _ = writeln!(md, "  duduclaw:");
    if !a.icon.is_empty() {
        let _ = writeln!(md, "    icon: {}", yq(&a.icon));
    }
    if !a.trigger.is_empty() {
        let _ = writeln!(md, "    trigger: {}", yq(&a.trigger));
    }
    if !a.model.is_empty() {
        let _ = writeln!(md, "    model: {}", yq(&a.model));
    }
    let _ = writeln!(md, "---");
    // Body = SOUL.md verbatim (spec: body is the agent's default
    // instructions). No contract/CLAUDE.md text is mixed in, so a re-import
    // reproduces SOUL.md byte-stable (modulo one trailing newline).
    if let Some(soul) = &a.soul {
        md.push_str(soul);
        if !soul.ends_with('\n') {
            md.push('\n');
        }
    }
    md
}

/// Render `agents/<slug>/docs/contract.md` from a CONTRACT.toml summary.
pub(crate) fn render_contract_doc(c: &ContractSummary) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "# Behavioral contract");
    let _ = writeln!(md);
    let _ = writeln!(
        md,
        "Summarized from the agent's DuDuClaw `CONTRACT.toml` boundaries."
    );
    if !c.must_not.is_empty() {
        let _ = writeln!(md);
        let _ = writeln!(md, "## Must not");
        let _ = writeln!(md);
        for b in &c.must_not {
            let _ = writeln!(md, "- {b}");
        }
    }
    if !c.must_always.is_empty() {
        let _ = writeln!(md);
        let _ = writeln!(md, "## Must always");
        let _ = writeln!(md);
        for b in &c.must_always {
            let _ = writeln!(md, "- {b}");
        }
    }
    md
}

/// Render the package root `COMPANY.md` (spec-required name/description/slug
/// plus the explicit no-secrets manifest note).
pub(crate) fn render_company_md(agents: &[(String, String, String, Option<String>)]) -> String {
    // tuples: (slug, display_name, role, reports_to)
    let mut md = String::new();
    let _ = writeln!(md, "---");
    let _ = writeln!(md, "schema: {SCHEMA}");
    let _ = writeln!(md, "kind: company");
    let _ = writeln!(md, "slug: {COMPANY_SLUG}");
    let _ = writeln!(md, "name: {}", yq("DuDuClaw agents"));
    let _ = writeln!(
        md,
        "description: {}",
        yq("Agent company package exported from DuDuClaw")
    );
    let _ = writeln!(md, "---");
    let _ = writeln!(md, "# DuDuClaw agents");
    let _ = writeln!(md);
    let _ = writeln!(
        md,
        "This package was exported from DuDuClaw in the agentcompanies format."
    );
    let _ = writeln!(md);
    let _ = writeln!(md, "## Agents");
    let _ = writeln!(md);
    for (slug, name, role, rt) in agents {
        match rt {
            Some(parent) => {
                let _ = writeln!(md, "- `{slug}` — {name} ({role}), reports to `{parent}`");
            }
            None => {
                let _ = writeln!(md, "- `{slug}` — {name} ({role})");
            }
        }
    }
    let _ = writeln!(md);
    let _ = writeln!(md, "## Excluded secrets");
    let _ = writeln!(md);
    let _ = writeln!(
        md,
        "Per the agentcompanies export rules, this package contains no secret\n\
         values: channel tokens, API keys, OAuth credentials, machine-local\n\
         identifiers and machine-specific paths were all excluded at export\n\
         time. Importers must provision their own runtime credentials."
    );
    md
}

/// Render the `.paperclip.yaml` vendor sidecar (adapter wiring so a paperclip
/// host can run these agents through `@duduclaw/paperclip-adapter`).
pub(crate) fn render_sidecar(agents: &[(String, String)]) -> String {
    // tuples: (slug, model)
    let mut y = String::new();
    let _ = writeln!(y, "schema: paperclip/v1");
    let _ = writeln!(y, "agents:");
    for (slug, model) in agents {
        let _ = writeln!(y, "  {slug}:");
        let _ = writeln!(y, "    adapter:");
        let _ = writeln!(y, "      type: duduclaw");
        let _ = writeln!(y, "      config:");
        let _ = writeln!(y, "        agentId: {}", yq(slug));
        if !model.is_empty() {
            let _ = writeln!(y, "        model: {}", yq(model));
        }
    }
    y
}

// ─────────────────────────── Package writer ───────────────────────────

/// Recursively copy a directory tree (local twin of the migrate-from helper,
/// which is module-private there).
///
/// Symlinks are NEVER followed (`symlink_metadata`, not `metadata`): a link
/// inside a skill dir pointing at `~/.ssh/id_rsa` or `config.toml` must not
/// smuggle that content into the exported package. Skipped links are
/// collected so the caller can surface them (PARTIAL + manifest note).
fn copy_dir_recursive(
    src: &Path,
    dest: &Path,
    skipped_symlinks: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            skipped_symlinks.push(path);
            continue;
        }
        let target = dest.join(entry.file_name());
        if meta.is_dir() {
            copy_dir_recursive(&path, &target, skipped_symlinks)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// Recursively scrub every UTF-8 text file under `dir` with
/// [`redact_secrets`] — scripts, references, samples, not just SKILL.md.
/// Non-UTF-8 (binary) files are left untouched. Returns total redactions.
fn scrub_tree(dir: &Path) -> std::io::Result<usize> {
    let mut hits = 0usize;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            hits += scrub_tree(&path)?;
        } else if meta.is_file() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let (scrubbed, n) = redact_secrets(&text);
                if n > 0 {
                    std::fs::write(&path, scrubbed)?;
                    hits += n;
                }
            }
        }
    }
    Ok(hits)
}

fn write_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            DuDuClawError::Io(std::io::Error::other(format!(
                "建立 {} 失敗: {e}",
                parent.display()
            )))
        })?;
    }
    std::fs::write(path, content).map_err(|e| {
        DuDuClawError::Io(std::io::Error::other(format!(
            "寫入 {} 失敗: {e}",
            path.display()
        )))
    })
}

/// List agent ids under `<home>/agents/` (sorted, hidden/underscore skipped).
fn list_agent_ids(home: &Path) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(home.join("agents"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('_') && !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// Export `selection` (None = all agents) into `out` as an agentcompanies
/// package. Deterministic; never reads config.toml or any secret store.
pub(crate) fn export_package(
    home: &Path,
    selection: Option<&str>,
    out: &Path,
) -> Result<ExportReport> {
    let mut report = ExportReport::new(out);

    let ids = match selection {
        Some(id) => {
            let id = id.trim();
            if !crate::is_valid_agent_id(id) {
                return Err(DuDuClawError::Config(format!(
                    "無效的 agent id '{id}'（僅允許小寫英數與連字號）"
                )));
            }
            if !home.join("agents").join(id).is_dir() {
                return Err(DuDuClawError::Config(format!(
                    "找不到 agent '{id}'（{} 下無此目錄）",
                    home.join("agents").display()
                )));
            }
            vec![id.to_string()]
        }
        None => {
            let all = list_agent_ids(home);
            if all.is_empty() {
                return Err(DuDuClawError::Config(format!(
                    "{} 下沒有任何 agent 可匯出",
                    home.join("agents").display()
                )));
            }
            all
        }
    };

    // Read every agent in the set.
    let mut agents: Vec<ExportedAgent> = Vec::new();
    for id in &ids {
        match read_agent(home, id) {
            Some(a) => agents.push(a),
            None => report.skipped("agent", id, "agent.toml 缺失或無法解析"),
        }
    }
    if agents.is_empty() {
        report.note("沒有任何 agent 匯出成功。");
        return Ok(report);
    }

    let in_set: std::collections::BTreeSet<&str> = agents.iter().map(|a| a.slug.as_str()).collect();

    // ── Skill dedup: shortname → (owner slug, source dir) ──
    // First (sorted) owner claims the plain shortname; a later agent with a
    // byte-identical SKILL.md shares it, a divergent one gets `--<slug>`.
    let mut skill_sources: BTreeMap<String, (String, PathBuf, Vec<u8>)> = BTreeMap::new();
    let mut agent_skills: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for a in &agents {
        let mut names: Vec<String> = Vec::new();
        for (name, dir) in &a.skill_dirs {
            let bytes = match std::fs::read(dir.join("SKILL.md")) {
                Ok(b) => b,
                Err(e) => {
                    report.skipped("skill", name, format!("讀取 SKILL.md 失敗: {e}"));
                    continue;
                }
            };
            let shortname = match skill_sources.get(name) {
                None => {
                    skill_sources.insert(name.clone(), (a.slug.clone(), dir.clone(), bytes));
                    name.clone()
                }
                Some((_, _, existing)) if *existing == bytes => name.clone(),
                Some(_) => {
                    let alt = format!("{name}--{}", a.slug);
                    skill_sources.entry(alt.clone()).or_insert((
                        a.slug.clone(),
                        dir.clone(),
                        bytes,
                    ));
                    report.partial(
                        "skill",
                        name,
                        format!("與其他 agent 的同名 skill 內容不同，改以 {alt} 匯出"),
                    );
                    alt
                }
            };
            names.push(shortname);
        }
        names.sort();
        names.dedup();
        agent_skills.insert(a.slug.clone(), names);
    }

    // ── agents/<slug>/AGENTS.md (+ docs/contract.md) ──
    for a in &agents {
        let reports_to = {
            let rt = a.reports_to_raw.trim();
            if !rt.is_empty() && in_set.contains(rt) && rt != a.slug {
                Some(rt)
            } else {
                if !rt.is_empty() && !in_set.contains(rt) {
                    report.partial(
                        "agent",
                        &a.slug,
                        format!("reports_to '{rt}' 不在匯出集合內，已省略 reportsTo"),
                    );
                }
                None
            }
        };
        let skills = agent_skills.get(&a.slug).cloned().unwrap_or_default();
        let rendered = render_agents_md(a, reports_to, &skills, a.contract.is_some());
        let (scrubbed, hits) = redact_secrets(&rendered);
        write_file(
            &out.join("agents").join(&a.slug).join("AGENTS.md"),
            &scrubbed,
        )?;
        if hits > 0 {
            report.partial(
                "agent",
                &a.slug,
                format!("SOUL.md 內容命中 {hits} 個疑似機密樣式，已以 {REDACTED} 取代"),
            );
        } else {
            report.exported("agent", &a.slug);
        }
        if let Some(c) = &a.contract {
            // Contract boundaries are operator-authored free text — scrub
            // them like every other emitted text file.
            let (contract_md, contract_hits) = redact_secrets(&render_contract_doc(c));
            write_file(
                &out.join("agents")
                    .join(&a.slug)
                    .join("docs")
                    .join("contract.md"),
                &contract_md,
            )?;
            if contract_hits > 0 {
                report.partial(
                    "contract",
                    &a.slug,
                    format!("CONTRACT 內容命中 {contract_hits} 個疑似機密樣式，已以 {REDACTED} 取代"),
                );
            } else {
                report.exported("contract", &a.slug);
            }
        }
        if a.soul.is_none() {
            report.partial("agent-soul", &a.slug, "無 SOUL.md，AGENTS.md body 為空");
        }
    }

    // ── skills/<shortname>/ (full dir copy, symlink-safe, ALL text files
    // scrubbed — scripts and reference docs leak secrets just as well as
    // SKILL.md does) ──
    for (shortname, (_owner, dir, _bytes)) in &skill_sources {
        let dest = out.join("skills").join(shortname);
        let mut skipped_symlinks: Vec<PathBuf> = Vec::new();
        if let Err(e) = copy_dir_recursive(dir, &dest, &mut skipped_symlinks) {
            report.skipped("skill", shortname, format!("複製失敗: {e}"));
            continue;
        }
        let hits = match scrub_tree(&dest) {
            Ok(h) => h,
            Err(e) => {
                // Fail-closed: if we cannot verify the copy is clean, the
                // item is at best PARTIAL — never silently EXPORTED.
                report.partial("skill", shortname, format!("機密掃描未完成: {e}"));
                continue;
            }
        };
        let mut reasons: Vec<String> = Vec::new();
        if hits > 0 {
            reasons.push(format!("命中 {hits} 個疑似機密樣式，已以 {REDACTED} 取代"));
        }
        if !skipped_symlinks.is_empty() {
            reasons.push(format!("略過 {} 個符號連結（不隨匯出複製）", skipped_symlinks.len()));
            let names: Vec<String> = skipped_symlinks
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect();
            report.note(format!(
                "skill '{shortname}'：符號連結不隨匯出複製（僅記錄名稱）：{}",
                names.join(", ")
            ));
        }
        if reasons.is_empty() {
            report.exported("skill", shortname);
        } else {
            report.partial("skill", shortname, reasons.join("；"));
        }
    }

    // ── COMPANY.md + .paperclip.yaml ──
    let roster: Vec<(String, String, String, Option<String>)> = agents
        .iter()
        .map(|a| {
            let rt = a.reports_to_raw.trim();
            let rt = if !rt.is_empty() && in_set.contains(rt) && rt != a.slug {
                Some(rt.to_string())
            } else {
                None
            };
            (a.slug.clone(), a.display_name.clone(), a.role.clone(), rt)
        })
        .collect();
    write_file(&out.join("COMPANY.md"), &render_company_md(&roster))?;
    report.exported("company", "COMPANY.md");

    let sidecar_agents: Vec<(String, String)> = agents
        .iter()
        .map(|a| (a.slug.clone(), a.model.clone()))
        .collect();
    write_file(
        &out.join(".paperclip.yaml"),
        &render_sidecar(&sidecar_agents),
    )?;
    report.exported("sidecar", ".paperclip.yaml");

    report.note("機密（channel tokens / API keys / OAuth）一律未匯出；COMPANY.md 已載明排除說明。");
    report.note("CLAUDE.md 為 DuDuClaw 執行期產生的樣板，未匯出（匯入端會重新生成）。");
    report.note("reports_to 階層以 AGENTS.md 的 reportsTo 欄位表達（agentcompanies/v1）。");
    Ok(report)
}

/// CLI entry for `duduclaw export --format agentcompanies`.
pub(crate) async fn run(
    agent: Option<String>,
    all: bool,
    out: Option<PathBuf>,
    json: bool,
) -> Result<()> {
    if agent.is_none() && !all {
        return Err(DuDuClawError::Config(
            "請指定 --agent <id>（單一）或 --all（全部）".to_string(),
        ));
    }
    if agent.is_some() && all {
        return Err(DuDuClawError::Config(
            "--agent 與 --all 不可同時使用".to_string(),
        ));
    }
    let home = crate::duduclaw_home();
    let out = out.unwrap_or_else(|| PathBuf::from("duduclaw-agentcompanies"));
    let report = export_package(&home, agent.as_deref(), &out)?;
    if json {
        // Exactly one JSON object on stdout — logs stay on stderr.
        println!("{}", serde_json::to_string(&report.to_json())?);
    } else {
        report.render_console();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_fixture() -> ExportedAgent {
        ExportedAgent {
            slug: "worker".into(),
            display_name: "Worker \"W\"".into(),
            role: "specialist".into(),
            reports_to_raw: "boss".into(),
            icon: "🤖".into(),
            trigger: "@Worker".into(),
            model: "claude-sonnet-4-6".into(),
            soul: Some("# Worker\n\nI am the worker.\n".into()),
            contract: None,
            skill_dirs: vec![],
        }
    }

    #[test]
    fn yq_escapes_quotes_and_keeps_cjk() {
        assert_eq!(yq("a\"b"), "\"a\\\"b\"");
        assert_eq!(yq("嘟嘟🐾"), "\"嘟嘟🐾\"");
        assert_eq!(yq("line\nbreak"), "\"line\\nbreak\"");
    }

    #[test]
    fn agents_md_is_deterministic_and_body_verbatim() {
        let a = agent_fixture();
        let one = render_agents_md(&a, Some("boss"), &["hello".into()], false);
        let two = render_agents_md(&a, Some("boss"), &["hello".into()], false);
        assert_eq!(one, two, "repeated renders must be byte-identical");
        assert!(one.starts_with("---\nschema: agentcompanies/v1\nkind: agent\n"));
        assert!(one.contains("reportsTo: boss\n"));
        assert!(one.contains("skills:\n  - hello\n"));
        assert!(one.ends_with("---\n# Worker\n\nI am the worker.\n"));
    }

    #[test]
    fn agents_md_omits_reports_to_when_outside_set() {
        let a = agent_fixture();
        let md = render_agents_md(&a, None, &[], false);
        assert!(!md.contains("reportsTo"));
        assert!(!md.contains("skills:"));
    }

    #[test]
    fn redact_known_secret_prefixes() {
        let (out, n) = redact_secrets("key sk-ant-abc123def456ghi and xoxb-1234567890-abcdef");
        assert_eq!(n, 2);
        assert!(!out.contains("sk-ant-abc123def456ghi"));
        assert!(!out.contains("xoxb-1234567890-abcdef"));
        assert!(out.contains(REDACTED));
        // Telegram bot token shape.
        let (out2, n2) = redact_secrets("token 123456789:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA-1234 end");
        assert_eq!(n2, 1);
        assert!(!out2.contains(":AAAAAAAA"));
        // Short mentions of the prefix alone stay untouched.
        let (out3, n3) = redact_secrets("the sk-ant- prefix and version 1:2 pairs");
        assert_eq!(n3, 0);
        assert_eq!(out3, "the sk-ant- prefix and version 1:2 pairs");
    }

    #[test]
    fn redact_extended_token_shapes() {
        // Discord bot token (3 dot-joined base64ish segments).
        let (out, n) =
            redact_secrets("bot MTIzNDU2Nzg5MDEyMzQ1Njc4OTA.GaBcDe.abcdefghijklmnopqrstuvwxyz12 x");
        assert_eq!(n, 1, "{out}");
        assert!(out.contains(REDACTED));
        assert!(!out.contains(".GaBcDe."));

        // Groq / OpenRouter / WhatsApp prefixes.
        let (out, n) = redact_secrets(
            "a gsk_abcdefghijklmnop b sk-or-v1-abcdef1234567890 c EAAGm0PX4ZCpsBAKZAZBZBZBZBZB123456 d",
        );
        assert_eq!(n, 3, "{out}");

        // AWS secret heuristic: 40 chars, mixed case + digit.
        let (out, n) = redact_secrets("secret=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY end");
        assert_eq!(n, 1, "{out}");
        assert!(!out.contains("wJalrXUtnFEMI"));
        // 40-char lowercase hex (a git SHA-1-ish run) must NOT trip it.
        let sha = "d3b07384d113edec49eaa6238ad5ff00aabbccdd";
        let (out, n) = redact_secrets(&format!("commit {sha} ok"));
        assert_eq!(n, 0, "{out}");

        // Long base64 run (LINE token / `_enc` ciphertext shape).
        let long = format!("{}==", "Ab3dEf6hIj9kLm2nOp5qRs8tUv1wXy4zAb3dEf6hIj9kLm2nOp5qRs8tUv1w");
        assert!(long.len() >= 60);
        let (out, n) = redact_secrets(&format!("enc: {long}\n"));
        assert_eq!(n, 1, "{out}");
        // Markdown rules of `=`/`-` must NOT trip the long-run check.
        let (out, n) = redact_secrets(&format!("{}\n{}\n", "=".repeat(70), "-".repeat(70)));
        assert_eq!(n, 0, "{out}");

        // Ordinary URLs stay intact (dots break the long-run class).
        let url = "https://example.com/very/long/path/to/some/skill/reference/document.html";
        let (out, n) = redact_secrets(url);
        assert_eq!(n, 0);
        assert_eq!(out, url);
    }

    #[cfg(unix)]
    #[test]
    fn export_skips_symlinks_and_reports_partial() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let dir = home.join("agents").join("worker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            "[agent]\nname = \"worker\"\ndisplay_name = \"Worker\"\nrole = \"specialist\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("SOUL.md"), "# Worker\n").unwrap();

        // Skill dir with a symlink pointing at a secret OUTSIDE the dir.
        let secret = home.join("config.toml");
        std::fs::write(&secret, "api_key = \"sk-ant-abc123def456ghi789jkl\"").unwrap();
        let skill = dir.join("SKILLS").join("helper");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: helper\n---\nbody").unwrap();
        std::os::unix::fs::symlink(&secret, skill.join("stolen.toml")).unwrap();

        let out = tmp.path().join("pkg");
        let report = export_package(&home, None, &out).unwrap();

        assert!(
            !out.join("skills/helper/stolen.toml").exists(),
            "symlink target must NOT be copied into the package"
        );
        let item = report
            .items
            .iter()
            .find(|i| i.kind == "skill" && i.name == "helper")
            .expect("skill item present");
        assert!(
            matches!(&item.status, ExportStatus::Partial(r) if r.contains("符號連結")),
            "symlink skip must surface as PARTIAL: {:?}",
            item.status
        );
        assert!(
            report.notes.iter().any(|n| n.contains("stolen.toml")),
            "manifest note must name the skipped link"
        );
    }

    #[test]
    fn export_scrubs_non_skill_md_text_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let dir = home.join("agents").join("worker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            "[agent]\nname = \"worker\"\ndisplay_name = \"Worker\"\nrole = \"specialist\"\n",
        )
        .unwrap();
        let skill = dir.join("SKILLS").join("helper");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: helper\n---\nclean").unwrap();
        // Secret hides in a sidecar script, not SKILL.md.
        std::fs::write(
            skill.join("run.sh"),
            "#!/bin/sh\nexport TOKEN=xoxb-1234567890-abcdefghijk\n",
        )
        .unwrap();

        let out = tmp.path().join("pkg");
        let report = export_package(&home, None, &out).unwrap();
        let script = std::fs::read_to_string(out.join("skills/helper/run.sh")).unwrap();
        assert!(!script.contains("xoxb-1234567890"), "sidecar must be scrubbed");
        assert!(script.contains(REDACTED));
        assert!(
            report
                .items
                .iter()
                .any(|i| i.kind == "skill" && matches!(i.status, ExportStatus::Partial(_))),
            "redaction in a sidecar must surface as PARTIAL"
        );
    }

    #[test]
    fn company_md_carries_secret_exclusion_note() {
        let md = render_company_md(&[("boss".into(), "Boss".into(), "main".into(), None)]);
        assert!(md.contains("schema: agentcompanies/v1"));
        assert!(md.contains("kind: company"));
        assert!(md.contains("## Excluded secrets"));
        assert!(md.contains("- `boss` — Boss (main)"));
    }

    #[test]
    fn export_package_redacts_secret_in_soul_and_reports_partial() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let dir = home.join("agents").join("leaky");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            "[agent]\nname = \"leaky\"\ndisplay_name = \"Leaky\"\nrole = \"specialist\"\n\
             status = \"active\"\ntrigger = \"@Leaky\"\nreports_to = \"\"\nicon = \"🤖\"\n\n\
             [model]\npreferred = \"claude-sonnet-4-6\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("SOUL.md"),
            "# Leaky\nMy key is sk-ant-abc123def456ghi789.\n",
        )
        .unwrap();

        let out = tmp.path().join("pkg");
        let report = export_package(&home, None, &out).unwrap();
        let md = std::fs::read_to_string(out.join("agents/leaky/AGENTS.md")).unwrap();
        assert!(
            !md.contains("sk-ant-abc123def456ghi789"),
            "secret must never reach the exported package"
        );
        assert!(md.contains(REDACTED));
        assert!(
            report
                .items
                .iter()
                .any(|i| matches!(i.status, ExportStatus::Partial(_))),
            "redaction must be surfaced as PARTIAL, never silent"
        );
        assert_eq!(report.overall(), "DEGRADED");
    }

    #[test]
    fn export_package_unknown_agent_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join("agents")).unwrap();
        let out = tmp.path().join("pkg");
        assert!(export_package(&home, Some("ghost"), &out).is_err());
        assert!(export_package(&home, Some("BAD ID!"), &out).is_err());
        // No agents at all → clear error, not an empty package.
        assert!(export_package(&home, None, &out).is_err());
    }

    #[test]
    fn sidecar_shape_matches_paperclip_v1() {
        let y = render_sidecar(&[("boss".into(), "claude-sonnet-4-6".into())]);
        assert!(y.starts_with("schema: paperclip/v1\nagents:\n"));
        assert!(y.contains("  boss:\n    adapter:\n      type: duduclaw\n"));
        assert!(y.contains("agentId: \"boss\""));
    }
}
