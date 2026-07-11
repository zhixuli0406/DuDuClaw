//! Dynamic runtime model discovery.
//!
//! The dashboard model picker used to be backed by a **hard-coded, hand-edited**
//! cloud model list in `handle_models_list` — it drifted out of date the moment
//! a provider shipped a new model (e.g. Claude "opus-4-6" long after Fable 5
//! became the default). This module replaces that with a per-provider discovery
//! chain that probes the *actual* installed CLIs / APIs for their real available
//! models, caches the result to `~/.duduclaw/runtime_models.json`, and refreshes
//! it on a background 12-hour interval.
//!
//! ## Honesty contract
//!
//! Every provider result carries a [`DiscoverySource`]. When live discovery
//! fails we fall back to a small static list **and mark it `Fallback`** — the UI
//! surfaces "預設清單，未能即時取得" so an operator can never mistake a stale
//! guess for a live registry. We never fabricate a live-looking list.
//!
//! ## Probe safety
//!
//! `claude models` is NOT a subcommand — invoking it drops into the interactive
//! REPL. Every CLI probe therefore closes stdin (`Stdio::null()`), sets a hard
//! timeout (≤5s for `--help`, ≤10s for the HTTP API), and uses
//! `kill_on_drop(true)` so a hung probe is reaped rather than leaked. The
//! optional `pty_probe` source (default OFF) is the only path that intentionally
//! drives an interactive session, and it is fully bounded + killed.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Background refresh cadence: every 12 hours.
pub const REFRESH_INTERVAL_SECS: u64 = 12 * 3600;

const CACHE_FILE: &str = "runtime_models.json";
const HELP_TIMEOUT: Duration = Duration::from_secs(5);
const API_TIMEOUT: Duration = Duration::from_secs(10);
const ANTHROPIC_MODELS_URL: &str = "https://api.anthropic.com/v1/models";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// One discoverable model, provider-tagged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeModel {
    pub id: String,
    pub label: String,
    /// `claude` / `codex` / `gemini` — matches the UI grouping the old static
    /// list used.
    pub provider: String,
}

/// Where a provider's list came from. Serialized as snake_case so it lands in
/// the cache file + RPC payload verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoverySource {
    /// Live provider HTTP API (`GET /v1/models`).
    LiveApi,
    /// A CLI subcommand that lists models (e.g. `codex models`).
    CliProbe,
    /// Parsed from `<cli> --help` (aliases only — best effort).
    HelpParse,
    /// Driven the interactive REPL `/model` menu (opt-in, default off).
    PtyProbe,
    /// Live discovery failed — this is a static, possibly-stale list.
    Fallback,
}

impl DiscoverySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            DiscoverySource::LiveApi => "live_api",
            DiscoverySource::CliProbe => "cli_probe",
            DiscoverySource::HelpParse => "help_parse",
            DiscoverySource::PtyProbe => "pty_probe",
            DiscoverySource::Fallback => "fallback",
        }
    }
}

/// The discovery outcome for a single provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderModels {
    pub models: Vec<RuntimeModel>,
    pub source: DiscoverySource,
    /// RFC3339 timestamp of when this provider was last (successfully or not)
    /// probed.
    pub fetched_at: String,
}

/// The full cache written to `~/.duduclaw/runtime_models.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeModelsCache {
    /// Keyed by cache key (`claude` / `codex` / `gemini` / `agy`).
    pub providers: BTreeMap<String, ProviderModels>,
    pub fetched_at: String,
}

// ── Static fallbacks ─────────────────────────────────────────────────────

fn m(id: &str, label: &str, provider: &str) -> RuntimeModel {
    RuntimeModel { id: id.into(), label: label.into(), provider: provider.into() }
}

/// Claude static fallback — the three ids the old hard-coded list carried.
/// Deliberately conservative; only used when every live source fails.
pub fn claude_fallback() -> Vec<RuntimeModel> {
    vec![
        m("claude-opus-4-6", "Claude Opus 4.6", "claude"),
        m("claude-sonnet-4-6", "Claude Sonnet 4.6", "claude"),
        m("claude-haiku-4-5", "Claude Haiku 4.5", "claude"),
    ]
}

/// Codex has NO model-listing command (openai/codex#8871 closed "not planned"),
/// so this fallback IS the codex list in practice. Ids per the official model
/// docs (developers.openai.com/codex/models, checked 2026-07-11).
fn codex_fallback() -> Vec<RuntimeModel> {
    vec![
        m("gpt-5.6-sol", "GPT-5.6 Sol", "codex"),
        m("gpt-5.6-terra", "GPT-5.6 Terra", "codex"),
        m("gpt-5.6-luna", "GPT-5.6 Luna", "codex"),
        m("gpt-5.5", "GPT-5.5", "codex"),
        m("gpt-5.4", "GPT-5.4", "codex"),
        m("gpt-5.4-mini", "GPT-5.4 mini", "codex"),
        m("gpt-5.3-codex-spark", "GPT-5.3 Codex Spark", "codex"),
    ]
}

