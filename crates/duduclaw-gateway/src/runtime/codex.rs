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
/// [`sandbox_args`], or in the explore lane the read-only sandbox whatever
/// the employee's own level (the lane only narrows).
fn sandbox_args_for(caps: Option<&CapabilitiesConfig>, explore: bool) -> Vec<String> {
    if explore {
        crate::explore_lane::codex_sandbox_args()
    } else {
        sandbox_args(caps)
    }
}

// Enforcing egress downgrades FullAccess before choosing employee permissions.
fn egress_sandbox_caps(caps: Option<&CapabilitiesConfig>, mode: duduclaw_core::egress_guard::Mode) -> Option<&CapabilitiesConfig> {
    if mode >= duduclaw_core::egress_guard::Mode::Ask
        && sandbox_level_for(caps) == duduclaw_core::types::SandboxLevel::FullAccess
    {
        None // The default is WorkspaceWrite.
    } else {
        caps
    }
}

fn egress_network_args(mode: duduclaw_core::egress_guard::Mode) -> Vec<String> {
    if mode >= duduclaw_core::egress_guard::Mode::Ask {
        ["-c", "sandbox_workspace_write.network_access=false", "-c", "web_search=\"disabled\""]
            .into_iter().map(str::to_string).collect()
    } else {
        Vec::new()
    }
}

#[cfg(any(not(unix), test))]
fn egress_sandbox_args(caps: Option<&CapabilitiesConfig>, explore: bool, mode: duduclaw_core::egress_guard::Mode) -> Vec<String> {
    let mut args = sandbox_args_for(egress_sandbox_caps(caps, mode), explore);
    args.extend(egress_network_args(mode));
    args
}

#[cfg(unix)]
fn employee_egress_sandbox_args(
    caps: Option<&CapabilitiesConfig>,
    explore: bool,
    mode: duduclaw_core::egress_guard::Mode,
    agent_dir: Option<&std::path::Path>,
    work_root: Option<&std::path::Path>,
    supports_permissions: bool,
) -> Result<Vec<String>, String> {
    let mut args = employee_sandbox_args(egress_sandbox_caps(caps, mode), explore, agent_dir, work_root, supports_permissions)?;
    args.extend(egress_network_args(mode));
    Ok(args)
}

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

#[cfg(unix)]
const EMPLOYEE_PERMISSION_PROFILE: &str = "duduclaw-employee";

/// Absolute exclusions keep protecting the employee when the spawn cwd is overridden.
#[cfg(unix)]
fn employee_permission_args(agent_dir: &std::path::Path, work_root: Option<&std::path::Path>) -> Result<Vec<String>, String> {
    let mut entries = vec![
        format!("{}={}", toml_string_literal(":root"), toml_string_literal("read")),
        format!("{}={}", toml_string_literal(":tmpdir"), toml_string_literal("write")),
        format!("{}={}", toml_string_literal("/tmp"), toml_string_literal("write")),
        format!("{}={{{}={}}}", toml_string_literal(":project_roots"),
            toml_string_literal("."), toml_string_literal("write")),
    ];
    let mut protected = std::collections::HashSet::new();
    for dir in std::iter::once(agent_dir).chain(work_root) {
        if !dir.is_absolute() {
            return Err("Codex employee permissions require absolute agent and working directories — refusing to spawn".into());
        }
        if dir.to_str().is_none() {
            return Err("Codex employee permissions require UTF-8 agent and working directories — refusing to spawn".into());
        }
        for path in duduclaw_core::agent_guard::employee_control_paths() {
            let absolute = dir.join(path);
            if protected.insert(absolute.clone()) {
                entries.push(format!("{}={}", toml_string_literal(absolute.to_str().unwrap()), toml_string_literal("read")));
            }
        }
    }
    let profile = EMPLOYEE_PERMISSION_PROFILE;
    Ok(vec![
        "-c".into(), format!("default_permissions={}", toml_string_literal(profile)),
        "-c".into(), format!("permissions.{profile}.filesystem={{{}}}", entries.join(",")),
    ])
}

#[cfg(unix)]
fn employee_sandbox_args(
    caps: Option<&CapabilitiesConfig>,
    explore: bool,
    agent_dir: Option<&std::path::Path>,
    work_root: Option<&std::path::Path>,
    supports_permissions: bool,
) -> Result<Vec<String>, String> {
    let mut args = sandbox_args_for(caps, explore);
    if !explore && sandbox_level_for(caps) == duduclaw_core::types::SandboxLevel::WorkspaceWrite {
        let Some(dir) = agent_dir else {
            return Err("Codex employee permissions required but no agent directory is set — refusing to spawn".into());
        };
        if !supports_permissions {
            return Err("Codex 員工控制檔保護需要 Codex CLI 0.164.0 以上；版本過舊或無法確認。請到儀表板首頁按「更新 Codex CLI」，或在終端機執行 `npm install -g @openai/codex@latest`。".into());
        }
        args.extend(employee_permission_args(dir, work_root)?);
    }
    Ok(args)
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
pub(super) fn env_value_as_string(v: &serde_json::Value) -> Option<String> {
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
pub(super) fn is_safe_config_key_segment(k: &str) -> bool {
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

/// First Codex CLI version live-verified to enforce employee permission profiles.
#[cfg(unix)]
const CODEX_EMPLOYEE_PERMISSIONS_MIN_VERSION: (u64, u64, u64) = (0, 164, 0);

#[cfg(unix)]
pub(crate) fn version_supports_employee_permissions(output: &str) -> bool {
    parse_codex_version(output).is_some_and(|v| v >= CODEX_EMPLOYEE_PERMISSIONS_MIN_VERSION)
}

#[derive(Clone, Copy, Default)]
struct CodexSupport {
    env_vars: bool,
    #[cfg(unix)]
    employee_permissions: bool,
}

/// Per-binary-path memo of Codex feature support, for the process
/// lifetime. A `tokio::sync::OnceCell` per path guarantees the probe subprocess
/// runs at most once even if several spawns race.
type CodexSupportCache =
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::OnceCell<CodexSupport>>>>;
static CODEX_SUPPORT: std::sync::OnceLock<CodexSupportCache> =
    std::sync::OnceLock::new();

/// Run `<bin> --version` once and return its output, or `None` on spawn
/// failure / non-zero exit / 5s timeout.
///
/// Every failure returns `None`: env passthrough falls back, while the
/// required employee permission profile refuses WorkspaceWrite spawns.
pub(crate) async fn probe_codex_version_output(bin: &str) -> Option<String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("--version");
    let out = crate::process_group::output(
        &mut cmd, None, std::time::Duration::from_secs(5),
    ).await.ok()?;
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

/// Replace the per-path cell: an in-flight old probe cannot repopulate it.
pub(crate) fn clear_codex_support(bin: &str) {
    let mut map = CODEX_SUPPORT
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock().unwrap_or_else(|e| e.into_inner());
    map.insert(bin.to_string(), std::sync::Arc::new(tokio::sync::OnceCell::new()));
}

/// Runtime::new uses the bare command; discovery also returns absolute paths.
pub(crate) fn clear_installed_codex_support(before: Option<&str>, found: Option<&str>) {
    clear_codex_support("codex");
    for bin in before.into_iter().chain(found) {
        clear_codex_support(bin);
    }
}

pub(crate) const EMPLOYEE_MINIMUM_VERSION: &str = "0.164.0";

/// Only Codex on Unix has a required CLI version. Unknown versions fail closed.
pub(crate) fn required_update(provider: &str, installed: bool, output: Option<&str>) -> bool {
    #[cfg(unix)]
    { provider == "codex" && installed && !output.is_some_and(version_supports_employee_permissions) }
    #[cfg(not(unix))]
    { let _ = (provider, installed, output); false }
}

pub(crate) fn display_version(output: &str) -> Option<String> {
    parse_codex_version(output).map(|(a, b, c)| format!("{a}.{b}.{c}"))
}

/// Codex feature support with the probe injected, so the caching
/// behavior can be tested without running a real binary.
async fn cached_codex_support<F, Fut>(bin: &str, probe: F) -> CodexSupport
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    let cell = {
        // Short, await-free critical section: pick (or create) this path's
        // cell, then drop the lock before the probe runs.
        let mut map = CODEX_SUPPORT
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
                    CodexSupport {
                        env_vars: supported,
                        #[cfg(unix)]
                        employee_permissions: version_supports_employee_permissions(&output),
                    }
                }
                None => {
                    warn!(
                        runtime = "codex",
                        "could not read `codex --version` (spawn failure, non-zero exit or \
                         timeout) — assuming no `env_vars` support and falling back to \
                         `-c …env.<K>=\"<value>\"`"
                    );
                    CodexSupport::default()
                }
            }
        })
        .await
}

