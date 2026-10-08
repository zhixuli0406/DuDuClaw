//! OpenAI Codex CLI runtime — `codex exec --json` JSONL streaming.
//!
//! Codex CLI outputs JSONL events on stdout when invoked with `--json`:
//!   - `thread.started` — session created
//!   - `turn.started` / `turn.completed` — contains token usage
//!   - `item.completed` (type=message) — assistant text content
//!
//! Authentication: `OPENAI_API_KEY` environment variable.

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{info, warn};

use duduclaw_core::types::{CapabilitiesConfig, sandbox_level_for};

use super::{AgentRuntime, RuntimeContext, RuntimeResponse};

const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Derive Codex CLI sandbox/approval flags from the agent's capabilities.
///
/// Replaces the former blanket `--full-auto` (which unconditionally implied
/// `workspace-write` + no approvals, ignoring `CapabilitiesConfig` entirely).
/// Non-interactive `codex exec` needs an approval policy, and the blast radius
/// is scoped via `--sandbox`:
/// - restrictive caps (no write tools, no browser/computer use) → `read-only`
/// - default / `None` caps → `workspace-write` (same write scope `--full-auto` granted)
/// - explicit `computer_use = true` grant → `danger-full-access`
///
/// **Fixed 2026-09-24**: the approval policy used to be passed as
/// `--ask-for-approval never`. That flag does not exist on Codex CLI 0.156.1 —
/// `codex exec` rejects it with `error: unexpected argument '--ask-for-approval'
/// found` (exit 2), so EVERY codex spawn failed and fell over to the fallback
/// runtime. `codex exec --help` on 0.156.1 lists only `-s/--sandbox`,
/// `--approve-for-me`, `--dangerously-bypass-approvals-and-sandbox` and
/// `-c/--config`. The approval policy is a config key rather than a flag, so it
/// goes through the same `-c` override channel [`mcp_override_args`] already
/// uses. Verified live, not inferred:
///
/// ```text
/// $ codex doctor                                 → approval policy  OnRequest
/// $ codex doctor -c approval_policy=never        → approval policy  Never
/// $ codex doctor -c approval_policy=untrusted    → 1 fail
/// ```
///
/// matching the official config reference (learn.chatgpt.com config-reference):
/// `approval_policy` takes `on-request | never | {granular=…}`, with "`untrusted`
/// is unsupported, and `on-failure` is deprecated; use `on-request` for
/// interactive runs or **`never` for non-interactive runs**".
///
/// `--dangerously-bypass-approvals-and-sandbox` is used at exactly one level —
/// `FullAccess`, where lifting the sandbox is the explicit operator grant. The
/// other two levels keep a real sandbox.
///
/// **`ReadOnly` no longer fails open (2026-09-28 review, decided: option A).**
/// Until this change every non-`FullAccess` level was spawned with
/// `--approve-for-me` plus `-c sandbox_mode="<level>"`. That is fine for
/// `WorkspaceWrite`, but for `ReadOnly` it was a fail-OPEN gate: verified live
/// on 0.156.1, `--approve-for-me` routes approvals "through automatic review
/// using the workspace-write sandbox", so a `read-only` *declaration* did not
/// block file writes at all — a capability-restricted agent still had write
/// access. The two doc paragraphs in this file disagreed about it, one
/// claiming enforcement and one admitting it was advisory.
///
/// `ReadOnly` now spawns with the real `-s read-only` flag, which does block
/// writes. The cost, stated out loud rather than discovered later: `-s/--sandbox`
/// is mutually exclusive with `--approve-for-me` (clap error), so a `ReadOnly`
/// codex spawn has no automatic approver and **every MCP tool call is
/// auto-rejected** by `approval_policy=never` — the agent keeps reasoning and
/// reading, and loses the duduclaw tool surface. That is the correct trade for a
/// level whose whole meaning is "this agent may not change anything", and
/// [`AgentRuntime::execute`] emits one `warn!` per spawn saying so.
fn sandbox_args(caps: Option<&CapabilitiesConfig>) -> Vec<String> {
    let level = sandbox_level_for(caps);
    // Live-verified on Codex 0.156.1 (2026-09-24, eight variants):
    // * every MCP tool call is gated behind an approval request; with
    //   `approval_policy=never` the request is auto-*rejected*, and neither
    //   `mcp_servers.<id>.default_tools_approval_mode="auto"` (via `-c` or the
    //   config file) nor `projects.<cwd>.trust_level="trusted"` changes that;
    // * `--approve-for-me` ("route approval requests through automatic review
    //   using the workspace-write sandbox") is the supported non-interactive
    //   path and lets MCP calls through — but it runs that review in a
    //   workspace-write sandbox, so it cannot be combined with a read-only
    //   confinement (see the doc comment above);
    // * `--approve-for-me` is mutually exclusive with `-s/--sandbox <MODE>`
    //   (clap error), so a level that needs `--approve-for-me` declares itself
    //   through the `sandbox_mode` config key instead;
    // * `danger-full-access` keeps the explicit bypass flag, which is the only
    //   way to actually lift the sandbox, and stays an explicit operator
    //   opt-in through `sandbox_level_for`.
    match level {
        duduclaw_core::types::SandboxLevel::FullAccess => {
            vec!["--dangerously-bypass-approvals-and-sandbox".to_string()]
        }
        // Real confinement, no automatic approver. `approval_policy=never`
        // still has to be declared: without it a non-interactive `codex exec`
        // waits on a prompt nobody can answer.
        duduclaw_core::types::SandboxLevel::ReadOnly => vec![
            "-s".to_string(),
            "read-only".to_string(),
            "-c".to_string(),
            "approval_policy=never".to_string(),
        ],
        duduclaw_core::types::SandboxLevel::WorkspaceWrite => vec![
            "--approve-for-me".to_string(),
            "-c".to_string(),
            "approval_policy=never".to_string(),
            "-c".to_string(),
            format!(
                "sandbox_mode={}",
                toml_string_literal(level.as_codex_flag())
            ),
        ],
    }
}

/// `--output-schema <FILE>` for one invocation (Team-as-Agent live round 8).
///
/// Round 8 reached the settle with real work on disk and was rejected on the
/// *shape of the verdict*: with `[dispatch] judge_provider = "codex"` both the
/// two-stage evaluator and the MAV panel replied in prose, and both parsers
/// refused it fail-closed. Claude obeys "reply with ONLY a JSON object"; codex
/// does not reliably, and does not need to — `codex exec --output-schema
/// <FILE>` takes a JSON Schema file and constrains the reply itself.
///
/// Returns `(args, tempfile)`. The `NamedTempFile` is returned rather than
/// dropped so the file outlives the spawn — dropping it here would delete the
/// schema before codex opened it. `None` schema ⇒ empty argv, byte-identical
/// to before this existed. A write failure degrades to "no flag" with a
/// `warn!`: an adjudication must never fail because the schema could not be
/// staged (the parser is still fail-closed downstream, so the worst case is
/// the pre-round-8 behavior, not a wrong verdict).
fn output_schema_args(
    schema: Option<&serde_json::Value>,
) -> (Vec<String>, Option<tempfile::NamedTempFile>) {
    let Some(schema) = schema else {
        return (Vec::new(), None);
    };
    let mut file = match tempfile::Builder::new()
        .prefix("duduclaw-codex-schema-")
        .suffix(".json")
        .tempfile()
    {
        Ok(f) => f,
        Err(e) => {
            warn!(runtime = "codex", error = %e, "could not stage --output-schema file — continuing without structured output");
            return (Vec::new(), None);
        }
    };
    use std::io::Write;
    let rendered = match serde_json::to_vec_pretty(schema) {
        Ok(b) => b,
        Err(e) => {
            warn!(runtime = "codex", error = %e, "output_schema is not serializable — continuing without structured output");
            return (Vec::new(), None);
        }
    };
    if let Err(e) = file.write_all(&rendered).and_then(|_| file.flush()) {
        warn!(runtime = "codex", error = %e, "could not write --output-schema file — continuing without structured output");
        return (Vec::new(), None);
    }
    let path = file.path().to_string_lossy().to_string();
    (vec!["--output-schema".to_string(), path], Some(file))
}

/// `-c model_reasoning_effort=<level>` for one invocation (P1/WP-3).
///
/// Codex has no dedicated effort flag; `model_reasoning_effort` is a config key
/// reached through the same `-c` channel as the MCP overrides below. Probed
/// values on 0.156.1 are `low|medium|high|xhigh` — no `max`, so
/// `Effort::clamp_for` folds `Max` down to `XHigh`. `None` ⇒ empty, and the
/// argv is byte-identical to before effort existed.
fn effort_args(effort: Option<duduclaw_core::effort::Effort>) -> Vec<String> {
    match effort {
        Some(e) => {
            let clamped = e.clamp_for(duduclaw_core::types::RuntimeType::Codex);
            vec![
                "-c".to_string(),
                format!("model_reasoning_effort={}", clamped.as_str()),
            ]
        }
        None => Vec::new(),
    }
}

/// Render `s` as a TOML **basic string** literal (quotes included).
///
/// Single source of TOML string quoting for this module — used both by the
/// per-invocation `-c key=value` overrides below and by
/// [`CodexRuntime::render_mcp_toml`].
///
/// **Why this exists (live round 3, 2026-09-24).** `codex exec -c key=value`
/// parses the `value` half **as TOML** and only falls back to "raw string" when
/// that parse fails (`codex exec --help`: "The `value` portion is parsed as
/// TOML. If it fails to parse as TOML, the raw string is used as a literal").
/// So a value that happens to be valid TOML of the *wrong type* is accepted
/// with that wrong type. `DUDUCLAW_PORT` is the concrete case: the gateway's
/// `.mcp.json` env block holds the JSON **string** `"18999"`, the override was
/// emitted unquoted as `mcp_servers.duduclaw.env.DUDUCLAW_PORT=18999`, codex
/// parsed it as the TOML integer `18999`, and every single codex spawn died
/// before it started with
///
/// ```text
/// Error loading config.toml: invalid type: integer `18999`, expected a string
///   in `mcp_servers.duduclaw.env.DUDUCLAW_PORT`
/// ```
///
/// (exit 1). This is a pre-existing codex-runtime bug, not a team one — it
/// broke ordinary codex agents and the `[dispatch] judge_provider = "codex"`
/// judge exactly the same way; the team live test is only where it surfaced.
/// An env map is `HashMap<String, String>` on the codex side, so **every**
/// value must be quoted, not just the numeric-looking ones.
///
/// Escaping follows the TOML 1.0 basic-string rules: backslash and quote are
/// escaped, the four compact control escapes are used where they exist, and
/// any other control character becomes `\uXXXX`. Non-control non-ASCII (CJK,
/// emoji) is emitted verbatim — TOML basic strings are UTF-8.
fn toml_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `-c` config-override args registering the duduclaw MCP server for THIS
/// invocation. Codex only reads MCP servers from `$CODEX_HOME/config.toml`;
/// redirecting `CODEX_HOME` at the agent dir would orphan the user's
/// `~/.codex/auth.json` (breaking ChatGPT-plan OAuth), so per-invocation
/// `--config` overrides are the safe way to guarantee registration.
///
/// Every scalar goes through [`toml_string_literal`] — see its doc comment for
/// the spawn-killing bug that unquoted values caused.
///
/// **`default_tools_approval_mode = "auto"` (live round 5, 2026-09-24).**
/// Registering the server is not enough: the member *saw* the duduclaw tools
/// (`mcp__duduclaw__team_handoff`, `working_state_handoff` were in its tool
/// list) and then had **every single call refused** with
///
/// ```text
/// MCP tool call requires approval, but approval policy is never
/// ```
///
/// which is codex's fail-closed reading of [`sandbox_args`]'s
/// `approval_policy=never`: a non-interactive `codex exec` has nobody to
/// answer an approval prompt, so a tool that needs one can only be denied.
/// The official config reference (learn.chatgpt.com config-reference, fetched
/// 2026-09-24) gives the per-server escape hatch —
/// `mcp_servers.<id>.default_tools_approval_mode = "auto" | "prompt" |
/// "writes" | "approve"`, with a per-tool `mcp_servers.<id>.tools.<tool>.
/// approval_mode` override — so the duduclaw server is declared `auto`.
///
/// Why that is not a hole: this scopes to **our own** server only (codex's
/// own shell/patch tools keep whatever the `--sandbox` level allows, and any
/// other MCP server the operator registered keeps its own default), and the
/// duduclaw MCP server is already scope-gated (`Scope::*` per tool),
/// capability-gated (`allowed_tools`/`denied_tools`/`scoped_tools` at the
/// dispatch gate) and audited (`tool_calls.jsonl`) on its own side. The
/// approval prompt codex wants to show would be answered by nobody; the real
/// authorization lives one process over, not in an unattended TTY.
fn mcp_override_args(agent_id: &str, home_dir: &std::path::Path) -> Vec<String> {
    let Some(def) = super::duduclaw_mcp_server_json_for_home(agent_id, home_dir) else {
        return Vec::new();
    };
    let mut args = mcp_override_base_args(&def);
    if let Some(env) = def.get("env").and_then(|e| e.as_object()) {
        args.extend(mcp_env_override_args(env));
    }
    args
}