/// Gemini CLI only lists models via the interactive `/model` dialog — no
/// non-interactive listing exists — so this fallback IS the gemini list in
/// practice. Ids per the official `-m/--model` docs
/// (google-gemini/gemini-cli docs/cli/model.md, checked 2026-07-11).
fn gemini_fallback() -> Vec<RuntimeModel> {
    vec![
        m("gemini-3-pro-preview", "Gemini 3 Pro", "gemini"),
        m("gemini-3-flash-preview", "Gemini 3 Flash", "gemini"),
        m("gemini-2.5-pro", "Gemini 2.5 Pro", "gemini"),
        m("gemini-2.5-flash", "Gemini 2.5 Flash", "gemini"),
    ]
}

/// agy fallback — only used if the `agy models` probe fails. NOTE: agy's
/// `--model` takes the *display name* verbatim (official codelab:
/// `agy --model "Gemini 3.5 Flash (Low)"`), so ids here are display names.
fn agy_fallback() -> Vec<RuntimeModel> {
    vec![
        m("Gemini 3.1 Pro (High)", "Gemini 3.1 Pro (High)", "gemini"),
        m("Gemini 3.5 Flash (Medium)", "Gemini 3.5 Flash (Medium)", "gemini"),
    ]
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ── Public entry points ──────────────────────────────────────────────────

/// Run the full discovery for every installed provider. Never fails: any
/// provider that can't be probed live falls back to a marked static list.
pub async fn discover_all(home_dir: &Path) -> RuntimeModelsCache {
    let mut providers = BTreeMap::new();

    providers.insert("claude".to_string(), discover_claude(home_dir).await);

    // NOTE: `home_dir` here is the DuDuClaw home (~/.duduclaw), NOT the user
    // home — `which_*_in_home(home_dir)` alone would probe
    // `~/.duduclaw/.local/bin/<cli>` and never find anything. PATH/real-HOME
    // resolution (`which_*()`) comes first, mirroring the claude fix.
    if let Some(bin) =
        duduclaw_core::which_codex().or_else(|| duduclaw_core::which_codex_in_home(home_dir))
    {
        providers.insert(
            "codex".to_string(),
            discover_generic("codex", &bin, codex_fallback()).await,
        );
    }
    if let Some(bin) =
        duduclaw_core::which_gemini().or_else(|| duduclaw_core::which_gemini_in_home(home_dir))
    {
        providers.insert(
            "gemini".to_string(),
            discover_generic("gemini", &bin, gemini_fallback()).await,
        );
    }
    if let Some(bin) =
        duduclaw_core::which_agy().or_else(|| duduclaw_core::which_agy_in_home(home_dir))
    {
        // `agy` runs Gemini models — group it under the "gemini" provider label
        // (matching the old UI) but keep a distinct cache key. agy has its own
        // parser: `agy models` prints display names ("Gemini 3.5 Flash (Low)")
        // and `--model` accepts exactly those strings, so lines pass verbatim.
        providers.insert("agy".to_string(), discover_agy(&bin).await);
    }

    RuntimeModelsCache { providers, fetched_at: now_rfc3339() }
}

/// Load the cached discovery result. Returns `None` when there's no cache yet.
pub async fn load_cache(home_dir: &Path) -> Option<RuntimeModelsCache> {
    let path = home_dir.join(CACHE_FILE);
    let content = tokio::fs::read_to_string(&path).await.ok()?;
    serde_json::from_str(&content).ok()
}

/// Persist the cache atomically (temp + rename).
pub async fn save_cache(home_dir: &Path, cache: &RuntimeModelsCache) -> std::io::Result<()> {
    let path = home_dir.join(CACHE_FILE);
    let tmp = home_dir.join(format!("{CACHE_FILE}.tmp"));
    let json = serde_json::to_string_pretty(cache)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    tokio::fs::write(&tmp, json).await?;
    tokio::fs::rename(&tmp, &path).await
}

/// Discover + persist in one shot. Used by the `models.refresh` RPC and the
/// background task. On discovery success the cache is overwritten; on save
/// failure the old cache is preserved and the error logged.
pub async fn refresh_and_save(home_dir: &Path) -> RuntimeModelsCache {
    let cache = discover_all(home_dir).await;
    if let Err(e) = save_cache(home_dir, &cache).await {
        warn!("runtime_models: failed to persist cache: {e}");
    } else {
        let sources: Vec<String> = cache
            .providers
            .iter()
            .map(|(k, v)| format!("{k}={}", v.source.as_str()))
            .collect();
        info!("runtime_models: refreshed [{}]", sources.join(", "));
    }
    cache
}

/// Return the cache, discovering + persisting on first call if none exists.
pub async fn load_or_refresh(home_dir: &Path) -> RuntimeModelsCache {
    if let Some(cache) = load_cache(home_dir).await {
        return cache;
    }
    refresh_and_save(home_dir).await
}

/// Spawn the background refresh loop: one immediate refresh at startup, then
/// every [`REFRESH_INTERVAL_SECS`]. On failure the previous cache is kept.
pub fn spawn_periodic_refresh(home_dir: std::path::PathBuf) {
    tokio::spawn(async move {
        // Startup refresh — populate/refresh the cache once the gateway is up.
        refresh_and_save(&home_dir).await;
        let mut interval =
            tokio::time::interval(Duration::from_secs(REFRESH_INTERVAL_SECS));
        interval.tick().await; // discard the immediate first tick
        loop {
            interval.tick().await;
            refresh_and_save(&home_dir).await;
        }
    });
}

// ── Per-provider discovery ───────────────────────────────────────────────

async fn discover_claude(home_dir: &Path) -> ProviderModels {
    let fetched_at = now_rfc3339();

    // ① Live API when an Anthropic API key is configured.
    if let Some(key) = resolve_anthropic_key(home_dir).await {
        match probe_anthropic_api(&key).await {
            Some(models) if !models.is_empty() => {
                return ProviderModels { models, source: DiscoverySource::LiveApi, fetched_at };
            }
            _ => debug!("runtime_models: anthropic /v1/models probe yielded nothing"),
        }
    }

    // ①.5 Optional PTY probe (default OFF; opt-in via `[models] pty_probe`).
    // Only used when there's no API key and help parsing is the last resort —
    // it costs one interactive session, so it stays behind the flag.
    if pty_probe_enabled(home_dir).await {
        match probe_claude_pty(home_dir).await {
            Some(models) if !models.is_empty() => {
                return ProviderModels { models, source: DiscoverySource::PtyProbe, fetched_at };
            }
            _ => debug!("runtime_models: pty /model probe yielded nothing"),
        }
    }

    // ② Parse `claude --help` for the --model aliases.
    // Version-aware resolution: which_claude() probes every install and
    // prefers the newest (a stale /usr/local/bin copy must not win).
    if let Some(bin) = duduclaw_core::which_claude()
        .or_else(|| duduclaw_core::which_claude_in_home(home_dir))
    {
        if let Some(out) = run_probe(&bin, &["--help"], HELP_TIMEOUT).await {
            let models = parse_claude_help_models(&out);
            if !models.is_empty() {
                return ProviderModels { models, source: DiscoverySource::HelpParse, fetched_at };
            }
        }
    }

    // ③ Static fallback (clearly marked).
    ProviderModels {
        models: claude_fallback(),
        source: DiscoverySource::Fallback,
        fetched_at,
    }
}

/// Generic codex/gemini/agy discovery: best-effort CLI probe, else fallback.
async fn discover_generic(provider: &str, bin: &str, fallback: Vec<RuntimeModel>) -> ProviderModels {
    let fetched_at = now_rfc3339();

    if let Some(help) = run_probe(bin, &["--help"], HELP_TIMEOUT).await {
        // Only attempt a `models` subcommand if --help actually advertises one,
        // so we don't blindly run an unknown subcommand that might block.
        if word_present(&help, "models") {
            for args in [["models", "list"].as_slice(), ["models"].as_slice()] {
                if let Some(out) = run_probe(bin, args, HELP_TIMEOUT).await {
                    let models = parse_generic_model_lines(provider, &out);
                    if !models.is_empty() {
                        return ProviderModels {
                            models,
                            source: DiscoverySource::CliProbe,
                            fetched_at,
                        };
                    }
                }
            }
        }
    }

    ProviderModels { models: fallback, source: DiscoverySource::Fallback, fetched_at }
}

/// agy discovery: `agy models` is an official subcommand (antigravity.google
/// codelab) whose output lines are the exact strings `--model` accepts —
/// display names with spaces/parens, e.g. `Claude Sonnet 4.6 (Thinking)`.
/// The generic token parser would reject those as prose, hence this path.
async fn discover_agy(bin: &str) -> ProviderModels {
    let fetched_at = now_rfc3339();
    if let Some(out) = run_probe(bin, &["models"], HELP_TIMEOUT).await {
        let models = parse_agy_model_lines(&out);
        if !models.is_empty() {
            return ProviderModels { models, source: DiscoverySource::CliProbe, fetched_at };
        }
    }
    ProviderModels { models: agy_fallback(), source: DiscoverySource::Fallback, fetched_at }
}

// ── Probes ───────────────────────────────────────────────────────────────

async fn resolve_anthropic_key(home_dir: &Path) -> Option<String> {
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.is_empty() {
            return Some(k);
        }
    }
    crate::config_crypto::read_encrypted_config_field(home_dir, "api", "anthropic_api_key").await
}