async fn cached_env_vars_support<F, Fut>(bin: &str, probe: F) -> bool
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    cached_codex_support(bin, probe).await.env_vars
}

#[cfg(unix)]
async fn codex_supports_employee_permissions(bin: &str) -> bool {
    cached_codex_support(bin, || probe_codex_version_output(bin)).await.employee_permissions
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
/// T10 (design commercial/docs/specs/design-task-forward-model-2026-08-06.md
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

/// Keep completed assistant messages in emission order for a timed-out run.
/// The normal parser deliberately keeps only the last message; do not change it.
fn completed_codex_messages(stdout: &str) -> Vec<String> {
    stdout.lines().filter_map(|line| {
        let event: CodexEvent = serde_json::from_str(line).ok()?;
        if event.event_type != "item.completed" {
            return None;
        }
        let item = event.extra.get("item")?;
        let kind = item.get("type")?.as_str()?;
        if kind != "agent_message" && kind != "message" {
            return None;
        }
        let text = if kind == "agent_message" && item.get("text").and_then(|v| v.as_str()).is_some() {
            item["text"].as_str()?.to_string()
        } else {
            item.get("content")?.as_array()?.iter()
                .filter(|block| block.get("type").and_then(|v| v.as_str()) == Some("output_text"))
                .filter_map(|block| block.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>().join("\n")
        };
        (!text.trim().is_empty()).then_some(text)
    }).collect()
}

// Bound every retained representation independently. Pipes are always drained,
// including oversized lines and streams beyond the retention budget.
const MAX_STDOUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_STDOUT_LINE_BYTES: usize = 1024 * 1024;
const MAX_STDERR_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct CodexOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    line: Vec<u8>,
    dropping_line: bool,
    truncated: bool,
    content: String,
    input_tokens: u64,
    output_tokens: u64,
    chunks: Vec<super::RuntimeChunk>,
    chunk_bytes: usize,
    completed: Vec<String>,
    completed_bytes: usize,
    last_line: String,
    status: Option<std::process::ExitStatus>,
}

impl CodexOutput {
    fn stdout_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.dropping_line {
                if byte == b'\n' { self.dropping_line = false; }
                continue;
            }
            if self.line.len() == MAX_STDOUT_LINE_BYTES {
                self.line.clear();
                self.dropping_line = byte != b'\n';
                self.truncated = true;
                continue;
            }
            self.line.push(byte);
            if byte == b'\n' { self.finish_line(); }
        }
    }

    fn finish_line(&mut self) {
        if self.line.is_empty() { return; }
        let line_bytes = std::mem::take(&mut self.line);
        if self.stdout.len() + line_bytes.len() <= MAX_STDOUT_BYTES {
            self.stdout.extend_from_slice(&line_bytes);
        } else {
            self.truncated = true;
        }
        let line = String::from_utf8_lossy(&line_bytes);
        self.last_line = line.lines().last().unwrap_or("").to_string();
        // Parse each bounded event as it arrives, even after the raw-output
        // retention cap. In particular, final usage must not disappear merely
        // because earlier tool output consumed the raw-log budget.
        if let Ok(event) = serde_json::from_str::<CodexEvent>(&line) {
            let (content, input, output, chunks) = parse_codex_stdout(&line);
            if event.event_type == "turn.completed"
                && event.extra.get("usage").and_then(|u| serde_json::from_value::<CodexUsage>(u.clone()).ok()).is_some()
            {
                self.input_tokens = input;
                self.output_tokens = output;
            }
            if event.event_type == "item.completed" {
                if let Some(item) = event.extra.get("item") {
                    let kind = item.get("type").and_then(|v| v.as_str());
                    let has_text = (kind == Some("agent_message") && item.get("text").and_then(|v| v.as_str()).is_some())
                        || item.get("content").and_then(|v| v.as_array())
                            .and_then(|blocks| blocks.iter().find(|block|
                                block.get("type").and_then(|v| v.as_str()) == Some("output_text")))
                            .and_then(|block| block.get("text")).and_then(|v| v.as_str()).is_some();
                    if matches!(kind, Some("message" | "agent_message")) && has_text {
                        self.content = content;
                    }
                }
                for text in completed_codex_messages(&line) {
                    let charge = text.len().max(128);
                    if self.completed_bytes + charge <= MAX_STDOUT_BYTES {
                        self.completed_bytes += charge;
                        self.completed.push(text);
                    } else {
                        self.truncated = true;
                    }
                }
            }
            if !chunks.is_empty() {
                // Charge at least 128 bytes per event to bound vector overhead
                // for adversarial streams containing many tiny tool events.
                let charge = line_bytes.len().max(128);
                if self.chunk_bytes + charge <= MAX_STDOUT_BYTES {
                    self.chunk_bytes += charge;
                    self.chunks.extend(chunks);
                } else {
                    self.truncated = true;
                }
            }
        }
    }

    fn stderr_bytes(&mut self, bytes: &[u8]) {
        if self.stderr.len() + bytes.len() > MAX_STDERR_BYTES {
            self.truncated = true;
            let remove = (self.stderr.len() + bytes.len() - MAX_STDERR_BYTES).min(self.stderr.len());
            self.stderr.drain(..remove);
        }
        let start = bytes.len().saturating_sub(MAX_STDERR_BYTES);
        self.stderr.extend_from_slice(&bytes[start..]);
    }

    fn timeout_content(&self, timeout_secs: u64) -> Result<String, String> {
        if self.completed.is_empty() {
            return Err(format!("Codex CLI hard timeout after {timeout_secs}s (no completed output)"));
        }
        warn!(timeout_secs, completed_messages = self.completed.len(),
            "Codex CLI hard timeout; returning completed output");
        Ok(self.completed.join("\n\n"))
    }
}