/// The runtime-version-independent half of [`mcp_override_args`]: the server
/// command, its argv and the per-server approval mode. Split out so the two
/// credential-delivery shapes ([`mcp_override_args`] for old Codex,
/// [`mcp_override_args_with`] for `env_vars`-capable Codex) cannot drift apart.
fn mcp_override_base_args(def: &serde_json::Value) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(command) = def.get("command").and_then(|c| c.as_str()) {
        args.push("-c".to_string());
        args.push(format!(
            "mcp_servers.duduclaw.command={}",
            toml_string_literal(command)
        ));
    }
    args.push("-c".to_string());
    args.push(format!(
        "mcp_servers.duduclaw.args=[{}]",
        toml_string_literal("mcp-server")
    ));
    // See the doc comment: without this every duduclaw tool call is refused
    // under `approval_policy=never`. A TOML string, like every other scalar.
    args.push("-c".to_string());
    args.push(format!(
        "mcp_servers.duduclaw.default_tools_approval_mode={}",
        toml_string_literal("auto")
    ));
    args
}

/// What one Codex spawn needs in order to register the duduclaw MCP server:
/// the `-c` overrides that go in `argv`, plus the variables that must be set on
/// the **Codex process itself** so Codex can forward them by name.
#[derive(Debug, Default, PartialEq, Eq)]
struct McpOverrides {
    /// `-c key=value` pairs appended to the Codex argv.
    args: Vec<String>,
    /// `(name, value)` pairs to set via `Command::env` on the Codex process.
    /// Non-empty only on the `env_vars` branch; the values never reach `argv`.
    process_env: Vec<(String, String)>,
}

/// Does this env-var **name** designate a credential whose value must never
/// reach `argv`?
///
/// Exact ASCII-case-insensitive suffix match, deliberately aligned with the
/// secret-shape convention `duduclaw-core::spawn_env` documents and enforces
/// on its allowlist (`allowlist_never_carries_a_secret_shaped_name`). That
/// guard is a test-local list rather than an exported predicate, so this is a
/// local copy of the same rule — if it ever becomes a shared helper, both
/// should move onto it together.
///
/// `_KEY` (the fifth suffix that test uses) is **not** included: the spawn-env
/// allowlist is judging names it must refuse outright, while here a
/// false positive merely routes a harmless variable through the more private
/// channel. The four suffixes below cover every credential
/// `duduclaw_mcp_server_json_for_home` emits today
/// (`DUDUCLAW_MCP_API_KEY`, `DUDUCLAW_AGENT_TOKEN`), and widening the rule is
/// safe by construction — the `env_vars` branch is strictly more private than
/// the `env` branch, never less.
fn is_secret_env_name(k: &str) -> bool {
    const SECRET_SUFFIXES: &[&str] = &["_API_KEY", "_TOKEN", "_SECRET", "_PASSWORD"];
    let upper = k.to_ascii_uppercase();
    SECRET_SUFFIXES.iter().any(|s| upper.ends_with(s))
}

/// [`mcp_override_args`] with the Codex-version gate made explicit, so both
/// branches are testable without running a real `codex` binary.
///
/// `supports_env_vars = false` ⇒ byte-identical to [`mcp_override_args`]:
/// every value, credentials included, travels in `argv` (see the
/// `docs/features/13-multi-runtime.md` exposure note).
///
/// `supports_env_vars = true` (Codex ≥ 0.157.0, see
/// [`codex_supports_env_vars`]) ⇒ credential-shaped names are emitted as
/// `-c mcp_servers.duduclaw.env_vars=["NAME", …]` — only the **names** reach
/// `argv` — and their values are set on the Codex process environment, from
/// which Codex copies them into the MCP child. Non-credential entries
/// (`DUDUCLAW_HOME`, `DUDUCLAW_PORT`, `DUDUCLAW_AGENT_ID`, …) keep the
/// `env.<K>="<value>"` form: they are not secrets, and keeping them in the
/// config table means a spawn that loses the process env still registers a
/// usable server.
fn mcp_override_args_with(
    agent_id: &str,
    home_dir: &std::path::Path,
    supports_env_vars: bool,
) -> McpOverrides {
    if !supports_env_vars {
        return McpOverrides {
            args: mcp_override_args(agent_id, home_dir),
            process_env: Vec::new(),
        };
    }
    let Some(def) = super::duduclaw_mcp_server_json_for_home(agent_id, home_dir) else {
        return McpOverrides::default();
    };
    let mut args = mcp_override_base_args(&def);
    let mut process_env: Vec<(String, String)> = Vec::new();
    if let Some(env) = def.get("env").and_then(|e| e.as_object()) {
        let (env_args, secrets) = split_env_overrides(env);
        args.extend(env_args);
        process_env = secrets;
    }
    McpOverrides { args, process_env }
}

/// The `env_vars` branch's env handling, split out so it can be driven by a
/// synthetic map (the production map comes from
/// `duduclaw_mcp_server_json_for_home`, which reads process state and may
/// carry no credentials at all on a developer machine).
///
/// Returns `(argv overrides, values for the codex process env)`. Credential-
/// shaped names appear in the argv ONLY inside the
/// `mcp_servers.duduclaw.env_vars=[…]` array; their values appear only in the
/// second half.
fn split_env_overrides(
    env: &serde_json::Map<String, serde_json::Value>,
) -> (Vec<String>, Vec<(String, String)>) {
    let mut plain = serde_json::Map::new();
    let mut secret_names: Vec<String> = Vec::new();
    let mut process_env: Vec<(String, String)> = Vec::new();
    for (k, v) in env {
        if !is_secret_env_name(k) {
            plain.insert(k.clone(), v.clone());
            continue;
        }
        // The name itself is interpolated into a TOML array literal and then
        // used by Codex as an env-var name: same key-shape gate the `env.<K>`
        // form uses, for the same reason.
        if !is_safe_config_key_segment(k) {
            warn!(
                runtime = "codex",
                key = %k,
                "MCP env key is not a bare TOML key — dropped from the `env_vars` overrides"
            );
            continue;
        }
        let Some(val) = env_value_as_string(v) else {
            continue;
        };
        secret_names.push(k.clone());
        process_env.push((k.clone(), val));
    }
    let mut args = mcp_env_override_args(&plain);
    if !secret_names.is_empty() {
        let names: Vec<String> = secret_names.iter().map(|n| toml_string_literal(n)).collect();
        args.push("-c".to_string());
        args.push(format!(
            "mcp_servers.duduclaw.env_vars=[{}]",
            names.join(", ")
        ));
    }
    (args, process_env)
}

/// `-c mcp_servers.duduclaw.env.<K>="<v>"` for the turn/run source pairs
/// (P2-B N4). None of them is a credential, so the `env` form is used on
/// every Codex version.
fn turn_source_override_args(pairs: &[(String, String)]) -> Vec<String> {
    let env: serde_json::Map<String, serde_json::Value> = pairs
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    mcp_env_override_args(&env)
}

/// The `-c mcp_servers.duduclaw.env.<K>=<V>` half of [`mcp_override_args`],
/// split out so it can be tested against a synthetic env map (the production
/// map comes from `duduclaw_mcp_server_json`, which reads process state).
///
/// Codex types that table as `HashMap<String, String>`, so **every** value is
/// emitted as a quoted TOML string. A non-string JSON scalar (number, bool) is
/// stringified rather than skipped — the env block's contract is "a process
/// environment", where everything is text, and skipping would silently deprive
/// the member's MCP server of, say, its port. Arrays/objects/null have no
/// environment-variable meaning and are skipped.
fn mcp_env_override_args(env: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    let mut args = Vec::new();
    for (k, v) in env {
        if !is_safe_config_key_segment(k) {
            warn!(
                runtime = "codex",
                key = %k,
                "MCP env key is not a bare TOML key — dropped from the `-c` overrides"
            );
            continue;
        }
        let Some(val) = env_value_as_string(v) else {
            continue;
        };
        args.push("-c".to_string());
        args.push(format!(
            "mcp_servers.duduclaw.env.{k}={}",
            toml_string_literal(&val)
        ));
    }
    args
}