async fn pty_probe_enabled(home_dir: &Path) -> bool {
    let path = home_dir.join("config.toml");
    let Ok(content) = tokio::fs::read_to_string(&path).await else {
        return false;
    };
    content
        .parse::<toml::Table>()
        .ok()
        .and_then(|t| t.get("models")?.get("pty_probe")?.as_bool())
        .unwrap_or(false)
}

/// Run a CLI probe safely: stdin closed, hard timeout, killed on drop. Returns
/// combined stdout+stderr on success, `None` on timeout/spawn error.
async fn run_probe(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    use tokio::process::Command;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().ok()?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => {
            let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&out.stderr));
            Some(s)
        }
        Ok(Err(_)) => None,
        Err(_) => {
            // Timed out — the child future is dropped here; kill_on_drop reaps it.
            warn!("runtime_models: probe `{program}` timed out after {timeout:?}");
            None
        }
    }
}

async fn probe_anthropic_api(api_key: &str) -> Option<Vec<RuntimeModel>> {
    let client = reqwest::Client::builder().timeout(API_TIMEOUT).build().ok()?;
    let resp = client
        .get(ANTHROPIC_MODELS_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        debug!("runtime_models: anthropic /v1/models returned {}", resp.status());
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    Some(parse_anthropic_models_response(&json))
}

// ── Parsers (pure — unit tested) ─────────────────────────────────────────

/// Parse `GET /v1/models` → `{ "data": [ { id, display_name }, ... ] }`.
pub fn parse_anthropic_models_response(json: &serde_json::Value) -> Vec<RuntimeModel> {
    let Some(data) = json.get("data").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    data.iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(|v| v.as_str())?;
            if id.is_empty() {
                return None;
            }
            let label = entry
                .get("display_name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| id.to_string());
            Some(m(id, &label, "claude"))
        })
        .collect()
}