/// All readers are local futures. Cancellation drops the readers and the
/// managed leader, which kills its group and schedules reaping.
async fn collect_codex_output(
    cmd: &mut tokio::process::Command,
    timeout: std::time::Duration,
) -> Result<CodexOutput, String> {
    #[cfg(test)]
    if let Ok(output) = tests::COLLECT_PROBE.try_with(|probe| probe(cmd)) {
        return output;
    }
    use tokio::io::AsyncReadExt;
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut managed = crate::process_group::ManagedChild::spawn(cmd)
        .map_err(|e| format!("Failed to spawn codex: {e}"))?;
    let mut stdout = managed.child().stdout.take().ok_or("Failed to capture codex stdout")?;
    let mut stderr = managed.child().stderr.take().ok_or("Failed to capture codex stderr")?;
    let mut output = CodexOutput::default();
    let mut stdout_buffer = [0u8; 8192];
    let mut stderr_buffer = [0u8; 8192];
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut status = None;
    let result = tokio::time::timeout(timeout, async {
        while !stdout_done || !stderr_done || status.is_none() {
            tokio::select! {
                read = stdout.read(&mut stdout_buffer), if !stdout_done => {
                    let read = read?;
                    if read == 0 { stdout_done = true; output.finish_line(); }
                    else { output.stdout_bytes(&stdout_buffer[..read]); }
                }
                read = stderr.read(&mut stderr_buffer), if !stderr_done => {
                    let read = read?;
                    if read == 0 { stderr_done = true; }
                    else { output.stderr_bytes(&stderr_buffer[..read]); }
                }
                waited = managed.child().wait(), if status.is_none() => {
                    status = Some(waited?);
                    managed.leader_exited();
                }
            }
        }
        Ok::<(), std::io::Error>(())
    }).await;
    output.finish_line();
    if output.truncated {
        warn!(stdout_limit = MAX_STDOUT_BYTES, line_limit = MAX_STDOUT_LINE_BYTES,
            stderr_limit = MAX_STDERR_BYTES, "Codex output retention truncated; pipes continued draining");
    }
    match result {
        Ok(Ok(())) => {
            if status.is_some_and(|status| status.success()) { managed.disarm(); }
            else { managed.kill_and_reap().await; }
            output.status = status;
        }
        Ok(Err(error)) => {
            managed.kill_and_reap().await;
            return Err(format!("Failed to read codex output: {error}"));
        }
        Err(_) => { managed.kill_and_reap().await; }
    }
    Ok(output)
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

/// Restricted utility profile, verified against codex-cli 0.164.0 help and
/// upstream 970111f6 config/features/tool registry. This is NOT tool-free:
/// built-in read-capable tools may remain. See the security report for the
/// system/managed configuration boundary of --ignore-user-config.
fn utility_args() -> Vec<String> {
    ["exec", "--json", "--skip-git-repo-check", "--sandbox", "read-only",
        "--ignore-user-config", "--ephemeral", "--ignore-rules",
        "-c", "approval_policy=never",
        "-c", "features.shell_tool=false",
        "-c", "features.view_image=false",
        "-c", "features.image_generation=false",
        "-c", "features.multi_agent=false",
        "-c", "features.multi_agent_v2=false",
        "-c", "agents.enabled=false",
        "-c", "web_search=\"disabled\"",
        "-c", "features.hooks=false",
        "-c", "features.plugins=false",
        "-c", "features.apps=false",
        "-c", "features.skip_host_skill_discovery=true",
        "-c", "project_doc_max_bytes=0",
    ].into_iter().map(str::to_owned).collect()
}

fn utility_native_caps(caps: Option<&CapabilitiesConfig>) -> CapabilitiesConfig {
    CapabilitiesConfig {
        native_sandbox: caps.is_some_and(|c| c.native_sandbox),
        allowed_tools: vec!["Read".into()],
        computer_use: false,
        browser_via_bash: false,
        ..Default::default()
    }
}

impl CodexRuntime {
    fn prepare_utility_command(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<(tokio::process::Command, tempfile::TempDir, Option<tempfile::NamedTempFile>), String> {
        // TempDir owns cleanup on success, spawn failure, timeout and cancellation.
        // tempfile creates private directories (0700 on Unix); set explicitly too.
        let work_dir = tempfile::Builder::new().prefix("duduclaw-codex-utility-")
            .tempdir().map_err(|e| format!("cannot create Codex utility directory: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(work_dir.path(), std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("cannot secure Codex utility directory: {e}"))?;
        }
        let mut cmd = tokio::process::Command::new(&self.codex_path);
        duduclaw_core::spawn_env::apply_agent_cli_env_allowlist(&mut cmd);
        // Auth stays in the real auth store, never copied into the temporary cwd.
        // CODEX_HOME is an auth location, absent from the generic spawn allowlist.
        if let Some(home) = std::env::var_os("CODEX_HOME") { cmd.env("CODEX_HOME", home); }
        if let Some(key) = std::env::var_os("OPENAI_API_KEY") { cmd.env("OPENAI_API_KEY", key); }
        cmd.args(utility_args());
        cmd.args(effort_args(context.effort));
        let (schema_args, schema_file) =
            output_schema_args(crate::runtime_dispatch::output_schema_override().as_ref());
        cmd.args(schema_args);
        let safe_prompt = if prompt.starts_with('-') { format!(" {prompt}") } else { prompt.to_owned() };
        let system_prompt = duduclaw_core::truncate_bytes(&context.system_prompt, 65536);
        let augmented = if system_prompt.is_empty() { safe_prompt } else {
            format!("<role_system_prompt>\n{}\n</role_system_prompt>\n\n{safe_prompt}",
                system_prompt.replace("</role_system_prompt>", "&lt;/role_system_prompt&gt;"))
        };
        cmd.arg(augmented);
        if !context.model.is_empty() { cmd.arg("-m").arg(&context.model); }
        // Both process cwd and Codex's working root exclude employee/project files.
        cmd.current_dir(work_dir.path()).arg("--cd").arg(work_dir.path());
        let caps = utility_native_caps(context.capabilities.as_ref());
        super::apply_native_sandbox(&mut cmd, Some(&caps), Some(work_dir.path()), "codex")?;
        Ok((cmd, work_dir, schema_file))
    }
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
        if crate::runtime_dispatch::in_utility_call() {
            let (mut cmd, _work_dir, _schema_file) = self.prepare_utility_command(prompt, context)?;
            return execute_codex_command(&mut cmd, context).await;
        }
        // Explore lane: `-s read-only` whatever the capability level, and the
        // duduclaw MCP server registered with the lane variable (below).
        let explore = crate::explore_lane::in_explore();
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
        if level == duduclaw_core::types::SandboxLevel::ReadOnly && !explore {
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

        // W2 (MCP wiring): register the duduclaw MCP server before spawning,
        // plus (2026-10-19) the employee's third-party `.mcp.json` servers,
        // each behind `duduclaw mcp-proxy` (see `third_party`).
        // 1) Per-invocation `-c` overrides — effective regardless of CODEX_HOME.
        // 2) Best-effort per-agent `.codex/config.toml` for operators who run
        //    codex manually in the agent dir with CODEX_HOME pointed there.
        //    Warn-not-fatal: writing that file must not block the reply.
        // The `.mcp.json` spawn gate runs first and its refusal (or an
        // unreadable / malformed `.mcp.json`) stops this spawn: fail closed.
        let own_mcp =
            super::duduclaw_mcp_server_json_for_home(&context.agent_id, &context.home_dir);
        let mut third_party_servers = match (context.agent_dir.as_deref(), own_mcp.as_ref()) {
            (Some(dir), Some(own)) => {
                third_party::gated_servers_for_spawn(dir, &context.home_dir, own, explore).await?
            }
            _ => std::collections::BTreeMap::new(),
        };
        if let Some(ref dir) = context.agent_dir {
            let written = match own_mcp.as_ref() {
                Some(own) => Self::write_spawn_mcp_config(dir, own, &third_party_servers),
                None => Err("duduclaw binary did not resolve to an absolute path".to_string()),
            };
            if let Err(e) = written {
                warn!(
                    runtime = "codex",
                    agent = %context.agent_id,
                    error = %e,
                    "failed to write per-agent codex MCP config — continuing without it"
                );
            }
        }

        // Working root. Normally the agent's own directory; a caller may
        // override it via `super::SPAWN_OVERRIDE` (today: the team composer,
        // putting a role member in the employee's workspace so its files
        // outlive the throwaway scaffold — design §4.3 E3). Identity is NOT
        // affected: the `-c mcp_servers.duduclaw.env.DUDUCLAW_AGENT_ID`
        // override carries the member id independently of cwd.
        //
        // Resolved ONCE (it used to be read twice, so a `work_dir` that failed
        // validation could still have flipped `cwd_overridden`), through the
        // shared [`super::resolve_spawn_work_dir`] which validates the override
        // is a real directory and otherwise warns and falls back.
        let work_root: Option<std::path::PathBuf> =
            super::resolve_spawn_work_dir(context.agent_dir.as_deref(), &context.agent_id);
        let cwd_overridden = work_root.is_some() && work_root != context.agent_dir;

        let mut cmd = tokio::process::Command::new(&self.codex_path);
        cmd.arg("exec").arg("--json");
        // Agent directories are not git repositories. Codex 0.156.x refuses to
        // run outside a trusted (git) directory unless told otherwise —
        // observed live 2026-09-24: "Not inside a trusted directory and
        // --skip-git-repo-check was not specified." (exit 1). Reproduced in a
        // plain temp dir; the same invocation succeeds with the flag.
        cmd.arg("--skip-git-repo-check");
        let egress_mode = duduclaw_core::egress_guard::effective_mode(&context.home_dir, context.agent_dir.as_deref());
        let effective_caps = egress_sandbox_caps(caps, egress_mode);
        #[cfg(unix)]
        {
            let needs_permissions = !explore
                && sandbox_level_for(effective_caps) == duduclaw_core::types::SandboxLevel::WorkspaceWrite;
            let supports_permissions = needs_permissions
                && codex_supports_employee_permissions(&self.codex_path).await;
            let args = employee_egress_sandbox_args(caps, explore, egress_mode, context.agent_dir.as_deref(), work_root.as_deref(), supports_permissions)
                .map_err(|error| duduclaw_agent::mcp_spawn_gate::spawn_gate_error(&error))?;
            cmd.args(args);
        }
        // Windows control-file permission profiles are unverified.
        #[cfg(not(unix))]
        cmd.args(egress_sandbox_args(effective_caps, explore, egress_mode));
        cmd.args(effort_args(context.effort));
        // Thinking display: ask for reasoning summaries only when this
        // reply turn's viewer reads thinking text; otherwise nothing is
        // added (byte-identical argv).
        cmd.args(crate::thinking::producers::codex_reasoning_summary_args(
            &crate::thinking::policy(),
        ));
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
        // path per process; env passthrough alone falls back on probe failure.
        // The required employee permission profile was checked above.
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
        // Third-party servers (2026-10-19). `_third_party_env_files` holds
        // each proxy's upstream-env temp file until Codex has exited.
        // Host identity is ephemeral: the persistent config above deliberately
        // keeps the identity MAC and turn out of third-party entries.
        let host_proxy_env = super::third_party_mcp::host_proxy_env(&context.home_dir, &context.agent_id);
        for server in third_party_servers.values_mut() {
            server.env.extend(host_proxy_env.iter().cloned());
        }
        let third_party_overrides = third_party::override_args(
            &third_party_servers,
            supports_env_vars,
            crate::explore_lane::lane_env().as_ref(),
            &mcp_overrides.process_env,
            &context.home_dir,
        )
        .map_err(|e| {
            duduclaw_agent::mcp_spawn_gate::spawn_gate_error(&format!(
                "第三方 MCP 伺服器的啟動設定無法產生（{e}），這次不啟動。"
            ))
        })?;
        for (k, v) in &third_party_overrides.process_env {
            cmd.env(k, v);
        }
        cmd.args(&third_party_overrides.args);
        let _third_party_env_files = third_party_overrides.temp_files;
        // P2-B N4: this turn's or run's source identity for the duduclaw MCP
        // server, so what the employee stores there can be forgotten by its
        // conversation. Per-spawn `-c` overrides, never a persisted config.
        // In the explore lane the same table carries `DUDUCLAW_LANE=explore`,
        // so the MCP server (should Codex ever call it) lists and runs
        // read-only tools only.
        let mut source_pairs = crate::memory_provenance::turn_source_env_pairs();
        source_pairs.extend(crate::explore_lane::lane_env());
        cmd.args(turn_source_override_args(&source_pairs));

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

        execute_codex_command(&mut cmd, context).await
    }

    async fn is_available(&self) -> bool {
        probe_codex_version_output(&self.codex_path).await.is_some()
    }
}

// Shared by conversational and utility calls: retain bounded incremental output,
// process-group cleanup, timeout partials, usage and native-event reporting.
async fn execute_codex_command(
    cmd: &mut tokio::process::Command,
    context: &RuntimeContext,
) -> Result<RuntimeResponse, String> {
    let timeout_secs = crate::runtime_config::call_timeout_secs(context.agent_dir.as_deref());
    let output = collect_codex_output(
        cmd, std::time::Duration::from_secs(timeout_secs),
    ).await?;

    if let Some(status) = output.status {
        if !status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Codex CLI exited with {}: {}",
                status,
                stderr.chars().take(500).collect::<String>()
            ));
        }
    }

    // Parse JSONL output
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut response = RuntimeResponse {
        partial_timeout_secs: output.status.is_none().then_some(timeout_secs),
        content: output.content.clone(),
        input_tokens: output.input_tokens,
        output_tokens: output.output_tokens,
        cache_read_tokens: 0,
        model_used: context.model.clone(),
        runtime_name: "codex".to_string(),
    };
    super::extend_native_tool_events(super::native_tool_events_from_chunks(&output.chunks));
    // Thinking display: `reasoning` items and reasoning tokens, reported
    // after the run (no-op outside a reply turn).
    crate::thinking::producers::report_codex(
        &crate::thinking::producers::parse_codex_thinking(&stdout),
        &mut crate::thinking::Recorder::new("codex"),
    );

    // Parse usage, native events and thinking above on both paths, even
    // when the deadline left no completed assistant message.
    if output.status.is_none() {
        response = crate::runtime_dispatch::validate_runtime_response(response, context).await?;
        response.content = match output.timeout_content(timeout_secs) {
            Ok(content) => content,
            Err(error) => {
                crate::runtime_dispatch::record_discarded_runtime_usage(context, &response).await;
                return Err(error);
            }
        };
    }

    if response.content.is_empty() {
        // Fallback: use the last line as content
        response.content = output.last_line.clone();
    }

    // Still empty ⇒ FAILURE, not success: Ok("") would be silently dropped
    // by every channel and poison the session with an empty assistant turn.
    if response.content.trim().is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Empty response from Codex CLI (exit 0); stderr tail: {}",
            duduclaw_core::truncate_bytes(stderr.trim(), 300)
        ));
    }

    Ok(response)
}