/// A JSON scalar as the string a process environment would carry, or `None`
/// for a value with no environment-variable meaning (array / object / null).
/// See [`mcp_env_override_args`] for why non-string scalars are stringified
/// rather than skipped.
fn env_value_as_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Is `k` safe to interpolate into the **key half** of a `-c <key>=<value>`
/// override? (2026-09-28 review, latent-surface closure.)
///
/// [`toml_string_literal`] has always guarded the value half; the key half was
/// interpolated raw. Every production key today comes from
/// [`super::duduclaw_mcp_server_json_for_home`]'s fixed `DUDUCLAW_*` set, so
/// nothing is known to be broken — this closes the surface before a key ever
/// arrives from somewhere less fixed. A key containing `=`, a quote, a dot, a
/// newline or a space would either retarget the override at a different config
/// path or produce an unparseable `-c` payload, so anything outside a bare TOML
/// key's character class is dropped with a `warn!` rather than quoted: an
/// environment variable name that needs quoting is not a name this code should
/// be inventing a shape for.
///
/// `.` is deliberately **excluded** even though it is legal inside a dotted
/// config path — here the segment is a single env-var name, and a dot in it
/// would silently create a nested table instead of an env entry.
fn is_safe_config_key_segment(k: &str) -> bool {
    !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

// ── `env_vars` capability gate ──────────────────────────────────
//
// `mcp_servers.<id>.env_vars = ["NAME", …]` names variables Codex copies out
// of its OWN process environment into the MCP child, so a credential reaches
// the child without its value ever appearing in `argv`. Live-probed working on
// `codex-cli 0.157.1` (evidence:
// `commercial/evidence/team-as-agent-2026-09-28/codex-env-passthrough/`).
//
// It cannot simply be adopted unconditionally. The field is part of
// `McpServerTransportConfig::Stdio` in `codex-rs/config/src/mcp_types.rs`, and
// the surrounding `RawMcpServerConfig` carries `deny_unknown_fields`; on a
// Codex old enough not to know the key, the run either dies at config parse
// (EVERY spawn lost — this repo has already been bitten twice by exactly that,
// by `--ask-for-approval` and by an unquoted `DUDUCLAW_PORT`) or silently drops
// the credential (the agent loses every duduclaw tool with no error). So the
// shape is chosen per binary, from the binary's own reported version, and
// anything that is not a confident "new enough" falls back to the pre-existing
// `argv` behavior.

/// First Codex CLI version known to accept `mcp_servers.<id>.env_vars`.
const CODEX_ENV_VARS_MIN_VERSION: (u64, u64, u64) = (0, 157, 0);

/// Parse a `codex --version` line (`codex-cli 0.157.1`) into `(major, minor,
/// patch)`.
///
/// Deliberately strict: a token must carry exactly three dot-separated numeric
/// components (a trailing `-rc1` / `+build` suffix is tolerated and ignored).
/// Anything else — two components, words, an empty string — is `None`, which
/// the caller reads as "not new enough" and falls back to `argv`.
fn parse_codex_version(output: &str) -> Option<(u64, u64, u64)> {
    for token in output.split_whitespace() {
        // `codex-cli` itself has no digits+dots shape, but be explicit: strip
        // a leading `v` and cut any pre-release / build metadata.
        let token = token.trim_start_matches('v');
        let core = token.split(['-', '+']).next().unwrap_or(token);
        let mut parts = core.split('.');
        let (Some(a), Some(b), Some(c), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if let (Ok(a), Ok(b), Ok(c)) = (a.parse::<u64>(), b.parse::<u64>(), c.parse::<u64>()) {
            return Some((a, b, c));
        }
    }
    None
}

/// Does a `codex --version` output report a CLI that understands `env_vars`?
/// Unparseable ⇒ `false` (fall back to the `argv` shape, never guess).
fn version_supports_env_vars(output: &str) -> bool {
    parse_codex_version(output).is_some_and(|v| v >= CODEX_ENV_VARS_MIN_VERSION)
}

/// Per-binary-path memo of [`codex_supports_env_vars`], for the process
/// lifetime. A `tokio::sync::OnceCell` per path guarantees the probe subprocess
/// runs at most once even if several spawns race.
type EnvVarsSupportCache =
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::OnceCell<bool>>>>;
static CODEX_ENV_VARS_SUPPORT: std::sync::OnceLock<EnvVarsSupportCache> =
    std::sync::OnceLock::new();

/// Run `<bin> --version` once and return its output, or `None` on spawn
/// failure / non-zero exit / 5s timeout.
///
/// The probe can never make a spawn fail: every failure mode returns `None`,
/// which [`codex_supports_env_vars`] reads as "not supported".
async fn probe_codex_version_output(bin: &str) -> Option<String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new(bin)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if stdout.trim().is_empty() {
        // Some builds print the banner on stderr; a version is a version.
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return (!stderr.trim().is_empty()).then_some(stderr);
    }
    Some(stdout)
}

/// [`codex_supports_env_vars`] with the probe injected, so the caching
/// behavior can be tested without running a real binary.
async fn cached_env_vars_support<F, Fut>(bin: &str, probe: F) -> bool
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    let cell = {
        // Short, await-free critical section: pick (or create) this path's
        // cell, then drop the lock before the probe runs.
        let mut map = CODEX_ENV_VARS_SUPPORT
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.entry(bin.to_string())
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::OnceCell::new()))
            .clone()
    };
    *cell
        .get_or_init(|| async {
            match probe().await {
                Some(output) => {
                    let supported = version_supports_env_vars(&output);
                    if supported {
                        info!(
                            runtime = "codex",
                            version = %output.trim(),
                            "Codex supports `mcp_servers.<id>.env_vars` — MCP credentials \
                             stay out of the codex argv"
                        );
                    } else {
                        warn!(
                            runtime = "codex",
                            version = %output.trim(),
                            min_version = ?CODEX_ENV_VARS_MIN_VERSION,
                            "Codex is older than the first release known to accept \
                             `mcp_servers.<id>.env_vars` (or reported an unparseable \
                             version) — falling back to `-c …env.<K>=\"<value>\"`, which \
                             puts MCP credentials in this host's process list"
                        );
                    }
                    supported
                }
                None => {
                    warn!(
                        runtime = "codex",
                        "could not read `codex --version` (spawn failure, non-zero exit or \
                         timeout) — assuming no `env_vars` support and falling back to \
                         `-c …env.<K>=\"<value>\"`"
                    );
                    false
                }
            }
        })
        .await
}

/// Is the Codex binary at `bin` new enough to accept
/// `mcp_servers.<id>.env_vars`? Probed once per binary path per process.
async fn codex_supports_env_vars(bin: &str) -> bool {
    cached_env_vars_support(bin, || probe_codex_version_output(bin)).await
}

/// Runtime that delegates to the OpenAI Codex CLI.
pub struct CodexRuntime {
    codex_path: String,
}

impl CodexRuntime {
    pub fn new() -> Self {
        Self {
            codex_path: "codex".to_string(),
        }
    }
}

// ── JSONL event types ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CodexEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(flatten)]
    extra: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct CodexUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