/// The claude CLI's stable `--model` family aliases. The `--help` text only
/// shows an "e.g." subset (haiku is routinely omitted), so parsing alone
/// under-reports; `haiku` verified live 2026-07-11 (`claude -p … --model haiku`
/// → ok). Any alias parsed from help gets the full set supplemented.
const CLAUDE_MODEL_ALIASES: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];

/// Extract the `--model` aliases from `claude --help`. The help text lists a few
/// aliases (e.g. `'fable'`, `'opus'`, `'sonnet'`) and an example full name
/// (`'claude-fable-5'`). Aliases found are supplemented to the full known set
/// (the "e.g." list is illustrative), and example full names that merely repeat
/// an alias family (`claude-fable-5` vs `fable`) are dropped as duplicates.
pub fn parse_claude_help_models(help: &str) -> Vec<RuntimeModel> {
    // Narrow to the window after the `--model` flag so we don't grab quoted
    // tokens from unrelated flags.
    let start = match help.find("--model") {
        Some(i) => i,
        None => return Vec::new(),
    };
    let window = &help[start..(start + 600).min(help.len())];

    let mut out: Vec<RuntimeModel> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut alias_found = false;
    for token in extract_single_quoted(window) {
        let t = token.trim();
        // Accept short family aliases and full `claude-*` ids only.
        let is_alias = CLAUDE_MODEL_ALIASES.contains(&t);
        let is_full = t.starts_with("claude-") && t.len() > 7;
        if !(is_alias || is_full) {
            continue;
        }
        // A full-name *example* whose id embeds a family word (claude-fable-5,
        // claude-3-5-sonnet-…) duplicates that family's alias — skip it.
        if is_full
            && t.split('-').any(|seg| CLAUDE_MODEL_ALIASES.contains(&seg))
        {
            continue;
        }
        if !seen.insert(t.to_string()) {
            continue;
        }
        alias_found |= is_alias;
        let label = if is_alias { alias_label(t) } else { t.to_string() };
        out.push(m(t, &label, "claude"));
    }
    // Supplement the aliases help didn't happen to mention — but only when the
    // window really was the alias help text (at least one alias parsed).
    if alias_found {
        for alias in CLAUDE_MODEL_ALIASES {
            if seen.insert(alias.to_string()) {
                out.push(m(alias, &alias_label(alias), "claude"));
            }
        }
    }
    out
}