// ── Streaming ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "codex_timeout_usage_tests.rs"]
mod timeout_usage_tests;

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
    ///
    /// The file carries `DUDUCLAW_AGENT_TOKEN` in plaintext, so it is written
    /// atomically through an owner-only temp file (0600 from the first byte,
    /// then renamed over the old file — no window in which it is readable by
    /// others), and an up-to-date file is still reset to owner-only. Only a
    /// regular file counts as up to date: a symbolic link there is replaced,
    /// never followed.
    pub fn write_mcp_config(
        agent_dir: &std::path::Path,
        servers: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<bool, String> {
        let config_path = agent_dir.join(".codex").join("config.toml");
        let content = Self::render_mcp_toml(servers);
        let is_regular = std::fs::symlink_metadata(&config_path).is_ok_and(|m| m.file_type().is_file());
        if is_regular && std::fs::read_to_string(&config_path).is_ok_and(|e| e == content) {
            duduclaw_core::platform::set_owner_only(&config_path).map_err(|e| e.to_string())?;
            return Ok(false);
        }
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        duduclaw_agent::mcp_template::write_mcp_config_atomic(&config_path, content.as_bytes())?;
        Ok(true)
    }

    /// W2: write the agent's codex config with the duduclaw MCP server
    /// (`own`: absolute binary + `mcp-server` arg + identity env) and, since
    /// 2026-10-19, the employee's gated third-party servers. Called from
    /// [`AgentRuntime::execute`] before every spawn — cheap check-before-write
    /// keeps it idempotent. `own` always holds the `duduclaw` key: a
    /// third-party entry cannot take it (they never carry a DuDuClaw name).
    pub(crate) fn write_spawn_mcp_config(
        agent_dir: &std::path::Path,
        own: &serde_json::Value,
        third_party: &std::collections::BTreeMap<String, third_party::GatedServer>,
    ) -> Result<bool, String> {
        let mut servers = third_party::file_entries(third_party);
        servers.insert("duduclaw".to_string(), own.clone());
        Self::write_mcp_config(agent_dir, &servers)
    }
}