/// Parse a complete `codex exec --json` stdout buffer into `(content,
/// input_tokens, output_tokens, chunks)`.
///
/// T10 (design commercial/docs/design-task-forward-model-2026-08-06.md
/// §8.2/§9): the previously zero-constructor `RuntimeChunk::ToolUse` /
/// `ToolResult` variants (`runtime/mod.rs`) are populated from `item.
/// completed` events whose `item.type` is `command_execution` or
/// `mcp_tool_call` — the two tool-item types `codex exec --json` emits
/// alongside the `message` item this parser already reads for `content`.
/// `execute()` below folds `chunks` into `NativeToolEvent`s via
/// [`super::native_tool_events_from_chunks`] — the same runtime-neutral fold
/// `gemini.rs` uses, so `prediction::task_observe` never needs
/// codex-specific code (design §8.2's stated purpose for routing through
/// `RuntimeChunk`).
///
/// Schema grounded in the published `codex exec --json` event reference
/// (item fields: `status` ∈ {in_progress, completed, failed};
/// `command_execution` carries `command`/`aggregated_output`/`exit_code`;
/// `mcp_tool_call` carries `server`/`tool`/`arguments`/`result`/`error`) —
/// NOT verified against the Rust source (`codex-rs/core/protocol.rs`)
/// directly, so exact field names are treated as best-effort: an
/// absent/renamed field degrades to skipping that event, never a
/// fabricated tool name or success value.
fn parse_codex_stdout(stdout: &str) -> (String, u64, u64, Vec<super::RuntimeChunk>) {
    use super::RuntimeChunk;

    let mut content = String::new();
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
    let mut chunks: Vec<RuntimeChunk> = Vec::new();

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<CodexEvent>(line) {
            match event.event_type.as_str() {
                "item.completed" => {
                    // Extract text from message items
                    if let Some(item) = event.extra.get("item") {
                        match item.get("type").and_then(|t| t.as_str()) {
                            Some("message") => {
                                if let Some(text) = item
                                    .get("content")
                                    .and_then(|c| c.as_array())
                                    .and_then(|arr| {
                                        arr.iter().find(|b| {
                                            b.get("type").and_then(|t| t.as_str())
                                                == Some("output_text")
                                        })
                                    })
                                    .and_then(|b| b.get("text"))
                                    .and_then(|t| t.as_str())
                                {
                                    content = text.to_string();
                                }
                            }
                            // Codex CLI 0.156.x emits the assistant's final
                            // message as `agent_message` with a flat `text`
                            // (observed live 2026-09-24: `{"type":"item.completed",
                            // "item":{"id":"item_0","type":"agent_message","text":"OK"}}`).
                            // Without this arm the content fell back to the
                            // last raw stdout line (`turn.completed` usage
                            // JSON), which broke every judge/verifier parse.
                            Some("agent_message") => {
                                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                    content = text.to_string();
                                } else if let Some(text) = item
                                    .get("content")
                                    .and_then(|c| c.as_array())
                                    .and_then(|arr| {
                                        arr.iter().find(|b| {
                                            b.get("type").and_then(|t| t.as_str())
                                                == Some("output_text")
                                        })
                                    })
                                    .and_then(|b| b.get("text"))
                                    .and_then(|t| t.as_str())
                                {
                                    content = text.to_string();
                                }
                            }
                            Some("command_execution") => {
                                // ToolClass::classify maps codex's shell tool
                                // via the literal name "shell" (see
                                // `prediction/tool_class.rs`'s cross-runtime
                                // Exec alias table) — this is a synthesized
                                // label, not something read off the wire
                                // (the command_execution item has no
                                // separate "tool name" field; the item TYPE
                                // itself is the signal).
                                let status = item.get("status").and_then(|s| s.as_str());
                                // "failed" is the only documented failure
                                // status; anything else (including an
                                // absent field) is treated as
                                // attempted-and-not-known-to-have-failed,
                                // matching this collector's optimistic
                                // default on the claude/gemini paths.
                                let is_error = status == Some("failed");
                                let input = item
                                    .get("command")
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null);
                                // R1: the documented `aggregated_output` field
                                // carries the shell command's actual stdout —
                                // captured verbatim here; masking + capping
                                // happens downstream in
                                // `native_tool_events_from_chunks` (never
                                // guessed at when absent).
                                let output = item
                                    .get("aggregated_output")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                chunks.push(RuntimeChunk::ToolUse {
                                    name: "shell".to_string(),
                                    input,
                                });
                                chunks.push(RuntimeChunk::ToolResult { output, is_error });
                            }
                            Some("mcp_tool_call") => {
                                if let Some(tool) = item.get("tool").and_then(|t| t.as_str()) {
                                    let status = item.get("status").and_then(|s| s.as_str());
                                    let is_error = status == Some("failed");
                                    let input = item
                                        .get("arguments")
                                        .cloned()
                                        .unwrap_or(serde_json::Value::Null);
                                    // R1: dual-name tolerance — the documented
                                    // shape is `result.content[].text`
                                    // (mirrors an MCP tool_result content
                                    // array); on failure fall back to
                                    // `error.message`. Neither present ⇒
                                    // empty output, never fabricated.
                                    let output =
                                        extract_mcp_tool_call_output(&item).unwrap_or_default();
                                    chunks.push(RuntimeChunk::ToolUse {
                                        name: tool.to_string(),
                                        input,
                                    });
                                    chunks.push(RuntimeChunk::ToolResult { output, is_error });
                                }
                                // No "tool" field ⇒ skip (don't fabricate a
                                // name) rather than record an "unknown"
                                // placeholder.
                            }
                            _ => {}
                        }
                    }
                }
                "turn.completed" => {
                    // Extract token usage
                    if let Some(usage) = event.extra.get("usage") {
                        if let Ok(u) = serde_json::from_value::<CodexUsage>(usage.clone()) {
                            input_tokens = u.input_tokens;
                            output_tokens = u.output_tokens;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    (content, input_tokens, output_tokens, chunks)
}

/// R1: extract an `mcp_tool_call` item's output text for `RuntimeChunk::ToolResult`.
/// Tries the documented success shape first (`result.content[].text`, the
/// same content-block array MCP `tool_result`s use), then falls back to
/// `error.message` on failure. `None` when neither is present — the caller
/// treats that as "nothing captured", never fabricates a placeholder.
fn extract_mcp_tool_call_output(item: &serde_json::Value) -> Option<String> {
    if let Some(arr) = item.pointer("/result/content").and_then(|c| c.as_array()) {
        let parts: Vec<&str> = arr
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect();
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    // A bare string result (undocumented but tolerated — dual-shape).
    if let Some(s) = item.get("result").and_then(|r| r.as_str()) {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    item.pointer("/error/message")
        .and_then(|m| m.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

// ── AgentRuntime impl ───────────────────────────────────────────

#[async_trait]
impl AgentRuntime for CodexRuntime {
    fn name(&self) -> &str {
        "codex"
    }

    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        // P5: this runtime cannot carry the read-only explore lane.
        crate::explore_lane::refuse_unsupported_runtime("codex")?;
        info!(agent = %context.agent_id, "CodexRuntime: executing via codex exec --json");

        // Limit system_prompt to 64KB to avoid ARG_MAX issues.
        // Char-boundary-safe truncation (never raw byte-index slicing on
        // potentially CJK/emoji content — 2026-06 review convention #1).
        const MAX_SYSTEM_PROMPT_BYTES: usize = 65536;
        let system_prompt: &str = if context.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
            tracing::warn!(
                agent = %context.agent_id,
                original_len = context.system_prompt.len(),
                "system_prompt truncated to 64KB"
            );
            duduclaw_core::truncate_bytes(&context.system_prompt, MAX_SYSTEM_PROMPT_BYTES)
        } else {
            &context.system_prompt
        };

        // Prevent argument injection: prompts starting with '-' would be parsed as flags
        let safe_prompt = if prompt.starts_with('-') {
            format!(" {prompt}")
        } else {
            prompt.to_string()
        };

        // W1 (capability enforcement): derive sandbox/approval flags from the
        // agent's capabilities instead of the former blanket `--full-auto`.
        let caps = context.capabilities.as_ref();
        let level = sandbox_level_for(caps);
        // Decided 2026-09-28 (option A): ReadOnly buys real write blocking at
        // the price of the MCP tool surface, because `-s read-only` and
        // `--approve-for-me` cannot coexist. Said once per spawn so the
        // shrinkage is discoverable from the log rather than from behaviour.
        if level == duduclaw_core::types::SandboxLevel::ReadOnly {
            warn!(
                runtime = "codex",
                agent = %context.agent_id,
                "read-only capabilities ⇒ spawning with `-s read-only`, which excludes \
                 `--approve-for-me`: writes are genuinely blocked, and every MCP tool call \
                 will be auto-rejected by `approval_policy=never` for this run"
            );
        }
        if let Some(c) = caps {
            if c.has_tool_restrictions() {
                warn!(
                    runtime = "codex",
                    agent = %context.agent_id,
                    sandbox = level.as_codex_flag(),
                    "capability enforcement is best-effort on this runtime — \
                     per-tool allow/deny lists collapse to a coarse --sandbox level"
                );
            }
        }

        // W2 (MCP wiring): register the duduclaw MCP server before spawning.
        // 1) Per-invocation `-c` overrides — effective regardless of CODEX_HOME.
        // 2) Best-effort per-agent `.codex/config.toml` for operators who run
        //    codex manually in the agent dir with CODEX_HOME pointed there.
        //    Warn-not-fatal: MCP registration failing must not block the reply.
        if let Some(ref dir) = context.agent_dir {
            if let Err(e) =
                Self::ensure_duduclaw_mcp_config(dir, &context.agent_id, &context.home_dir)
            {
                warn!(
                    runtime = "codex",
                    agent = %context.agent_id,
                    error = %e,
                    "failed to write per-agent codex MCP config — continuing without it"
                );
            }
        }

        let mut cmd = tokio::process::Command::new(&self.codex_path);
        cmd.arg("exec").arg("--json");
        // Agent directories are not git repositories. Codex 0.156.x refuses to
        // run outside a trusted (git) directory unless told otherwise —
        // observed live 2026-09-24: "Not inside a trusted directory and
        // --skip-git-repo-check was not specified." (exit 1). Reproduced in a
        // plain temp dir; the same invocation succeeds with the flag.
        cmd.arg("--skip-git-repo-check");
        cmd.args(sandbox_args(caps));
        cmd.args(effort_args(context.effort));
        // Live round 8: a caller-required reply schema, when one is scoped.
        // `_schema_file` is held to the end of this function on purpose — the
        // temp file must outlive the spawn.
        let (schema_args, _schema_file) =
            output_schema_args(crate::runtime_dispatch::output_schema_override().as_ref());
        if !schema_args.is_empty() {
            info!(
                agent = %context.agent_id,
                "CodexRuntime: constraining the reply with --output-schema"
            );
        }
        cmd.args(schema_args);
        // MCP registration. `self.codex_path` is the exact string this spawn
        // resolves through `PATH` — the same binary `duduclaw_core::which_codex`
        // finds (it shells out to `which` first), so probing it is strictly
        // more correct than probing a separately-resolved path. Probed once per
        // path per process; a failed probe means "old Codex", i.e. the
        // pre-existing argv behavior, never a failed spawn.
        let supports_env_vars = codex_supports_env_vars(&self.codex_path).await;
        let mcp_overrides =
            mcp_override_args_with(&context.agent_id, &context.home_dir, supports_env_vars);
        // Values first: on the `env_vars` branch Codex copies these out of its
        // OWN environment, so they must be set before it runs. They never
        // appear in `argv` (only their names do).
        for (k, v) in &mcp_overrides.process_env {
            cmd.env(k, v);
        }
        cmd.args(&mcp_overrides.args);
        // P2-B N4: this turn's or run's source identity for the duduclaw MCP
        // server, so what the employee stores there can be forgotten by its
        // conversation. Per-spawn `-c` overrides, never a persisted config.
        cmd.args(turn_source_override_args(
            &crate::memory_provenance::turn_source_env_pairs(),
        ));

        // Working root. Normally the agent's own directory; a caller may
        // override it via `super::SPAWN_OVERRIDE` (today: the team composer,
        // putting a role member in the employee's workspace so its files
        // outlive the throwaway scaffold — design §4.3 E3). Identity is NOT
        // affected: the `-c mcp_servers.duduclaw.env.DUDUCLAW_AGENT_ID`
        // override above already carries the member id independently of cwd.
        //
        // Resolved ONCE (it used to be read twice, so a `work_dir` that failed
        // validation could still have flipped `cwd_overridden`), through the
        // shared [`super::resolve_spawn_work_dir`] which validates the override
        // is a real directory and otherwise warns and falls back.
        let work_root: Option<std::path::PathBuf> =
            super::resolve_spawn_work_dir(context.agent_dir.as_deref(), &context.agent_id);
        let cwd_overridden = work_root.is_some() && work_root != context.agent_dir;

        // Pass system prompt via AGENTS.md in the working root.
        // Codex exec has no --instructions/--system-prompt flag (verified on
        // 0.156.1: `codex exec --help` lists neither); AGENTS.md at the
        // working root is the only file channel.
        //
        // With an overridden cwd that channel is unusable: writing the
        // member's system prompt to the employee's `AGENTS.md` would clobber
        // the employee's own file (and two concurrent members would race over
        // it). So when — and only when — the cwd is overridden, the system
        // prompt travels inside the prompt as an XML-delimited block instead.
        // Weaker placement than a real system prompt, and said out loud here
        // rather than silently dropping the member's instructions.
        let mut prompt_prefix = String::new();
        if !system_prompt.is_empty() {
            if cwd_overridden {
                prompt_prefix = format!(
                    "<role_system_prompt>\n{}\n</role_system_prompt>\n\n",
                    system_prompt.replace("</role_system_prompt>", "&lt;/role_system_prompt&gt;")
                );
            } else if let Some(ref dir) = context.agent_dir {
                let agents_md = dir.join("AGENTS.md");
                let _ = std::fs::write(&agents_md, system_prompt);
            }
        }

        // Prepend conversation history to prompt (Codex exec has no native multi-turn)
        let augmented_prompt = if context.conversation_history.is_empty() {
            safe_prompt
        } else {
            super::format_history_as_prompt(&context.conversation_history, &safe_prompt)
        };
        let augmented_prompt = if prompt_prefix.is_empty() {
            augmented_prompt
        } else {
            format!("{prompt_prefix}{augmented_prompt}")
        };

        cmd.arg(&augmented_prompt);

        // Set model if specified
        if !context.model.is_empty() {
            cmd.arg("-m").arg(&context.model);
        }

        // Set working directory
        if let Some(ref dir) = work_root {
            cmd.arg("--cd").arg(dir);
        }

        // Pass API key if available
        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        if !api_key.is_empty() {
            cmd.env("OPENAI_API_KEY", &api_key);
        }

        // Codex reads "additional input from stdin" whenever stdin is not a
        // TTY and concatenates it to the prompt; an inherited open pipe would
        // stall the run until EOF. The prompt is passed as an argument, so
        // stdin is deliberately closed.
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Native OS sandbox (opt-in). Layered on top of the CLI `--sandbox`
        // flag; fail-closed if required but unavailable. Scoped to the
        // working root, so an overridden cwd is the directory that gets
        // write access (a sandbox scoped to the scaffold would forbid exactly
        // the writes the override exists to allow).
        super::apply_native_sandbox(&mut cmd, caps, work_root.as_deref(), "codex")?;

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            cmd.output(),
        )
        .await
        .map_err(|_| "Codex CLI timed out".to_string())?
        .map_err(|e| format!("Failed to spawn codex: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Codex CLI exited with {}: {}",
                output.status,
                stderr.chars().take(500).collect::<String>()
            ));
        }

        // Parse JSONL output
        let stdout = String::from_utf8_lossy(&output.stdout);
        let (mut content, input_tokens, output_tokens, chunks) = parse_codex_stdout(&stdout);
        super::extend_native_tool_events(super::native_tool_events_from_chunks(&chunks));

        if content.is_empty() {
            // Fallback: use the last line as content
            content = stdout.lines().last().unwrap_or("").to_string();
        }

        // Still empty ⇒ FAILURE, not success: Ok("") would be silently dropped
        // by every channel and poison the session with an empty assistant turn.
        if content.trim().is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Empty response from Codex CLI (exit 0); stderr tail: {}",
                duduclaw_core::truncate_bytes(stderr.trim(), 300)
            ));
        }

        Ok(RuntimeResponse {
            content,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            model_used: context.model.clone(),
            runtime_name: "codex".to_string(),
        })
    }

    async fn is_available(&self) -> bool {
        tokio::process::Command::new(&self.codex_path)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

// ── Streaming ───────────────────────────────────────────────────

impl CodexRuntime {
    /// Execute and return chunks. Codex CLI does not support true streaming,
    /// so this wraps the normal execution into a single `Done` chunk.
    pub async fn execute_streaming(
        &self,
        prompt: &str,
        context: &super::RuntimeContext,
    ) -> Result<Vec<super::RuntimeChunk>, String> {
        let response = self.execute(prompt, context).await?;
        Ok(vec![super::RuntimeChunk::Done(response)])
    }
}

// ── MCP config ──────────────────────────────────────────────────

impl CodexRuntime {
    /// Render `[mcp_servers]` TOML deterministically (sorted server names and
    /// keys — a `HashMap` iteration order would make the idempotence check in
    /// [`Self::write_mcp_config`] flap between runs).
    fn render_mcp_toml(servers: &std::collections::HashMap<String, serde_json::Value>) -> String {
        // Shared with `mcp_override_args` so the file form and the `-c`
        // override form can never disagree about escaping (the local copy
        // this replaced escaped only `\` and `"`, leaving a control character
        // to produce an unparseable config.toml).
        let toml_string = toml_string_literal;
        let mut content = String::from("[mcp_servers]\n");
        let mut names: Vec<&String> = servers.keys().collect();
        names.sort();
        for name in names {
            let config = &servers[name];
            if name.contains('.') {
                content.push_str(&format!("[mcp_servers.{}]\n", toml_string(name)));
            } else {
                content.push_str(&format!("[mcp_servers.{name}]\n"));
            }
            let Some(obj) = config.as_object() else {
                continue;
            };
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            for k in keys {
                let v = &obj[k.as_str()];
                let toml_val = match v {
                    serde_json::Value::String(s) => format!("{k} = {}\n", toml_string(s)),
                    serde_json::Value::Array(arr) => {
                        let items: Vec<String> = arr
                            .iter()
                            .map(|item| {
                                if let Some(s) = item.as_str() {
                                    toml_string(s)
                                } else {
                                    item.to_string()
                                }
                            })
                            .collect();
                        format!("{k} = [{}]\n", items.join(", "))
                    }
                    serde_json::Value::Object(env) => {
                        // Inline table (used for the `env` map).
                        let mut env_keys: Vec<&String> = env.keys().collect();
                        env_keys.sort();
                        let pairs: Vec<String> = env_keys
                            .iter()
                            .filter_map(|ek| {
                                env[ek.as_str()]
                                    .as_str()
                                    .map(|ev| format!("{ek} = {}", toml_string(ev)))
                            })
                            .collect();
                        format!("{k} = {{ {} }}\n", pairs.join(", "))
                    }
                    _ => format!("{k} = {v}\n"),
                };
                content.push_str(&toml_val);
            }
        }
        content
    }

    /// Write MCP server configuration to the agent's codex config
    /// (`<agent_dir>/.codex/config.toml`). Idempotent: skips the write when the
    /// file already holds exactly the desired content. Returns `Ok(true)` when
    /// written, `Ok(false)` when already up to date.
    pub fn write_mcp_config(
        agent_dir: &std::path::Path,
        servers: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<bool, String> {
        let config_path = agent_dir.join(".codex").join("config.toml");
        let content = Self::render_mcp_toml(servers);
        if let Ok(existing) = std::fs::read_to_string(&config_path) {
            if existing == content {
                return Ok(false);
            }
        }
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&config_path, content).map_err(|e| e.to_string())?;
        // Carries DUDUCLAW_AGENT_TOKEN in plaintext — restrict to the owning
        // OS user (0600 on Unix; no-op on Windows).
        duduclaw_core::platform::set_owner_only(&config_path).ok();
        Ok(true)
    }

    /// W2: ensure the duduclaw MCP server (absolute binary + `mcp-server` arg +
    /// `DUDUCLAW_AGENT_ID` env) is registered in the agent's codex config.
    /// Called from [`AgentRuntime::execute`] before every spawn — cheap
    /// check-before-write keeps it idempotent.
    pub fn ensure_duduclaw_mcp_config(
        agent_dir: &std::path::Path,
        agent_id: &str,
        home_dir: &std::path::Path,
    ) -> Result<bool, String> {
        let Some(def) = super::duduclaw_mcp_server_json_for_home(agent_id, home_dir) else {
            return Err("duduclaw binary did not resolve to an absolute path".to_string());
        };
        let mut servers = std::collections::HashMap::new();
        servers.insert("duduclaw".to_string(), def);
        Self::write_mcp_config(agent_dir, &servers)
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_codex_event() {
        let line = r#"{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":50}}"#;
        let event: CodexEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.event_type, "turn.completed");
        let usage: CodexUsage =
            serde_json::from_value(event.extra.get("usage").unwrap().clone()).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
    }

    // ── T10: parse_codex_stdout → RuntimeChunk → NativeToolEvent ────────

    #[test]
    fn parse_codex_stdout_reads_agent_message_text_on_0_156() {
        // Real 0.156.1 stream shape (captured live 2026-09-24). The content must
        // be the agent message, never the trailing `turn.completed` line.
        let stdout = concat!(
            r#"{"type":"thread.started","thread_id":"t1"}"#,
            "\n",
            r#"{"type":"turn.started"}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"{\"verdict\":\"PASS\",\"reasons\":[]}"}}"#,
            "\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":14252,"cached_input_tokens":12160,"cache_write_input_tokens":0,"output_tokens":5,"reasoning_output_tokens":0}}"#,
            "\n",
        );
        let (content, _, _, _) = parse_codex_stdout(stdout);
        assert_eq!(content, r#"{"verdict":"PASS","reasons":[]}"#);
    }

    #[test]
    fn parse_codex_stdout_emits_tool_use_and_tool_result_chunks() {
        // The literal T10 ask: `RuntimeChunk::ToolUse`/`ToolResult` must
        // actually get constructed, not bypassed.
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"docs\nsrc\n","exit_code":0,"status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        assert_eq!(chunks.len(), 2);
        assert!(matches!(
            chunks[0],
            super::super::RuntimeChunk::ToolUse { .. }
        ));
        match &chunks[1] {
            super::super::RuntimeChunk::ToolResult { is_error, .. } => assert!(!is_error),
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn parse_codex_stdout_collects_command_execution_success() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"docs\nsrc\n","exit_code":0,"status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "shell");
        assert!(events[0].success);
    }

    #[test]
    fn parse_codex_stdout_collects_command_execution_failure() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc false","aggregated_output":"","exit_code":1,"status":"failed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "shell");
        assert!(!events[0].success);
    }

    #[test]
    fn parse_codex_stdout_collects_mcp_tool_call() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_5","type":"mcp_tool_call","server":"duduclaw","tool":"tasks_create","arguments":{"title":"x"},"result":{"content":[{"type":"text","text":"ok"}]},"error":null,"status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "tasks_create");
        assert!(events[0].success);
        assert_eq!(events[0].result_text.as_deref(), Some("ok"));
        assert!(events[0].input_text.as_deref().unwrap().contains("title"));
    }

    #[test]
    fn parse_codex_stdout_collects_mcp_tool_call_failure() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_6","type":"mcp_tool_call","server":"duduclaw","tool":"tasks_create","arguments":null,"result":null,"error":{"message":"boom"},"status":"failed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert!(!events[0].success);
        // R1: on failure, the item's `error.message` is captured as
        // `result_text` — dual-shape fallback from the documented
        // `result.content[].text` success shape.
        assert_eq!(events[0].result_text.as_deref(), Some("boom"));
    }

    // ── R1: result_text / input_text capture ─────────────────────────────

    #[test]
    fn parse_codex_stdout_command_execution_captures_aggregated_output() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"docs\nsrc\n","exit_code":0,"status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events[0].result_text.as_deref(), Some("docs\nsrc"));
        assert!(
            events[0]
                .input_text
                .as_deref()
                .unwrap()
                .contains("bash -lc ls")
        );
    }

    #[test]
    fn parse_codex_stdout_command_execution_empty_output_is_none() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc true","aggregated_output":"","exit_code":0,"status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert!(events[0].result_text.is_none());
    }

    #[test]
    fn parse_codex_stdout_masks_secret_in_command_execution_output() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"cat .env","aggregated_output":"ANTHROPIC_API_KEY=sk-ant-api03-verysecretvalue1234567890","exit_code":0,"status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        let result_text = events[0].result_text.as_deref().unwrap();
        assert!(
            !result_text.contains("sk-ant-api03-verysecretvalue1234567890"),
            "secret leaked into result_text: {result_text}"
        );
    }

    #[test]
    fn parse_codex_stdout_mixed_events_content_and_tools() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"ls","aggregated_output":"x","exit_code":0,"status":"completed"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_2","type":"mcp_tool_call","server":"duduclaw","tool":"memory_search","arguments":{},"result":{},"error":null,"status":"completed"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"message","content":[{"type":"output_text","text":"Hello world"}]}}"#,
            "\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#,
            "\n",
        );
        let (content, input_tokens, output_tokens, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(content, "Hello world");
        assert_eq!(input_tokens, 10);
        assert_eq!(output_tokens, 5);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].tool_name, "shell");
        assert_eq!(events[1].tool_name, "memory_search");
    }

    #[test]
    fn parse_codex_stdout_missing_tool_field_skips_not_fabricates() {
        // mcp_tool_call with no "tool" field — must be skipped, never
        // recorded under a fabricated "unknown" placeholder.
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_7","type":"mcp_tool_call","server":"duduclaw","status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        assert!(chunks.is_empty());
    }

    #[test]
    fn parse_codex_stdout_no_tool_items_is_empty_events() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"type":"message","content":[{"type":"output_text","text":"hi"}]}}"#,
            "\n",
        );
        let (content, _, _, chunks) = parse_codex_stdout(stdout);
        assert_eq!(content, "hi");
        assert!(chunks.is_empty());
    }

    fn caps(
        computer_use: bool,
        browser_via_bash: bool,
        allowed: &[&str],
        denied: &[&str],
    ) -> CapabilitiesConfig {
        CapabilitiesConfig {
            computer_use,
            browser_via_bash,
            allowed_tools: allowed.iter().map(|s| s.to_string()).collect(),
            denied_tools: denied.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    /// The sandbox level a `sandbox_args` vector declares: the `sandbox_mode`
    /// config override, the `-s <MODE>` flag, or `danger-full-access` when the
    /// explicit bypass flag is present (the only way to actually lift the
    /// sandbox on 0.156.1).
    fn declared_sandbox(args: &[String]) -> Option<String> {
        if args
            .iter()
            .any(|a| a == "--dangerously-bypass-approvals-and-sandbox")
        {
            return Some("danger-full-access".to_string());
        }
        args.windows(2)
            .find_map(|w| {
                (w[0] == "-c")
                    .then(|| w[1].strip_prefix("sandbox_mode="))
                    .flatten()
                    .map(|v| v.trim_matches('"').to_string())
            })
            .or_else(|| {
                args.windows(2)
                    .find(|w| w[0] == "-s" || w[0] == "--sandbox")
                    .map(|w| w[1].clone())
            })
    }

    #[test]
    fn sandbox_args_never_emit_the_nonexistent_ask_for_approval_flag() {
        // Regression (2026-09-24): `--ask-for-approval` does not exist on Codex
        // CLI 0.156.1 — `codex exec` exits 2 with "unexpected argument
        // '--ask-for-approval' found", so every codex spawn failed. Assert the
        // dead flag can never come back, at every sandbox level, and that
        // `-s/--sandbox` is never combined with `--approve-for-me` (a clap
        // mutual-exclusion error that would kill the spawn just as dead).
        for c in [
            caps(false, false, &[], &[]),               // workspace-write
            caps(false, false, &["Read", "Grep"], &[]), // read-only
            caps(true, false, &[], &[]),                // danger-full-access
        ] {
            let args = sandbox_args(Some(&c));
            assert!(
                !args.iter().any(|a| a == "--ask-for-approval"),
                "resurrected a flag Codex 0.156.1 rejects: {args:?}"
            );
            let declares_sandbox_flag = args.iter().any(|a| a == "-s" || a == "--sandbox");
            let approves = args.iter().any(|a| a == "--approve-for-me");
            assert!(
                !(declares_sandbox_flag && approves),
                "`-s/--sandbox` and `--approve-for-me` are mutually exclusive on 0.156.1: {args:?}"
            );
            let level = declared_sandbox(&args).expect("a sandbox level is always declared");
            if level == "danger-full-access" {
                // Explicit operator opt-in: the bypass flag is the only way to
                // lift the sandbox, and it must stand alone.
                assert_eq!(args, vec!["--dangerously-bypass-approvals-and-sandbox"]);
            } else {
                assert!(
                    args.windows(2)
                        .any(|w| w[0] == "-c" && w[1] == "approval_policy=never"),
                    "approval policy missing: {args:?}"
                );
                assert!(
                    !args
                        .iter()
                        .any(|a| a == "--dangerously-bypass-approvals-and-sandbox"),
                    "bypass flag leaked into a sandboxed level: {args:?}"
                );
            }
        }
    }

    /// Regression (2026-09-28 review, decided option A): `ReadOnly` used to
    /// fail OPEN. It was spawned with `--approve-for-me` + `-c
    /// sandbox_mode="read-only"`, and `--approve-for-me` runs its automatic
    /// review *in a workspace-write sandbox* — so a capability-restricted agent
    /// declared read-only could still write files. The level must now carry the
    /// real `-s read-only` flag, which by construction excludes
    /// `--approve-for-me`.
    #[test]
    fn sandbox_args_read_only_really_blocks_writes_not_just_declares_it() {
        let args = sandbox_args(Some(&caps(false, false, &["Read", "Grep"], &[])));
        assert_eq!(
            args,
            vec!["-s", "read-only", "-c", "approval_policy=never"],
            "read-only must spawn with the enforcing flag, not an advisory config key"
        );
        assert!(
            !args.iter().any(|a| a == "--approve-for-me"),
            "`--approve-for-me` silently re-grants workspace-write: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a.starts_with("sandbox_mode=")),
            "the advisory config-key form must not linger alongside the real flag: {args:?}"
        );
    }

    /// The other two levels are unchanged by the ReadOnly fix — `WorkspaceWrite`
    /// still needs `--approve-for-me` for MCP tool calls to be allowed at all.
    #[test]
    fn sandbox_args_workspace_write_keeps_the_automatic_approver() {
        let args = sandbox_args(Some(&caps(false, false, &[], &[])));
        assert!(args.iter().any(|a| a == "--approve-for-me"), "{args:?}");
        assert!(!args.iter().any(|a| a == "-s"), "{args:?}");
        assert_eq!(
            sandbox_args(Some(&caps(true, false, &[], &[]))),
            vec!["--dangerously-bypass-approvals-and-sandbox"]
        );
    }

    #[test]
    fn effort_args_are_empty_when_none_and_clamped_to_xhigh_for_codex() {
        use duduclaw_core::effort::Effort;

        // None ⇒ byte-identical argv (no `-c model_reasoning_effort=`).
        assert!(effort_args(None).is_empty());

        assert_eq!(
            effort_args(Some(Effort::Low)),
            vec!["-c", "model_reasoning_effort=low"]
        );
        assert_eq!(
            effort_args(Some(Effort::XHigh)),
            vec!["-c", "model_reasoning_effort=xhigh"]
        );
        // Codex 0.156.1 has no `max` — clamp down rather than send a value the
        // CLI would reject.
        assert_eq!(
            effort_args(Some(Effort::Max)),
            vec!["-c", "model_reasoning_effort=xhigh"]
        );
    }

    // ── Live round 8: `--output-schema` structured judge output ──────────

    #[test]
    fn no_output_schema_means_no_flag() {
        let (args, file) = output_schema_args(None);
        assert!(
            args.is_empty(),
            "argv must be byte-identical without a schema"
        );
        assert!(file.is_none());
    }

    #[test]
    fn an_output_schema_is_staged_to_a_file_the_flag_points_at() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "decision": { "type": "string", "enum": ["continue"] } },
            "required": ["decision"],
        });
        let (args, file) = output_schema_args(Some(&schema));
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "--output-schema");
        // The temp file is returned, not dropped — dropping it here would
        // delete the schema before codex ever opened it.
        let handle = file.expect("the schema file must outlive this call");
        assert_eq!(handle.path().to_string_lossy(), args[1]);
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
        assert_eq!(written, schema, "the file must carry the schema verbatim");
    }

    /// The flag has to coexist with the rest of the 0.156.1 argv — it is added
    /// alongside `--approve-for-me` / `-c sandbox_mode=…`, never in place of
    /// the `--sandbox` shape that CLI rejects.
    #[test]
    fn output_schema_composes_with_the_sandbox_and_effort_flags() {
        let schema = serde_json::json!({"type": "object"});
        let (schema_args, _file) = output_schema_args(Some(&schema));
        let mut argv: Vec<String> = vec![
            "exec".into(),
            "--json".into(),
            "--skip-git-repo-check".into(),
        ];
        argv.extend(sandbox_args(Some(&caps(false, false, &[], &[]))));
        argv.extend(effort_args(Some(duduclaw_core::effort::Effort::Medium)));
        argv.extend(schema_args);
        assert!(argv.iter().any(|a| a == "--output-schema"));
        assert!(argv.iter().any(|a| a == "--approve-for-me"));
        assert!(!argv.iter().any(|a| a == "--sandbox"));
        assert!(argv.iter().any(|a| a == "model_reasoning_effort=medium"));
    }

    #[test]
    fn sandbox_args_default_caps_is_workspace_write() {
        // Default caps (empty allowlist ⇒ full default toolset incl. Bash/Write)
        // keep the write scope --full-auto used to grant — workspace-write.
        let c = caps(false, false, &[], &[]);
        let args = sandbox_args(Some(&c));
        assert_eq!(
            args,
            vec![
                "--approve-for-me",
                "-c",
                "approval_policy=never",
                "-c",
                "sandbox_mode=\"workspace-write\""
            ]
        );
    }

    #[test]
    fn sandbox_args_none_caps_keeps_legacy_workspace_write() {
        let args = sandbox_args(None);
        assert_eq!(
            args,
            vec![
                "--approve-for-me",
                "-c",
                "approval_policy=never",
                "-c",
                "sandbox_mode=\"workspace-write\""
            ]
        );
    }

    #[test]
    fn sandbox_args_read_only_when_allowlist_has_no_write_tools() {
        let c = caps(false, false, &["Read", "Grep", "WebSearch"], &[]);
        let args = sandbox_args(Some(&c));
        assert_eq!(declared_sandbox(&args).as_deref(), Some("read-only"));
    }

    #[test]
    fn sandbox_args_read_only_when_all_write_tools_denied() {
        let c = caps(
            false,
            false,
            &[],
            &["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit"],
        );
        assert_eq!(
            declared_sandbox(&sandbox_args(Some(&c))).as_deref(),
            Some("read-only")
        );
    }

    #[test]
    fn sandbox_args_full_access_only_on_explicit_computer_use() {
        let c = caps(true, false, &[], &[]);
        assert_eq!(
            declared_sandbox(&sandbox_args(Some(&c))).as_deref(),
            Some("danger-full-access")
        );
    }

    #[test]
    fn sandbox_args_browser_via_bash_forces_workspace_write() {
        // A read-only allowlist + browser_via_bash still needs bash → not read-only.
        let c = caps(false, true, &["Read"], &[]);
        assert_eq!(
            declared_sandbox(&sandbox_args(Some(&c))).as_deref(),
            Some("workspace-write")
        );
    }

    #[test]
    fn sandbox_args_qualified_bash_allow_counts_as_write() {
        // `Bash(git:*)` is an anchored token grant of (scoped) Bash — must not
        // collapse to read-only, but must never escalate past workspace-write.
        let c = caps(false, false, &["Read", "Bash(git:*)"], &[]);
        assert_eq!(
            declared_sandbox(&sandbox_args(Some(&c))).as_deref(),
            Some("workspace-write")
        );
    }

    #[test]
    fn mcp_config_write_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut servers = std::collections::HashMap::new();
        servers.insert(
            "duduclaw".to_string(),
            serde_json::json!({
                "command": "/usr/local/bin/duduclaw",
                "args": ["mcp-server"],
                "env": { "DUDUCLAW_AGENT_ID": "agnes" },
            }),
        );
        assert!(CodexRuntime::write_mcp_config(dir.path(), &servers).unwrap());
        // Second call: identical content → no write reported.
        assert!(!CodexRuntime::write_mcp_config(dir.path(), &servers).unwrap());

        let content =
            std::fs::read_to_string(dir.path().join(".codex").join("config.toml")).unwrap();
        assert!(content.contains("[mcp_servers.duduclaw]"));
        assert!(content.contains("command = \"/usr/local/bin/duduclaw\""));
        assert!(content.contains("args = [\"mcp-server\"]"));
        assert!(content.contains("DUDUCLAW_AGENT_ID = \"agnes\""));
    }

    #[test]
    fn mcp_override_args_carry_agent_id_env() {
        // resolve_duduclaw_bin falls back to current_exe (absolute in tests),
        // so overrides should materialize with the agent-id env override.
        let eval_home = std::path::PathBuf::from("/tmp/duduclaw-eval-arm");
        let args = mcp_override_args("agnes", &eval_home);
        if args.is_empty() {
            return; // binary not resolvable to an absolute path in this env
        }
        assert!(
            args.iter()
                .any(|a| a == r#"mcp_servers.duduclaw.args=["mcp-server"]"#)
        );
        // QUOTED — codex parses the value half as TOML, so the bare form was
        // a type error for anything that parses as a non-string.
        assert!(
            args.iter()
                .any(|a| a == r#"mcp_servers.duduclaw.env.DUDUCLAW_AGENT_ID="agnes""#),
            "agent id env override missing or unquoted: {args:?}"
        );
        assert!(args.iter().any(|a| {
            a == r#"mcp_servers.duduclaw.env.DUDUCLAW_HOME="/tmp/duduclaw-eval-arm""#
        }));
        // The command path is a TOML string too (an unquoted absolute path
        // only ever survived because it fails to parse as TOML at all).
        assert!(
            args.iter()
                .any(|a| a.starts_with("mcp_servers.duduclaw.command=\"") && a.ends_with('"')),
            "command override not a quoted TOML string: {args:?}"
        );
    }

    /// Live round 5: the member saw the duduclaw tools and every call came
    /// back `MCP tool call requires approval, but approval policy is never`.
    /// The per-server approval mode is what makes a non-interactive run able
    /// to actually *call* the tools it was shown.
    #[test]
    fn mcp_override_args_auto_approve_the_duduclaw_server() {
        let args = mcp_override_args("agnes", &duduclaw_core::duduclaw_home());
        if args.is_empty() {
            return; // binary not resolvable to an absolute path in this env
        }
        assert!(
            args.iter()
                .any(|a| a == r#"mcp_servers.duduclaw.default_tools_approval_mode="auto""#),
            "approval-mode override missing or unquoted: {args:?}"
        );
        // Every override travels on the same `-c` channel, so the flag count
        // and the payload count stay in lockstep.
        assert_eq!(
            args.iter().filter(|a| a.as_str() == "-c").count(),
            args.len() / 2,
            "every override must be a (-c, payload) pair: {args:?}"
        );
        // It is a TOML *string*, like every other scalar codex parses.
        let pair = args
            .iter()
            .find(|a| a.starts_with("mcp_servers.duduclaw.default_tools_approval_mode="))
            .unwrap();
        let value = pair.split_once('=').unwrap().1;
        let parsed: toml::Value = format!("v = {value}").parse().unwrap();
        assert_eq!(parsed["v"].as_str().unwrap(), "auto");
    }

    /// The approval mode is scoped to OUR server: it must never be emitted as
    /// a global `approval_policy` relaxation, and it must not disturb the
    /// sandbox level (`sandbox_args` stays the only writer of `--sandbox`).
    #[test]
    fn approval_mode_is_per_server_not_a_global_policy_relaxation() {
        let args = mcp_override_args("agnes", &duduclaw_core::duduclaw_home());
        if args.is_empty() {
            return;
        }
        assert!(
            !args.iter().any(|a| a.starts_with("approval_policy=")),
            "per-server approval mode must not touch the global policy: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a.contains("sandbox")),
            "MCP overrides must not touch the sandbox level: {args:?}"
        );
        assert!(
            !args
                .iter()
                .any(|a| a.contains("dangerously-bypass-approvals-and-sandbox")),
            "{args:?}"
        );
        // The global policy is still `never`, emitted by sandbox_args alone.
        assert!(
            sandbox_args(None)
                .iter()
                .any(|a| a == "approval_policy=never"),
            "global approval policy must stay `never`"
        );
    }

    /// P2-B N4: turn/run source pairs become `env.<K>` overrides.
    #[test]
    fn turn_source_pairs_become_env_overrides() {
        let args = turn_source_override_args(&[
            ("DUDUCLAW_TURN_ID".into(), "t-1".into()),
            ("DUDUCLAW_DISPATCH_RUN_ID".into(), "abc".into()),
        ]);
        assert_eq!(args.len(), 4);
        assert_eq!(args.iter().filter(|a| *a == "-c").count(), 2);
        for want in [
            "mcp_servers.duduclaw.env.DUDUCLAW_DISPATCH_RUN_ID=\"abc\"",
            "mcp_servers.duduclaw.env.DUDUCLAW_TURN_ID=\"t-1\"",
        ] {
            assert!(args.iter().any(|a| a == want), "{args:?}");
        }
        assert!(turn_source_override_args(&[]).is_empty());
    }

    /// P2-B N4: inside a turn, the pairs the codex spawn adds name it.
    #[tokio::test]
    async fn the_codex_spawn_carries_the_turn_in_scope() {
        let args = duduclaw_memory::feedback::CURRENT_TURN_ID
            .scope(Some("t-5".into()), async {
                turn_source_override_args(&crate::memory_provenance::turn_source_env_pairs())
            })
            .await;
        assert!(
            args.iter().any(|a| a == "mcp_servers.duduclaw.env.DUDUCLAW_TURN_ID=\"t-5\""),
            "{args:?}"
        );
    }

    /// The live round 3 spawn-killer: a numeric-looking port in the `.mcp.json`
    /// env block was emitted bare, codex parsed it as a TOML integer, and every
    /// codex spawn died with `invalid type: integer 18999, expected a string`.
    #[test]
    fn mcp_env_override_args_quote_every_value_type() {
        let env: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(serde_json::json!({
                "DUDUCLAW_PORT": 18999,
                "DUDUCLAW_HOME": "/tmp/home",
                "DUDUCLAW_FLAG": true,
                "DUDUCLAW_LIST": ["a"],
                "DUDUCLAW_NULL": serde_json::Value::Null,
            }))
            .unwrap();
        let args = mcp_env_override_args(&env);
        assert!(
            args.iter()
                .any(|a| a == r#"mcp_servers.duduclaw.env.DUDUCLAW_PORT="18999""#),
            "{args:?}"
        );
        assert!(
            args.iter()
                .any(|a| a == r#"mcp_servers.duduclaw.env.DUDUCLAW_HOME="/tmp/home""#),
            "{args:?}"
        );
        assert!(
            args.iter()
                .any(|a| a == r#"mcp_servers.duduclaw.env.DUDUCLAW_FLAG="true""#),
            "{args:?}"
        );
        // Non-scalars have no environment-variable meaning.
        assert!(
            !args.iter().any(|a| a.contains("DUDUCLAW_LIST")),
            "{args:?}"
        );
        assert!(
            !args.iter().any(|a| a.contains("DUDUCLAW_NULL")),
            "{args:?}"
        );
        // Every emitted value parses back as a TOML *string*, which is the
        // type codex's `mcp_servers.<id>.env` table demands.
        for pair in args.iter().filter(|a| a.starts_with("mcp_servers.")) {
            let value = pair.split_once('=').unwrap().1;
            let parsed: toml::Value = format!("v = {value}").parse().unwrap();
            assert!(
                parsed["v"].is_str(),
                "{pair} did not round-trip as a string"
            );
        }
    }

    /// 2026-09-28 review, latent-surface closure: [`toml_string_literal`] has
    /// always guarded the VALUE half of a `-c key=value` override; the key half
    /// was interpolated raw, so a key carrying `=`, a quote, a dot or
    /// whitespace could retarget the override at a different config path or
    /// produce an unparseable payload.
    #[test]
    fn mcp_env_override_args_drop_keys_that_are_not_bare_toml_keys() {
        let env: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(serde_json::json!({
                "GOOD_KEY-1": "ok",
                "BAD=KEY": "x",
                "BAD\"KEY": "x",
                "BAD KEY": "x",
                "BAD.KEY": "x",
                "BAD\nKEY": "x",
                "": "x",
            }))
            .unwrap();
        let args = mcp_env_override_args(&env);
        assert!(
            args.iter()
                .any(|a| a == r#"mcp_servers.duduclaw.env.GOOD_KEY-1="ok""#),
            "{args:?}"
        );
        for bad in ["BAD=KEY", "BAD\"KEY", "BAD KEY", "BAD.KEY", "BAD\nKEY"] {
            assert!(
                !args.iter().any(|a| a.contains(bad)),
                "unsafe key {bad:?} reached the argv: {args:?}"
            );
        }
        // Every surviving payload is still a well-formed `key = value` TOML
        // assignment (the whole point of validating the key half).
        for pair in args.iter().filter(|a| a.starts_with("mcp_servers.")) {
            let parsed: Result<toml::Value, _> = pair.parse();
            assert!(parsed.is_ok(), "{pair} is not parseable TOML");
        }
    }

    #[test]
    fn is_safe_config_key_segment_accepts_env_names_and_refuses_the_rest() {
        for ok in ["DUDUCLAW_HOME", "A", "a-b", "X9_z"] {
            assert!(is_safe_config_key_segment(ok), "{ok}");
        }
        for bad in ["", "a.b", "a=b", "a b", "a\"b", "a\nb", "a/b", "客戶"] {
            assert!(!is_safe_config_key_segment(bad), "{bad:?}");
        }
    }

    /// The codex `-c` channel is the ONLY way a credential reaches a codex-
    /// spawned MCP server — live-probed 2026-09-28 (evidence:
    /// `commercial/evidence/team-as-agent-2026-09-28/codex-env-passthrough/`):
    /// codex `env_clear()`s every MCP child and re-adds an 11-name allowlist
    /// plus the config-declared `env` map, so the gateway's own process env
    /// does not reach it. That means `DUDUCLAW_MCP_API_KEY` /
    /// `DUDUCLAW_AGENT_TOKEN` currently travel in argv, readable by `ps` on the
    /// same host, and the exposure is documented in
    /// `docs/features/13-multi-runtime.md` rather than fixed.
    ///
    /// What this test locks is that the exposure cannot GROW silently: the set
    /// of env keys that may appear in argv is exactly the known `DUDUCLAW_*`
    /// block. A new secret added to `duduclaw_mcp_server_json_for_home` has to
    /// come past this assertion and past whoever reads it.
    #[test]
    fn only_the_known_env_keys_may_reach_the_codex_argv() {
        const KNOWN: &[&str] = &[
            "DUDUCLAW_AGENT_ID",
            "DUDUCLAW_AGENT_TOKEN",
            "DUDUCLAW_HOME",
            "DUDUCLAW_PORT",
            "DUDUCLAW_INSTANCE",
            "DUDUCLAW_MCP_API_KEY",
            "DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED",
        ];
        let args = mcp_override_args("agnes", &duduclaw_core::duduclaw_home());
        if args.is_empty() {
            return; // binary not resolvable to an absolute path in this env
        }
        for pair in args
            .iter()
            .filter_map(|a| a.strip_prefix("mcp_servers.duduclaw.env."))
        {
            let key = pair.split_once('=').expect("an override is key=value").0;
            assert!(
                KNOWN.contains(&key),
                "{key} newly reaches the codex argv (world-readable via `ps`). \
                 If it carries a secret, do not add it here — see the codex \
                 credential-exposure section of docs/features/13-multi-runtime.md"
            );
        }
    }

    // ── `env_vars` capability gate (2026-09-28) ─────────────────

    /// The point of the whole change: on an `env_vars`-capable Codex no
    /// credential VALUE may appear in `argv` — only its name — and the value
    /// must instead be handed to the Codex process environment.
    #[test]
    fn env_vars_branch_keeps_credential_values_out_of_the_argv() {
        let home = duduclaw_core::duduclaw_home();
        let overrides = mcp_override_args_with("agnes", &home, true);
        if overrides.args.is_empty() {
            return; // binary not resolvable to an absolute path in this env
        }
        // Every secret-shaped key the production env block carries is in the
        // `env_vars` array and in the process env, and in NEITHER case is its
        // value in argv.
        let env_vars_arg = overrides
            .args
            .iter()
            .find(|a| a.starts_with("mcp_servers.duduclaw.env_vars="));
        let secret_pairs: Vec<&(String, String)> = overrides.process_env.iter().collect();
        for (k, v) in &secret_pairs {
            assert!(is_secret_env_name(k), "{k} is not credential-shaped");
            let arg = env_vars_arg.expect("a non-empty process_env implies an env_vars array");
            assert!(arg.contains(&format!("\"{k}\"")), "{k} missing from {arg}");
            // The name is in argv; the value is not, anywhere.
            assert!(
                !v.is_empty(),
                "a credential with an empty value would make this test vacuous"
            );
            for a in &overrides.args {
                assert!(
                    !a.contains(v.as_str()),
                    "credential value for {k} reached the codex argv"
                );
            }
            // ...and never through the `env.<K>=` form either.
            assert!(
                !overrides
                    .args
                    .iter()
                    .any(|a| a.starts_with(&format!("mcp_servers.duduclaw.env.{k}="))),
                "{k} is still emitted through the argv `env` table"
            );
        }
        // Non-credential keys keep the `env.<K>` form.
        assert!(
            overrides
                .args
                .iter()
                .any(|a| a.starts_with("mcp_servers.duduclaw.env.DUDUCLAW_AGENT_ID=")),
            "{:?}",
            overrides.args
        );
        assert!(
            overrides.process_env.iter().all(|(k, _)| k != "DUDUCLAW_HOME"),
            "a non-credential key must not be routed through the process env"
        );
        // The (-c, payload) pairing invariant survives the new override.
        assert_eq!(
            overrides.args.iter().filter(|a| a.as_str() == "-c").count(),
            overrides.args.len() / 2,
            "every override must be a (-c, payload) pair: {:?}",
            overrides.args
        );
    }

    /// Synthetic env map so the assertion does not depend on which credentials
    /// happen to exist in this process — and so the `env_vars` array's TOML
    /// shape can be checked exactly.
    #[test]
    fn split_env_overrides_puts_only_names_in_argv_and_values_in_the_process_env() {
        let env: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(serde_json::json!({
                "DUDUCLAW_MCP_API_KEY": "s3cr3t-key-value",
                "DUDUCLAW_AGENT_TOKEN": "s3cr3t-token-value",
                "DUDUCLAW_HOME": "/tmp/home",
                "DUDUCLAW_PORT": 18999,
            }))
            .unwrap();
        let (args, process_env) = split_env_overrides(&env);

        // 1. No credential VALUE anywhere in the argv.
        for secret in ["s3cr3t-key-value", "s3cr3t-token-value"] {
            assert!(
                !args.iter().any(|a| a.contains(secret)),
                "credential value reached the argv: {args:?}"
            );
        }
        // 2. The names travel as a TOML string array.
        let arg = args
            .iter()
            .find(|a| a.starts_with("mcp_servers.duduclaw.env_vars="))
            .expect("env_vars array missing");
        let parsed: toml::Value = format!("v = {}", arg.split_once('=').unwrap().1)
            .parse()
            .unwrap();
        let got: Vec<&str> = parsed["v"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(got.contains(&"DUDUCLAW_MCP_API_KEY"), "{got:?}");
        assert!(got.contains(&"DUDUCLAW_AGENT_TOKEN"), "{got:?}");
        assert!(!got.contains(&"DUDUCLAW_HOME"), "{got:?}");
        assert!(!got.contains(&"DUDUCLAW_PORT"), "{got:?}");
        // 3. The values reach the codex process env instead. (Sorted: the
        //    serde_json map type — and therefore iteration order — depends on
        //    whether `preserve_order` is enabled anywhere in the dep graph.)
        let mut got_env = process_env.clone();
        got_env.sort();
        assert_eq!(
            got_env,
            vec![
                (
                    "DUDUCLAW_AGENT_TOKEN".to_string(),
                    "s3cr3t-token-value".to_string()
                ),
                (
                    "DUDUCLAW_MCP_API_KEY".to_string(),
                    "s3cr3t-key-value".to_string()
                ),
            ],
            "credential values must be handed to the codex process env"
        );
        // 4. Non-credentials keep the previous `env.<K>="<value>"` shape,
        //    quoting included (the live round-3 spawn-killer).
        assert!(
            args.iter()
                .any(|a| a == r#"mcp_servers.duduclaw.env.DUDUCLAW_HOME="/tmp/home""#),
            "{args:?}"
        );
        assert!(
            args.iter()
                .any(|a| a == r#"mcp_servers.duduclaw.env.DUDUCLAW_PORT="18999""#),
            "{args:?}"
        );
    }

    /// A credential-shaped key that is not a bare TOML key is dropped from
    /// BOTH halves — it can neither be named in the array nor smuggled onto
    /// the codex process env.
    #[test]
    fn split_env_overrides_drops_unsafe_credential_key_names() {
        let env: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(serde_json::json!({
                "BAD.KEY_TOKEN": "x",
                "BAD KEY_SECRET": "x",
                "GOOD_TOKEN": "ok",
            }))
            .unwrap();
        let (args, process_env) = split_env_overrides(&env);
        for bad in ["BAD.KEY_TOKEN", "BAD KEY_SECRET"] {
            assert!(!args.iter().any(|a| a.contains(bad)), "{args:?}");
            assert!(!process_env.iter().any(|(k, _)| k == bad), "{process_env:?}");
        }
        assert_eq!(
            process_env,
            vec![("GOOD_TOKEN".to_string(), "ok".to_string())]
        );
    }

    /// Regression guard for the fallback: an old Codex must see EXACTLY the
    /// argv it saw before this change, and nothing on the process env.
    #[test]
    fn unsupported_branch_is_byte_identical_to_the_legacy_argv() {
        let home = duduclaw_core::duduclaw_home();
        let legacy = mcp_override_args("agnes", &home);
        let overrides = mcp_override_args_with("agnes", &home, false);
        assert_eq!(overrides.args, legacy);
        assert!(
            overrides.process_env.is_empty(),
            "the fallback branch must not touch the codex process env"
        );
        if legacy.is_empty() {
            return; // binary not resolvable to an absolute path in this env
        }
        // ...and it is still the credentials-in-argv shape, so the exposure
        // note in docs/features/13-multi-runtime.md stays accurate for it.
        assert!(
            !legacy
                .iter()
                .any(|a| a.starts_with("mcp_servers.duduclaw.env_vars=")),
            "{legacy:?}"
        );
    }

    /// The `only_the_known_env_keys_may_reach_the_codex_argv` invariant, held
    /// on the new branch too: a newly-added key may only reach argv if it is on
    /// the known list, credential-shaped ones as NAMES inside `env_vars`.
    #[test]
    fn only_the_known_env_keys_may_reach_the_codex_argv_on_the_env_vars_branch() {
        const KNOWN: &[&str] = &[
            "DUDUCLAW_AGENT_ID",
            "DUDUCLAW_AGENT_TOKEN",
            "DUDUCLAW_HOME",
            "DUDUCLAW_PORT",
            "DUDUCLAW_INSTANCE",
            "DUDUCLAW_MCP_API_KEY",
            "DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED",
        ];
        let overrides = mcp_override_args_with("agnes", &duduclaw_core::duduclaw_home(), true);
        if overrides.args.is_empty() {
            return;
        }
        for pair in overrides
            .args
            .iter()
            .filter_map(|a| a.strip_prefix("mcp_servers.duduclaw.env."))
        {
            let key = pair.split_once('=').expect("an override is key=value").0;
            assert!(KNOWN.contains(&key), "{key} newly reaches the codex argv");
        }
        for (k, _) in &overrides.process_env {
            assert!(
                KNOWN.contains(&k.as_str()),
                "{k} newly reaches the codex process env"
            );
        }
    }

    #[test]
    fn is_secret_env_name_matches_exact_suffixes_case_insensitively() {
        for ok in [
            "DUDUCLAW_MCP_API_KEY",
            "DUDUCLAW_AGENT_TOKEN",
            "X_SECRET",
            "DB_PASSWORD",
            "db_password",
            "Some_Api_Key",
        ] {
            assert!(is_secret_env_name(ok), "{ok}");
        }
        for bad in [
            "DUDUCLAW_HOME",
            "DUDUCLAW_PORT",
            "DUDUCLAW_AGENT_ID",
            // Not a suffix match — substrings must not trip it.
            "TOKEN_BUDGET",
            "API_KEY_PATH",
            "",
        ] {
            assert!(!is_secret_env_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn codex_version_parsing_gates_on_0_157_0() {
        assert_eq!(
            parse_codex_version("codex-cli 0.157.1"),
            Some((0, 157, 1)),
            "the live-probed local output must parse"
        );
        assert!(version_supports_env_vars("codex-cli 0.157.1"));
        assert!(version_supports_env_vars("codex-cli 0.157.0"));
        assert!(version_supports_env_vars("codex-cli 0.158.0"));
        assert!(version_supports_env_vars("codex-cli 1.0.0"));
        // Older CLIs: the `env_vars` key is not known to be accepted.
        assert!(!version_supports_env_vars("codex-cli 0.156.9"));
        assert!(!version_supports_env_vars("codex-cli 0.156.1"));
        assert!(!version_supports_env_vars("codex-cli 0.9.99"));
        // Garbage / unparseable ⇒ false, never a guess.
        assert!(!version_supports_env_vars(""));
        assert!(!version_supports_env_vars("codex-cli"));
        assert!(!version_supports_env_vars("error: command not found"));
        assert!(!version_supports_env_vars("codex-cli 0.157"));
        assert!(!version_supports_env_vars("codex-cli vNEXT"));
        // Pre-release / build metadata is tolerated.
        assert_eq!(
            parse_codex_version("codex-cli 0.158.0-rc.1"),
            Some((0, 158, 0))
        );
        assert!(version_supports_env_vars("codex-cli v0.157.2+build7"));
    }

    /// The probe is a subprocess spawn on a hot path — it must run at most
    /// once per binary path for the life of the process.
    #[tokio::test]
    async fn env_vars_support_probe_runs_once_per_binary_path() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        // Unique key: the cache is process-global and shared with other tests.
        let bin = "/nonexistent/codex-probe-once-test";
        for _ in 0..3 {
            let supported = cached_env_vars_support(bin, || async {
                CALLS.fetch_add(1, Ordering::SeqCst);
                Some("codex-cli 0.157.1".to_string())
            })
            .await;
            assert!(supported);
        }
        assert_eq!(CALLS.load(Ordering::SeqCst), 1, "probe was re-run");

        // A different path is probed independently.
        static OTHER_CALLS: AtomicUsize = AtomicUsize::new(0);
        let other = "/nonexistent/codex-probe-once-test-old";
        for _ in 0..2 {
            let supported = cached_env_vars_support(other, || async {
                OTHER_CALLS.fetch_add(1, Ordering::SeqCst);
                Some("codex-cli 0.156.1".to_string())
            })
            .await;
            assert!(!supported, "0.156.1 must fall back to the argv shape");
        }
        assert_eq!(OTHER_CALLS.load(Ordering::SeqCst), 1);
    }

    /// A probe that cannot answer (binary missing, timeout, non-zero exit)
    /// must degrade to the old behavior, never to a failed spawn.
    #[tokio::test]
    async fn a_failed_version_probe_falls_back_to_the_argv_shape() {
        let bin = "/nonexistent/codex-probe-failure-test";
        assert!(!cached_env_vars_support(bin, || async { None }).await);
        // The real probe against a path that does not exist behaves the same.
        assert!(
            probe_codex_version_output("/nonexistent/codex-really-absent")
                .await
                .is_none()
        );
        assert!(!codex_supports_env_vars("/nonexistent/codex-really-absent").await);
    }

    #[test]
    fn toml_string_literal_escapes_quotes_backslashes_and_controls() {
        assert_eq!(toml_string_literal("plain"), "\"plain\"");
        assert_eq!(toml_string_literal("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(toml_string_literal("C:\\tmp"), "\"C:\\\\tmp\"");
        assert_eq!(toml_string_literal("a\nb\tc"), "\"a\\nb\\tc\"");
        assert_eq!(toml_string_literal("a\u{1}b"), "\"a\\u0001b\"");
        // Non-ASCII stays verbatim (TOML basic strings are UTF-8).
        assert_eq!(toml_string_literal("客戶"), "\"客戶\"");
        // Everything above must parse back to the original string.
        for raw in [
            "plain",
            "say \"hi\"",
            "C:\\tmp",
            "a\nb\tc",
            "a\u{1}b",
            "客戶",
        ] {
            let parsed: toml::Value = format!("v = {}", toml_string_literal(raw)).parse().unwrap();
            assert_eq!(parsed["v"].as_str().unwrap(), raw);
        }
    }

    #[test]
    fn test_parse_item_completed() {
        let line = r#"{"type":"item.completed","item":{"type":"message","content":[{"type":"output_text","text":"Hello world"}]}}"#;
        let event: CodexEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.event_type, "item.completed");
        let text = event
            .extra
            .get("item")
            .unwrap()
            .get("content")
            .unwrap()
            .as_array()
            .unwrap()[0]
            .get("text")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(text, "Hello world");
    }

    /// Live round 3 E3: a codex role member's working root is the employee's
    /// workspace, but its MCP identity stays its own — the `-c` env overrides
    /// are keyed by agent id and are entirely independent of `--cd`.
    #[tokio::test]
    async fn spawn_work_dir_override_moves_the_working_root_not_the_identity() {
        let member = "eph-agnes-r1-executor-abc123";
        let home = duduclaw_core::duduclaw_home();
        let baseline = mcp_override_args(member, &home);
        if baseline.is_empty() {
            return; // binary not resolvable to an absolute path in this env
        }
        let ws = std::path::PathBuf::from("/tmp/duduclaw-test/agents/agnes");
        super::super::SPAWN_OVERRIDE
            .scope(
                super::super::SpawnOverride {
                    work_dir: Some(ws.clone()),
                },
                async {
                    // The cwd the runtime would pass to `--cd`.
                    assert_eq!(super::super::spawn_work_dir_override(), Some(ws.clone()));
                    // The identity overrides are unchanged by the cwd move.
                    assert_eq!(mcp_override_args(member, &home), baseline);
                    assert!(
                        baseline.iter().any(|a| a
                            == &format!("mcp_servers.duduclaw.env.DUDUCLAW_AGENT_ID=\"{member}\"")),
                        "{baseline:?}"
                    );
                },
            )
            .await;
    }
}