fn alias_label(alias: &str) -> String {
    let mut c = alias.chars();
    match c.next() {
        Some(f) => format!("{}{}", f.to_uppercase(), c.as_str()),
        None => alias.to_string(),
    }
}

/// Parse the ANSI-mangled interactive `/model` menu into model families. The
/// TUI collapses whitespace, so we key off the family keyword + version number
/// (`Fable 5`, `Sonnet 4.6`, ...) rather than exact columns. Ids are the family
/// aliases, which are valid `--model` values.
pub fn parse_claude_pty_model_menu(raw: &str) -> Vec<RuntimeModel> {
    let clean = strip_ansi_lossy(raw);
    let mut out: Vec<RuntimeModel> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for family in ["Opus", "Sonnet", "Haiku", "Fable"] {
        if let Some(version) = find_family_version(&clean, family) {
            let alias = family.to_lowercase();
            if !seen.insert(alias.clone()) {
                continue;
            }
            out.push(m(&alias, &format!("{family} {version}"), "claude"));
        }
    }
    out
}

/// Parse a generic CLI `models` listing: one model id per line, keeping tokens
/// that look like a model id (`family-version` / dotted). Best effort.
pub fn parse_generic_model_lines(provider: &str, out: &str) -> Vec<RuntimeModel> {
    let clean = strip_ansi_lossy(out);
    let mut models: Vec<RuntimeModel> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in clean.lines() {
        for tok in line.split_whitespace() {
            let t = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            if looks_like_model_id(t) && seen.insert(t.to_string()) {
                models.push(m(t, t, provider));
            }
        }
    }
    models
}

/// Parse `agy models` output: one display name per line, taken verbatim as the
/// model id (that IS what `--model` accepts). A line qualifies when it carries
/// a digit, starts alphabetic, stays within display-name charset
/// (`[A-Za-z0-9 .()-]`), and isn't over-long prose. NOTE: agy silently falls
/// back to its default model on an unrecognized `--model` value (verified live
/// 2026-07-11 — no error), so passing a stale name degrades quietly.
pub fn parse_agy_model_lines(out: &str) -> Vec<RuntimeModel> {
    let clean = strip_ansi_lossy(out);
    let mut models: Vec<RuntimeModel> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in clean.lines() {
        let line = raw.trim();
        if line.len() < 3 || line.len() > 60 {
            continue;
        }
        let ok_chars = line
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '(' | ')' | '-'));
        let has_digit = line.chars().any(|c| c.is_ascii_digit());
        let starts_alpha = line.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
        // Display names are short: "Claude Sonnet 4.6 (Thinking)" = 4 words.
        let word_count = line.split_whitespace().count();
        if ok_chars && has_digit && starts_alpha && word_count <= 6 && seen.insert(line.to_string())
        {
            models.push(m(line, line, "gemini"));
        }
    }
    models
}

// ── Merge / dedup (pure — unit tested) ───────────────────────────────────

/// Flatten a cache's providers into a single deduped list, preserving provider
/// order and dropping duplicate ids (first-seen wins). Each entry keeps its
/// provider + source + fetched_at so the UI can label it.
pub fn merged_models(cache: &RuntimeModelsCache) -> Vec<serde_json::Value> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for pm in cache.providers.values() {
        for model in &pm.models {
            if !seen.insert(model.id.clone()) {
                continue;
            }
            out.push(serde_json::json!({
                "id": model.id,
                "label": model.label,
                "type": "cloud",
                "provider": model.provider,
                "source": pm.source.as_str(),
                "fetched_at": pm.fetched_at,
            }));
        }
    }
    out
}

// ── Small string helpers ─────────────────────────────────────────────────