pub(crate) mod third_party;

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

    #[test]
    fn bounded_parser_preserves_normal_messages_usage_and_tools() {
        let stdout = concat!(
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"完成\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\",\"command\":\"echo ok\",\"aggregated_output\":\"ok\",\"status\":\"completed\"}}\n",
            "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":7,\"output_tokens\":9}}\n"
        );
        let expected = parse_codex_stdout(stdout);
        let actual = collected_fixture(stdout);
        assert_eq!((actual.content, actual.input_tokens, actual.output_tokens), (expected.0, expected.1, expected.2));
        assert_eq!(format!("{:?}", actual.chunks), format!("{:?}", expected.3));
        assert_eq!(actual.stdout, stdout.as_bytes());
        assert!(!actual.truncated);
    }

    #[test]
    fn oversized_line_is_discarded_and_later_usage_is_still_parsed() {
        let mut output = CodexOutput::default();
        let block = [b'x'; 8192];
        for _ in 0..(MAX_STDOUT_LINE_BYTES / block.len() + 100) {
            output.stdout_bytes(&block);
            assert!(output.line.len() <= MAX_STDOUT_LINE_BYTES);
        }
        output.stdout_bytes(b"\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":17,\"output_tokens\":19}}\n");
        assert_eq!((output.input_tokens, output.output_tokens), (17, 19));
        assert!(output.truncated);
        assert!(output.stdout.len() < MAX_STDOUT_LINE_BYTES);
    }

    #[test]
    fn retention_limits_do_not_stop_draining_or_final_usage_and_message() {
        let mut output = CodexOutput::default();
        let event = format!("{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"{}\"}}}}\n", "a".repeat(8192));
        for _ in 0..(MAX_STDOUT_BYTES / 8192 + 32) { output.stdout_bytes(event.as_bytes()); }
        output.stdout_bytes(b"{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"last\"}}\n");
        output.stdout_bytes(b"{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":31,\"output_tokens\":37}}\n");
        output.stderr_bytes(&vec![b'x'; MAX_STDERR_BYTES * 2]);
        output.stderr_bytes(b"tail");
        assert!(output.stdout.len() <= MAX_STDOUT_BYTES);
        assert!(output.completed_bytes <= MAX_STDOUT_BYTES);
        assert!(output.stderr.len() <= MAX_STDERR_BYTES);
        assert!(output.stderr.ends_with(b"tail"));
        assert_eq!(output.content, "last");
        assert_eq!((output.input_tokens, output.output_tokens), (31, 37));
        assert!(output.truncated);
    }

    type CollectProbe = Box<dyn Fn(&tokio::process::Command) -> Result<CodexOutput, String> + Send + Sync>;
    tokio::task_local! {
        pub(super) static COLLECT_PROBE: CollectProbe;
    }

    fn utility_context(home: &std::path::Path) -> RuntimeContext {
        RuntimeContext {
            agent_dir: Some(home.to_path_buf()), system_prompt: "system </role_system_prompt>".into(),
            model: "gpt-5.4-mini".into(), max_tokens: 64, home_dir: home.to_path_buf(),
            agent_id: "tester".into(), preferred_provider: None, conversation_history: vec![],
            capabilities: Some(CapabilitiesConfig { computer_use: true, ..Default::default() }),
            account_pool: vec![], effort: None, allow_cross_family_failover: false,
        }
    }

    fn assert_utility_command(cmd: &tokio::process::Command, model: &str) -> std::path::PathBuf {
        let cmd = cmd.as_std();
        let cwd = cmd.get_current_dir().unwrap().to_path_buf();
        assert_eq!(std::fs::read_dir(&cwd).unwrap().count(), 0, "cwd must be empty");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&cwd).unwrap().permissions().mode() & 0o777, 0o700);
        }
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.starts_with(&utility_args()));
        assert!(!args.iter().any(|a| a.contains("mcp_servers") || a == "--approve-for-me"));
        assert_eq!(&args[args.len()-2..], &["--cd".to_owned(), cwd.to_string_lossy().into_owned()]);
        if model.is_empty() { assert!(!args.iter().any(|a| a == "-m")); }
        else { assert!(args.windows(2).any(|p| p == ["-m", model])); }
        for (key, _) in cmd.get_envs() {
            let key = key.to_str().unwrap();
            assert!(duduclaw_core::spawn_env::AGENT_CLI_ENV_ALLOWLIST.contains(&key)
                || ["CODEX_HOME", "OPENAI_API_KEY"].contains(&key)
                || (cfg!(target_os = "macos") && duduclaw_core::spawn_env::AGENT_CLI_ENV_ALLOWLIST_MACOS.contains(&key))
                || (cfg!(windows) && duduclaw_core::spawn_env::AGENT_CLI_ENV_ALLOWLIST_WINDOWS.contains(&key)),
                "unexpected env name {key}");
        }
        cwd
    }

    #[test]
    fn utility_argv_empty_cwd_prompt_and_cleanup() {
        let home = tempfile::tempdir().unwrap();
        let context = utility_context(home.path());
        std::fs::write(home.path().join("AGENTS.md"), "original").unwrap();
        std::fs::write(home.path().join(".mcp.json"), "malformed employee MCP must not be read").unwrap();
        std::fs::create_dir(home.path().join(".codex")).unwrap();
        std::fs::write(home.path().join(".codex/config.toml"), "original config").unwrap();
        let runtime = CodexRuntime { codex_path: "/nonexistent/vendor-cli".into() };
        for model in ["", "gpt-5.4-mini"] {
            let mut context = context.clone();
            context.model = model.into();
            let (cmd, dir, schema) = runtime.prepare_utility_command("-injected", &context).unwrap();
            let cwd = assert_utility_command(&cmd, model);
            assert_ne!(cwd, home.path());
            let mut expected: Vec<String> = [
                "exec", "--json", "--skip-git-repo-check", "--sandbox", "read-only",
                "--ignore-user-config", "--ephemeral", "--ignore-rules",
                "-c", "approval_policy=never", "-c", "features.shell_tool=false",
                "-c", "features.view_image=false", "-c", "features.image_generation=false",
                "-c", "features.multi_agent=false", "-c", "features.multi_agent_v2=false",
                "-c", "agents.enabled=false",
                "-c", "web_search=\"disabled\"",
                "-c", "features.hooks=false", "-c", "features.plugins=false",
                "-c", "features.apps=false", "-c", "features.skip_host_skill_discovery=true",
                "-c", "project_doc_max_bytes=0",
            ].into_iter().map(str::to_owned).collect();
            expected.push("<role_system_prompt>\nsystem &lt;/role_system_prompt&gt;\n</role_system_prompt>\n\n -injected".into());
            if !model.is_empty() { expected.extend(["-m".into(), model.into()]); }
            expected.extend(["--cd".into(), cwd.to_string_lossy().into_owned()]);
            assert_eq!(cmd.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>(), expected);
            assert!(schema.is_none());
            drop(dir);
            assert!(!cwd.exists());
        }
        assert_eq!(std::fs::read_to_string(home.path().join("AGENTS.md")).unwrap(), "original");
        assert_eq!(std::fs::read_to_string(home.path().join(".codex/config.toml")).unwrap(), "original config");
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 3);
        let caps = utility_native_caps(Some(&CapabilitiesConfig {
            native_sandbox: true, computer_use: true, browser_via_bash: true, ..Default::default()
        }));
        assert!(caps.native_sandbox);
        assert_eq!(sandbox_level_for(Some(&caps)), duduclaw_core::types::SandboxLevel::ReadOnly);
    }

    #[tokio::test]
    async fn collected_partial_timeout_is_display_only() {
        let home = tempfile::tempdir().unwrap();
        let context = utility_context(home.path());
        for utility in [false, true] {
            let probe: CollectProbe = Box::new(|_| {
                let mut output = collected_fixture(
                    "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"PASS\"}}\n");
                output.status = None;
                Ok(output)
            });
            let mut cmd = tokio::process::Command::new("fixture-never-spawned");
            let call = COLLECT_PROBE.scope(probe, execute_codex_command(&mut cmd, &context));
            if utility {
                let error = crate::runtime_dispatch::CHANNEL_REPLY_PARTIAL.scope((),
                    crate::runtime_dispatch::UTILITY_CALL.scope((), call)).await.unwrap_err();
                assert_eq!(error, "codex CLI hard timeout after 300s (partial output discarded)");
            } else {
                let response = crate::runtime_dispatch::CHANNEL_REPLY_PARTIAL.scope((), call).await.unwrap();
                assert_eq!(response.content, "PASS");
                assert_eq!(response.partial_timeout_secs, Some(300));
            }
        }
    }

    #[tokio::test]
    async fn normal_codex_command_keeps_conversational_flags_and_agent_prompt_file() {
        let home = tempfile::tempdir().unwrap();
        let mut context = utility_context(home.path());
        context.capabilities = None;
        let bin = "/nonexistent/codex-conversational-args-test";
        cached_codex_support(bin, || async { Some("codex-cli 0.164.0".into()) }).await;
        let runtime = CodexRuntime { codex_path: bin.into() };
        let agent = home.path().to_owned();
        let probe: CollectProbe = Box::new(move |cmd| {
            let args: Vec<_> = cmd.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            assert_eq!(&args[..8], &["exec", "--json", "--skip-git-repo-check", "--approve-for-me",
                "-c", "approval_policy=never", "-c", "sandbox_mode=\"workspace-write\""]);
            assert!(!args.iter().any(|a| ["--ignore-user-config", "--ephemeral", "--ignore-rules",
                "features.shell_tool=false", "project_doc_max_bytes=0"].contains(&a.as_str())));
            assert_eq!(&args[args.len()-5..], &["normal".to_owned(), "-m".into(),
                "gpt-5.4-mini".into(), "--cd".into(), agent.to_string_lossy().into_owned()]);
            assert_eq!(std::fs::read_to_string(agent.join("AGENTS.md")).unwrap(), "system </role_system_prompt>");
            Err("normal argv observed".into())
        });
        let error = COLLECT_PROBE.scope(probe, runtime.execute("normal", &context)).await.unwrap_err();
        assert_eq!(error, "normal argv observed");
    }

    #[tokio::test]
    async fn utility_cleans_temp_dir_after_error_and_timeout() {
        let home = tempfile::tempdir().unwrap();
        let context = utility_context(home.path());
        let runtime = CodexRuntime { codex_path: "/nonexistent/vendor-cli".into() };
        for timeout in [false, true] {
            let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
            let captured = seen.clone();
            let probe: CollectProbe = Box::new(move |cmd| {
                *captured.lock().unwrap() = Some(assert_utility_command(cmd, "gpt-5.4-mini"));
                if timeout { Ok(CodexOutput::default()) } else { Err("fixture spawn error".into()) }
            });
            let error = COLLECT_PROBE.scope(probe, crate::runtime_dispatch::UTILITY_CALL.scope((),
                runtime.execute("text", &context))).await.unwrap_err();
            assert!(if timeout { error.contains("hard timeout") } else { error == "fixture spawn error" }, "{error}");
            assert!(!seen.lock().unwrap().as_ref().unwrap().exists());
            assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
        }
    }

    #[tokio::test]
    async fn hinted_team_verifier_and_judge_dispatch_to_restricted_codex() {
        use crate::runtime_dispatch::{UtilityModelHint, run_utility_prompt_with_hint};
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents/tester");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[runtime]\nprovider='codex'\n[model]\nutility='gpt-5.4-mini'\n").unwrap();
        // No host CLI detection or credential-file reads: use the real Codex
        // runtime, but intercept its collector before any subprocess is spawned.
        let registry = super::super::RuntimeRegistry::with_runtimes(std::collections::HashMap::from([
            (duduclaw_core::types::RuntimeType::Codex,
                Box::new(CodexRuntime { codex_path: "/nonexistent/vendor-cli".into() }) as Box<dyn AgentRuntime>),
        ]));
        let _guard = crate::runtime_dispatch::testing::RegistryGuard::install(home.path(), registry).await;
        for owner in [Some(agent.as_path()), None] {
            let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
            let captured = seen.clone();
            let probe: CollectProbe = Box::new(move |cmd| {
                assert!(crate::runtime_dispatch::in_utility_call());
                *captured.lock().unwrap() = Some(assert_utility_command(cmd, "gpt-5.4-mini"));
                let mut output = collected_fixture("{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"verdict\"}}\n");
                output.status = Some(std::process::ExitStatus::default());
                Ok(output)
            });
            let hint = UtilityModelHint { provider: Some(duduclaw_core::types::RuntimeType::Codex),
                model: Some("gpt-5.4-mini".into()), ..Default::default() };
            let result = COLLECT_PROBE.scope(probe, run_utility_prompt_with_hint(
                home.path(), owner, "tester", "system", "verifier/judge material", 64, Some(&hint),
            )).await.unwrap();
            assert_eq!(result, "verdict");
            assert!(!seen.lock().unwrap().as_ref().unwrap().exists());
            assert!(!agent.join("AGENTS.md").exists());
            assert!(!agent.join(".codex").exists());
        }
    }

    fn collected_fixture(stdout: &str) -> CodexOutput {
        let mut output = CodexOutput::default();
        output.stdout_bytes(stdout.as_bytes());
        output.finish_line();
        output
    }

    #[test]
    fn timeout_keeps_all_completed_messages_from_truncated_jsonl() {
        let stdout = concat!(
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"查詢完成\"}}\n",
            "{\"type\":\"item.started\",\"item\":{\"type\":\"agent_message\",\"text\":\"未完成\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"已整理\"},{\"type\":\"output_text\",\"text\":\"細節\"}]}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"  \"}}\n",
            "{\"type\":\"item.completed\",\"item\":{"
        );
        assert_eq!(completed_codex_messages(stdout), vec!["查詢完成", "已整理\n細節"]);
        assert_eq!(collected_fixture(stdout).timeout_content(300).unwrap(),
            "查詢完成\n\n已整理\n細節");
        // Normal parsing still returns only the final completed message (even blank).
        assert_eq!(parse_codex_stdout(stdout).0, "  ");
    }

    #[test]
    fn timeout_without_completed_message_is_an_error() {
        for stdout in ["", "plain text", r#"{"type":"item.started","item":{"type":"agent_message","text":"pending"}}"#] {
            assert_eq!(collected_fixture(stdout).timeout_content(30).unwrap_err(),
                "Codex CLI hard timeout after 30s (no completed output)");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_collector_kills_long_sleeping_grandchild() {
        let home = tempfile::tempdir().unwrap();
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.env_clear().env("HOME", home.path()).current_dir(home.path())
            .args(["-c", "/bin/sleep 30 & echo $! > pid; wait"]);
        let output = collect_codex_output(&mut cmd, std::time::Duration::from_millis(300)).await.unwrap();
        assert!(output.status.is_none());
        let pid: i32 = std::fs::read_to_string(home.path().join("pid")).unwrap().trim().parse().unwrap();
        for _ in 0..100 {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() { return; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("Codex grandchild {pid} survived timeout");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_collects_stdout_while_draining_stderr() {
        let home = tempfile::tempdir().unwrap();
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.env_clear().env("HOME", home.path()).current_dir(home.path());
        cmd.args(["-c", r#"printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"完成"}}'; /bin/dd if=/dev/zero bs=65536 count=4 >&2 2>/dev/null; exec /bin/sleep 5"#]);
        let output = collect_codex_output(&mut cmd, std::time::Duration::from_millis(200)).await.unwrap();
        assert!(output.status.is_none());
        assert_eq!(output.stderr.len(), MAX_STDERR_BYTES);
        assert!(output.timeout_content(1).unwrap().starts_with("完成"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn collected_normal_output_preserves_every_byte() {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.env_clear().args(["-c", "printf 'first\\r\\nsecond'; printf 'diagnostic' >&2"]);
        let output = collect_codex_output(&mut cmd, std::time::Duration::from_secs(2)).await.unwrap();
        assert!(output.status.unwrap().success());
        assert_eq!(output.stdout, b"first\r\nsecond");
        assert_eq!(output.stderr, b"diagnostic");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_with_no_completed_output_never_returns_raw_json() {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.env_clear().args(["-c", r#"printf '%s\n' '{"type":"turn.started"}'; exec /bin/sleep 5"#]);
        let output = collect_codex_output(&mut cmd, std::time::Duration::from_millis(100)).await.unwrap();
        assert!(output.status.is_none());
        assert!(output.timeout_content(1).is_err());
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

    #[test]
    fn egress_codex_args_only_narrow_and_preserve_mcp_approver() {
        use duduclaw_core::egress_guard::Mode;
        let full = caps(true, true, &[], &[]);
        let read_only = caps(false, false, &["Read"], &[]);
        for mode in [Mode::Off, Mode::Shadow, Mode::Ask, Mode::Block] {
            for caps in [None, Some(&full), Some(&read_only)] {
                let args = egress_sandbox_args(caps, false, mode);
                if mode < Mode::Ask {
                    assert_eq!(args, sandbox_args(caps));
                } else {
                    assert!(!args.iter().any(|a| a == "--dangerously-bypass-approvals-and-sandbox"));
                    assert!(args.iter().any(|a| a == "sandbox_workspace_write.network_access=false"));
                    assert!(args.iter().any(|a| a == "web_search=\"disabled\""));
                    if sandbox_level_for(caps) != duduclaw_core::types::SandboxLevel::ReadOnly {
                        assert!(args.iter().any(|a| a == "--approve-for-me"));
                        assert!(!args.iter().any(|a| a == "-s"));
                        assert_eq!(declared_sandbox(&args).as_deref(), Some("workspace-write"));
                    } else {
                        assert_eq!(declared_sandbox(&args).as_deref(), Some("read-only"));
                    }
                }
            }
            let explore = egress_sandbox_args(Some(&full), true, mode);
            assert_eq!(declared_sandbox(&explore).as_deref(), Some("read-only"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn egress_employee_permissions_compose_after_full_access_downgrade() {
        use duduclaw_core::egress_guard::Mode;
        let full = caps(true, true, &[], &[]);
        let read_only = caps(false, false, &["Read"], &[]);
        let agent = std::path::Path::new("/agents/member");
        let root = std::path::Path::new("/agents/parent");
        for mode in [Mode::Ask, Mode::Block] {
            for caps in [None, Some(&full)] {
                let args = employee_egress_sandbox_args(caps, false, mode, Some(agent), Some(root), true).unwrap();
                let mut expected = sandbox_args(None);
                expected.extend(employee_permission_args(agent, Some(root)).unwrap());
                expected.extend(egress_network_args(mode));
                assert_eq!(args, expected);
                assert!(!args.iter().any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox" || arg == "-s"));
                assert!(employee_egress_sandbox_args(caps, false, mode, Some(agent), Some(root), false).unwrap_err().contains("0.164.0"));
                assert!(employee_egress_sandbox_args(caps, false, mode, None, Some(root), true).is_err());
            }
            assert_eq!(employee_egress_sandbox_args(Some(&read_only), false, mode, None, None, false).unwrap(), egress_sandbox_args(Some(&read_only), false, mode));
            assert_eq!(employee_egress_sandbox_args(Some(&full), true, mode, None, None, false).unwrap(), egress_sandbox_args(Some(&full), true, mode));
        }
        for mode in [Mode::Off, Mode::Shadow] {
            assert_eq!(employee_egress_sandbox_args(Some(&full), false, mode, None, None, false).unwrap(), sandbox_args(Some(&full)));
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

    /// Explore lane: read-only sandbox for every level, including an explicit
    /// full-access grant; outside the lane the argv is unchanged; the lane
    /// variable rides the same `-c` env table as the turn identity.
    #[test]
    fn explore_lane_forces_the_read_only_sandbox_and_the_lane_env() {
        for c in [
            caps(false, false, &[], &[]),
            caps(false, false, &["Read"], &[]),
            caps(true, false, &[], &[]),
        ] {
            let lane = sandbox_args_for(Some(&c), true);
            assert_eq!(declared_sandbox(&lane).as_deref(), Some("read-only"), "{lane:?}");
            assert!(lane.windows(2).any(|w| w[0] == "-c" && w[1] == "approval_policy=never"));
            assert!(!lane.iter().any(|a| a.contains("bypass") || a == "--approve-for-me"), "{lane:?}");
            assert_eq!(sandbox_args_for(Some(&c), false), sandbox_args(Some(&c)));
        }
        assert_eq!(sandbox_args_for(None, true), crate::explore_lane::codex_sandbox_args());
        let env = turn_source_override_args(&[(
            duduclaw_core::ENV_LANE.to_string(),
            duduclaw_core::LANE_EXPLORE.to_string(),
        )]);
        assert_eq!(env, vec!["-c".to_string(), "mcp_servers.duduclaw.env.DUDUCLAW_LANE=\"explore\"".to_string()]);
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

    #[tokio::test]
    async fn clearing_support_allows_a_new_probe() {
        let paths = ["codex", "/test/old-codex", "/test/new-codex"];
        for bin in paths {
            assert!(!cached_codex_support(bin, || async { Some("codex-cli 0.100.0".into()) }).await.env_vars);
        }
        clear_installed_codex_support(Some(paths[1]), Some(paths[2]));
        for bin in paths {
            let support = cached_codex_support(bin, || async { Some("codex-cli 0.164.0".into()) }).await;
            assert!(support.env_vars);
            #[cfg(unix)]
            assert!(support.employee_permissions);
        }
    }

    #[cfg(unix)]
    #[test]
    fn employee_permissions_lock_every_control_path_with_valid_toml() {
        let dir = std::path::Path::new("/agents/space and \"quote員工");
        let args = employee_sandbox_args(None, false, Some(dir), Some(dir), true).unwrap();
        let mut expected = sandbox_args(None);
        expected.extend(employee_permission_args(dir, Some(dir)).unwrap());
        assert_eq!(args, expected);
        assert_eq!(args.len(), sandbox_args(None).len() + 4);
        let overrides = &args[sandbox_args(None).len()..];
        assert_eq!(overrides[0], "-c");
        assert_eq!(overrides[1], "default_permissions=\"duduclaw-employee\"");
        assert_eq!(overrides[2], "-c");
        let (key, value) = overrides[3].split_once('=').unwrap();
        // Codex -c splits keys naively: quotes become part of the profile name.
        // Verified live on 0.164.0, including an agent path containing a space.
        assert!(overrides[3].starts_with("permissions.duduclaw-employee.filesystem="));
        assert_eq!(key, "permissions.duduclaw-employee.filesystem");
        assert!(!key.contains('"'));
        let parsed: toml::Value = format!("value={value}").parse().unwrap();
        let fs = parsed["value"].as_table().unwrap();
        assert_eq!(fs[":root"].as_str(), Some("read"));
        assert_eq!(fs[":tmpdir"].as_str(), Some("write"));
        assert_eq!(fs["/tmp"].as_str(), Some("write"));
        assert_eq!(fs[":project_roots"]["."].as_str(), Some("write"));
        assert_eq!(fs.len(), 4 + duduclaw_core::agent_guard::employee_control_paths().count());
        for path in duduclaw_core::agent_guard::employee_control_paths() {
            assert_eq!(fs[dir.join(path).to_str().unwrap()].as_str(), Some("read"));
        }
        for writable in ["MEMORY.md", "CLAUDE.md", "AGENTS.md", "wiki/page.md", "note.txt"] {
            assert!(!fs.contains_key(dir.join(writable).to_str().unwrap()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn employee_permissions_protect_the_member_and_parent_work_root() {
        let agent = std::path::Path::new("/agents/member");
        let root = std::path::Path::new("/agents/parent");
        let args = employee_sandbox_args(None, false, Some(agent), Some(root), true).unwrap();
        let (_, value) = args.last().unwrap().split_once('=').unwrap();
        let parsed: toml::Value = format!("value={value}").parse().unwrap();
        let fs = parsed["value"].as_table().unwrap();
        assert_eq!(fs.len(), 4 + 2 * duduclaw_core::agent_guard::employee_control_paths().count());
        for dir in [agent, root] {
            for path in duduclaw_core::agent_guard::employee_control_paths() {
                assert_eq!(fs[dir.join(path).to_str().unwrap()].as_str(), Some("read"));
            }
            for writable in ["MEMORY.md", "CLAUDE.md", "AGENTS.md", "wiki/page.md"] {
                assert!(!fs.contains_key(dir.join(writable).to_str().unwrap()));
            }
        }
        assert!(employee_sandbox_args(None, false, Some(agent), Some(std::path::Path::new("relative/root")), true).is_err());
        use std::os::unix::ffi::OsStrExt;
        let invalid = std::path::Path::new(std::ffi::OsStr::from_bytes(b"/agents/\xff"));
        assert!(employee_permission_args(agent, Some(invalid)).is_err());
        assert!(employee_permission_args(invalid, Some(root)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn employee_permissions_leave_other_sandbox_lanes_unchanged() {
        let read_only = CapabilitiesConfig {
            allowed_tools: vec!["Read".into()], ..Default::default()
        };
        let full_access = CapabilitiesConfig { computer_use: true, ..Default::default() };
        assert_eq!(sandbox_level_for(Some(&read_only)), duduclaw_core::types::SandboxLevel::ReadOnly);
        for caps in [Some(&read_only), Some(&full_access)] {
            assert_eq!(employee_sandbox_args(caps, false, None, None, false).unwrap(), sandbox_args(caps));
        }
        for caps in [None, Some(&read_only), Some(&full_access)] {
            assert_eq!(employee_sandbox_args(caps, true, None, None, false).unwrap(), sandbox_args_for(caps, true));
        }
    }

    #[cfg(unix)]
    #[test]
    fn employee_permissions_version_gate_and_missing_directory_fail_closed() {
        for (version, accepted) in [("codex-cli 0.163.9", false),
            ("codex-cli 0.164.0", true), ("codex-cli 0.165.0", true),
            ("unparseable", false)] {
            let supported = version_supports_employee_permissions(version);
            assert_eq!(supported, accepted);
            let result = employee_sandbox_args(None, false, Some(std::path::Path::new("/private/agent")), None, supported);
            assert_eq!(result.is_ok(), accepted);
            if let Err(error) = result {
                assert!(error.contains("Codex CLI 0.164.0"));
                assert!(!error.contains("/private/agent"));
            }
        }
        let error = employee_sandbox_args(None, false, None, None, true).unwrap_err();
        assert!(error.contains("no agent directory"));
        assert!(error.contains("refusing to spawn"));
        assert!(employee_permission_args(std::path::Path::new("relative/agent"), None).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn employee_permissions_share_version_probe_and_refuse_unprobeable_cli() {
        let bin = "/nonexistent/codex-employee-permissions-cache-test";
        assert!(cached_env_vars_support(bin, || async { Some("codex-cli 0.164.0".into()) }).await);
        assert!(cached_codex_support(bin, || async { panic!("version probe repeated") }).await.employee_permissions);
        let runtime = CodexRuntime { codex_path: "/nonexistent/codex-employee-permissions-failed-test".into() };
        let home = tempfile::tempdir().unwrap();
        let mut context = utility_context(home.path());
        context.capabilities = None;
        let error = runtime.execute("must not spawn", &context).await.unwrap_err();
        assert!(error.contains("Codex CLI 0.164.0"));
        assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error));
        assert!(!error.contains("/nonexistent"));
        assert!(!home.path().join("AGENTS.md").exists());
        context.agent_dir = None;
        let error = runtime.execute("must not spawn", &context).await.unwrap_err();
        assert!(error.contains("no agent directory"));
        assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error));
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