/// Extract `'token'`-quoted substrings where the content is a single model-id
/// token (`[A-Za-z0-9._-]+`, no whitespace). The token-charset constraint means
/// an apostrophe inside prose (e.g. "model's full name") does NOT pair with a
/// later real quote and swallow a token — a naive `find('\'')` pairing does.
fn extract_single_quoted(s: &str) -> Vec<String> {
    fn is_token_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
    }
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if chars[i] == '\'' {
            let mut j = i + 1;
            while j < n && is_token_char(chars[j]) {
                j += 1;
            }
            // Valid token: at least one token char and an immediate closing quote.
            if j > i + 1 && j < n && chars[j] == '\'' {
                out.push(chars[i + 1..j].iter().collect());
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Word-boundary case-insensitive substring test (avoids matching `models` in
/// `modelsomething`). Mirrors the project convention against unanchored
/// `contains` for routing decisions.
fn word_present(haystack: &str, needle: &str) -> bool {
    let hl = haystack.to_ascii_lowercase();
    let nl = needle.to_ascii_lowercase();
    hl.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w == nl)
}

fn looks_like_model_id(t: &str) -> bool {
    // Must contain a digit and a separator, be lowercase-ish, and reasonable
    // length — filters prose words while accepting `gpt-5.5`, `gemini-3.1-pro`.
    if t.len() < 3 || t.len() > 60 {
        return false;
    }
    let has_sep = t.contains('-') || t.contains('.');
    let has_digit = t.chars().any(|c| c.is_ascii_digit());
    let ok_chars = t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    has_sep && has_digit && ok_chars && t.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

/// Find `<family> <version>` (whitespace-insensitive) in the cleaned menu text.
/// Returns the version string (`5`, `4.6`, `4.8`) on first match.
fn find_family_version(text: &str, family: &str) -> Option<String> {
    let fl = family.to_lowercase();
    let bytes: Vec<char> = text.chars().collect();
    let flc: Vec<char> = fl.chars().collect();
    let lower: Vec<char> = text.to_lowercase().chars().collect();
    let n = lower.len();
    let mut i = 0;
    while i + flc.len() <= n {
        if lower[i..i + flc.len()] == flc[..] {
            // Scan forward past optional whitespace for a version number.
            let mut j = i + flc.len();
            while j < n && bytes[j].is_whitespace() {
                j += 1;
            }
            if j < n && bytes[j].is_ascii_digit() {
                let mut ver = String::new();
                while j < n && (bytes[j].is_ascii_digit() || bytes[j] == '.') {
                    ver.push(bytes[j]);
                    j += 1;
                }
                let ver = ver.trim_end_matches('.').to_string();
                if !ver.is_empty() {
                    return Some(ver);
                }
            }
        }
        i += 1;
    }
    None
}

/// Minimal ANSI/control stripper — drops CSI/OSC escape sequences and other
/// control bytes so downstream keyword matching works on the visible text.
fn strip_ansi_lossy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    // CSI: consume until a final byte in @..~
                    for nc in chars.by_ref() {
                        if ('@'..='~').contains(&nc) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: consume until BEL or ESC\
                    while let Some(nc) = chars.next() {
                        if nc == '\u{07}' {
                            break;
                        }
                        if nc == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        // Keep normal text + newlines; drop other control chars.
        if c == '\n' || c == '\t' || !c.is_control() {
            out.push(c);
        }
    }
    out
}

// ── PTY probe (opt-in, default OFF) ──────────────────────────────────────

/// Drive an interactive `claude` REPL, send `/model`, capture the menu, kill.
/// Fully bounded (≤20s) and always kills the child. Returns raw PTY output.
async fn probe_claude_pty(home_dir: &Path) -> Option<Vec<RuntimeModel>> {
    let program = duduclaw_core::which_claude()
        .or_else(|| duduclaw_core::which_claude_in_home(home_dir))?;
    let raw = tokio::task::spawn_blocking(move || pty_capture_model_menu(&program))
        .await
        .ok()??;
    let models = parse_claude_pty_model_menu(&raw);
    if models.is_empty() {
        None
    } else {
        Some(models)
    }
}

fn pty_capture_model_menu(program: &str) -> Option<String> {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: 40, cols: 120, pixel_width: 0, pixel_height: 0 })
        .ok()?;
    let mut cmd = CommandBuilder::new(program);
    cmd.env("NO_COLOR", "1");
    cmd.env("TERM", "xterm-256color");

    let mut child = pair.slave.spawn_command(cmd).ok()?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().ok()?;
    let mut writer = pair.master.take_writer().ok()?;

    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let done = Arc::new(AtomicBool::new(false));
    let buf_r = buf.clone();
    let done_r = done.clone();
    let reader_thread = std::thread::spawn(move || {
        let mut tmp = [0u8; 8192];
        while !done_r.load(Ordering::Relaxed) {
            match reader.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut b) = buf_r.lock() {
                        b.extend_from_slice(&tmp[..n]);
                    }
                }
                Err(_) => break,
            }
        }
    });

    let snapshot = |buf: &Arc<Mutex<Vec<u8>>>| -> String {
        buf.lock()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    };

    let start = Instant::now();
    let mut sent = false;
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let snap = snapshot(&buf);
        let low = snap.to_lowercase();
        if !sent {
            if low.contains("trust") && low.contains("folder") {
                let _ = writer.write_all(b"\r");
                let _ = writer.flush();
            }
            let ready = low.contains("shortcuts") || low.contains("try \"") || snap.contains('❯');
            if ready || start.elapsed() > Duration::from_secs(9) {
                let _ = writer.write_all(b"/model");
                let _ = writer.flush();
                std::thread::sleep(Duration::from_millis(400));
                let _ = writer.write_all(b"\r");
                let _ = writer.flush();
                sent = true;
            }
        } else if start.elapsed() > Duration::from_secs(17) {
            break;
        }
        if start.elapsed() > Duration::from_secs(20) {
            break;
        }
    }

    done.store(true, Ordering::Relaxed);
    let _ = child.kill();
    let _ = child.wait();
    drop(writer);
    let _ = reader_thread.join();

    Some(snapshot(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_parse_extracts_aliases_and_full_names() {
        let help = "\
  --model <model>                       Model for the current session. Provide
                                        an alias for the latest model (e.g.
                                        'fable', 'opus', or 'sonnet') or a
                                        model's full name (e.g.
                                        'claude-fable-5').
  -n, --name <name>                     Set a display name for this session";
        let models = parse_claude_help_models(help);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"fable"), "got {ids:?}");
        assert!(ids.contains(&"opus"));
        assert!(ids.contains(&"sonnet"));
        // haiku isn't in the help "e.g." list but IS a valid alias — supplemented.
        assert!(ids.contains(&"haiku"));
        // The full-name example duplicates the fable family — deduped away.
        assert!(!ids.contains(&"claude-fable-5"));
        assert_eq!(ids.len(), 4, "exactly the four family aliases: {ids:?}");
        // Labels are title-cased for aliases.
        let fable = models.iter().find(|m| m.id == "fable").unwrap();
        assert_eq!(fable.label, "Fable");
    }

    #[test]
    fn help_parse_no_supplement_without_alias_context() {
        // A window that quotes a full id but no alias words must NOT get the
        // alias set injected (we're likely not in the alias help text at all).
        let help = "--model <model> use 'claude-3-7-experimental' here";
        let models = parse_claude_help_models(help);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["claude-3-7-experimental"], "got {ids:?}");
    }

    #[test]
    fn help_parse_empty_when_no_model_flag() {
        assert!(parse_claude_help_models("no flags here 'opus'").is_empty());
    }

    #[test]
    fn anthropic_response_parse() {
        let json = serde_json::json!({
            "data": [
                { "id": "claude-fable-5", "display_name": "Claude Fable 5", "type": "model" },
                { "id": "claude-opus-4-8", "display_name": "Claude Opus 4.8", "type": "model" },
                { "id": "", "display_name": "skip-me" },
                { "id": "claude-haiku-4-5" }
            ]
        });
        let models = parse_anthropic_models_response(&json);
        assert_eq!(models.len(), 3, "empty id dropped: {models:?}");
        assert_eq!(models[0].id, "claude-fable-5");
        assert_eq!(models[0].label, "Claude Fable 5");
        // Missing display_name falls back to id.
        assert_eq!(models[2].label, "claude-haiku-4-5");
        assert_eq!(models[2].provider, "claude");
    }

    #[test]
    fn anthropic_response_parse_handles_garbage() {
        assert!(parse_anthropic_models_response(&serde_json::json!({})).is_empty());
        assert!(parse_anthropic_models_response(&serde_json::json!({ "data": "nope" })).is_empty());
    }

    #[test]
    fn pty_menu_parse_from_real_capture() {
        // Real (ANSI-stripped, whitespace-collapsed) capture from a live
        // `claude` `/model` menu — the exact shape the interactive TUI emits.
        let raw = "SelectmodelSwitchbetweenClaudemodels. \
            1.Default (recommended)  Opus 4.8 with 1M context \
            2. Fable  Fable 5 · Most capable \
            3.Sonnet Sonnet 4.6 · Efficient \
            4.Sonnet(1Mcontext)Sonnet4.6with1M \
            5.HaikuHaiku4.5·Fastest \
            6. Opus(1Mcontext) Opus 4.8 with1M";
        let models = parse_claude_pty_model_menu(raw);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"opus"), "got {ids:?}");
        assert!(ids.contains(&"sonnet"));
        assert!(ids.contains(&"haiku"));
        assert!(ids.contains(&"fable"));
        let fable = models.iter().find(|m| m.id == "fable").unwrap();
        assert_eq!(fable.label, "Fable 5");
        let opus = models.iter().find(|m| m.id == "opus").unwrap();
        assert_eq!(opus.label, "Opus 4.8");
    }

    #[test]
    fn generic_model_lines_parse() {
        let out = "Available models:\n  gpt-5.5\n  gpt-5.4\n  gpt-5.4-mini\n  (default: gpt-5.5)\nplain prose word\n";
        let models = parse_generic_model_lines("codex", out);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"gpt-5.5"), "got {ids:?}");
        assert!(ids.contains(&"gpt-5.4"));
        assert!(ids.contains(&"gpt-5.4-mini"));
        // Prose words without digit+separator are rejected.
        assert!(!ids.contains(&"prose"));
        // Deduped.
        assert_eq!(ids.iter().filter(|i| **i == "gpt-5.5").count(), 1);
    }

    #[test]
    fn agy_model_lines_parse_verbatim() {
        // Real `agy models` output captured live 2026-07-11. These display
        // names are the exact `--model` values (official codelab example:
        // `agy --model "Gemini 3.5 Flash (Low)"`), so lines pass verbatim.
        let out = "Gemini 3.5 Flash (Medium)\nGemini 3.5 Flash (High)\nGemini 3.5 Flash (Low)\nGemini 3.1 Pro (Low)\nGemini 3.1 Pro (High)\nClaude Sonnet 4.6 (Thinking)\nClaude Opus 4.6 (Thinking)\nGPT-OSS 120B (Medium)\n";
        let models = parse_agy_model_lines(out);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids.len(), 8, "got {ids:?}");
        assert!(ids.contains(&"Gemini 3.5 Flash (Low)"));
        assert!(ids.contains(&"Claude Sonnet 4.6 (Thinking)"));
        assert!(ids.contains(&"GPT-OSS 120B (Medium)"));
        // id == label (display name IS the id for agy).
        let flash = models.iter().find(|m| m.id == "Gemini 3.5 Flash (Low)").unwrap();
        assert_eq!(flash.label, flash.id);
        assert_eq!(flash.provider, "gemini");
    }

    #[test]
    fn agy_model_lines_reject_prose_and_noise() {
        let out = "Available models for your account:\n\nGemini 3.1 Pro (High)\nRun agy --model <name> to pick one of the models listed above today\n2026-07-11\n";
        let models = parse_agy_model_lines(out);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        // Only the actual model line survives: prose is over 6 words or has
        // disallowed chars (`<`, `:`), the date line starts with a digit.
        assert_eq!(ids, vec!["Gemini 3.1 Pro (High)"], "got {ids:?}");
    }

    #[test]
    fn merged_dedup_and_labels() {
        let mut providers = BTreeMap::new();
        providers.insert(
            "claude".to_string(),
            ProviderModels {
                models: vec![
                    m("claude-fable-5", "Claude Fable 5", "claude"),
                    m("claude-opus-4-8", "Claude Opus 4.8", "claude"),
                ],
                source: DiscoverySource::LiveApi,
                fetched_at: "2026-07-11T00:00:00Z".into(),
            },
        );
        providers.insert(
            "codex".to_string(),
            ProviderModels {
                models: vec![
                    m("gpt-5.5", "GPT-5.5", "codex"),
                    // Duplicate id across providers → dropped on second sight.
                    m("claude-fable-5", "dup", "codex"),
                ],
                source: DiscoverySource::Fallback,
                fetched_at: "2026-07-11T00:00:00Z".into(),
            },
        );
        let cache = RuntimeModelsCache { providers, fetched_at: "2026-07-11T00:00:00Z".into() };
        let merged = merged_models(&cache);
        assert_eq!(merged.len(), 3, "one dup dropped: {merged:?}");
        // Source label rides each entry.
        let fable = merged.iter().find(|v| v["id"] == "claude-fable-5").unwrap();
        assert_eq!(fable["source"], "live_api");
        let gpt = merged.iter().find(|v| v["id"] == "gpt-5.5").unwrap();
        assert_eq!(gpt["source"], "fallback");
    }

    #[test]
    fn fallback_source_is_marked() {
        let pm = ProviderModels {
            models: claude_fallback(),
            source: DiscoverySource::Fallback,
            fetched_at: now_rfc3339(),
        };
        assert_eq!(pm.source.as_str(), "fallback");
        assert_eq!(pm.models.len(), 3);
    }

    #[test]
    fn word_present_is_boundary_aware() {
        assert!(word_present("codex models list", "models"));
        assert!(!word_present("modelsomething else", "models"));
        assert!(word_present("Manage MODELS here", "models"));
    }

    #[test]
    fn strip_ansi_removes_escapes() {
        let s = "\u{1b}[1mBold\u{1b}[0m \u{1b}[38;5;12mColor\u{1b}[0m";
        assert_eq!(strip_ansi_lossy(s), "Bold Color");
    }
}
