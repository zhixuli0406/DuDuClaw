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

// Bubblewrap needs namespaces; only a spawn without its sandbox can inherit the guard.
fn employee_needs_namespace_guard(caps: Option<&CapabilitiesConfig>, explore: bool) -> bool {
    !explore && sandbox_level_for(caps) == duduclaw_core::types::SandboxLevel::FullAccess
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
    private_roots: &[std::path::PathBuf],
) -> Result<Vec<String>, String> {
    let mut args = employee_sandbox_args(egress_sandbox_caps(caps, mode), explore, agent_dir, work_root, supports_permissions, private_roots)?;
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
///
/// `private_roots` (H1, v1.84 security review): `<DUDUCLAW_HOME>/run`
/// itself (re-review F4: its directory entry must not be renamable), the
/// gateway-private per-call Codex home parent (`<DUDUCLAW_HOME>/run/codex`)
/// and the host credential store directory. They get the same `"read"` entry as the control paths —
/// the only per-path override this profile has been verified with — so tools
/// cannot write there even when a write root (an overridden working root,
/// `:tmpdir`, `/tmp`) contains them. The profile has no verified "no access"
/// value, so `:root = "read"` still lets tools READ them.
#[cfg(unix)]
pub(super) fn employee_permission_args(
    agent_dir: &std::path::Path,
    work_root: Option<&std::path::Path>,
    private_roots: &[std::path::PathBuf],
) -> Result<Vec<String>, String> {
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
                entries.push(format!("{}={}", toml_string_literal(duduclaw_core::path_compare::spawn_path(&absolute).to_str().unwrap()), toml_string_literal("read")));
            }
        }
    }
    for root in private_roots {
        let Some(text) = duduclaw_core::path_compare::spawn_path(root).to_str().filter(|_| root.is_absolute()) else {
            return Err("Codex employee permissions require absolute UTF-8 private directories — refusing to spawn".into());
        };
        if protected.insert(root.clone()) {
            entries.push(format!("{}={}", toml_string_literal(text), toml_string_literal("read")));
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
    private_roots: &[std::path::PathBuf],
) -> Result<Vec<String>, String> {
    let mut args = sandbox_args_for(caps, explore);
    if !explore && sandbox_level_for(caps) == duduclaw_core::types::SandboxLevel::WorkspaceWrite {
        let Some(dir) = agent_dir else {
            return Err("Codex employee permissions required but no agent directory is set — refusing to spawn".into());
        };
        if !supports_permissions {
            return Err("Codex 員工控制檔保護需要 Codex CLI 0.164.0 以上；版本過舊或無法確認。請到儀表板首頁按「更新 Codex CLI」，或在終端機執行 `npm install -g @openai/codex@latest`。".into());
        }
        args.extend(employee_permission_args(dir, work_root, private_roots)?);
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
/// `call_dir` is this call's [`owned_home::OwnedCallDir`] (v1.84 security
/// re-review F1). The schema file used to be a `NamedTempFile` in the system
/// temp directory, which the employee permission profile grants
/// `:tmpdir`/`/tmp` write to: another employee's tools could unlink and
/// recreate `duduclaw-codex-schema-*` before Codex opened it and force a fixed
/// judge verdict. It is now `<call_dir>/output-schema.json`, created
/// exclusively (`create_new`, 0600 on Unix) inside the 0700 gateway-private
/// directory that the profile marks read-only for tools, and it is removed
/// with that directory (success, error, timeout, cancellation).
///
/// `None` schema ⇒ empty argv, byte-identical to before this existed. A write
/// failure degrades to "no flag" with a `warn!`: an adjudication must never
/// fail because the schema could not be staged (the parser is still
/// fail-closed downstream, so the worst case is the pre-round-8 behavior, not
/// a wrong verdict).
fn output_schema_args(
    schema: Option<&serde_json::Value>,
    call_dir: &std::path::Path,
) -> Vec<String> {
    let Some(schema) = schema else {
        return Vec::new();
    };
    let rendered = match serde_json::to_vec_pretty(schema) {
        Ok(b) => b,
        Err(e) => {
            warn!(runtime = "codex", error = %e, "output_schema is not serializable — continuing without structured output");
            return Vec::new();
        }
    };
    let path = call_dir.join(OUTPUT_SCHEMA_FILE);
    let Some(text) = duduclaw_core::path_compare::spawn_path(&path).to_str().map(str::to_owned) else {
        warn!(runtime = "codex", "Codex call directory is not UTF-8 — continuing without structured output");
        return Vec::new();
    };
    if let Err(e) = write_new_owner_only(&path, &rendered) {
        warn!(runtime = "codex", error = %e, "could not write --output-schema file — continuing without structured output");
        return Vec::new();
    }
    vec!["--output-schema".to_string(), text]
}

/// File name of the `--output-schema` file inside a call directory.
const OUTPUT_SCHEMA_FILE: &str = "output-schema.json";

/// Create `path` exclusively (never reusing an entry or following a planted
/// link) with mode 0600 on Unix before the first byte, and write `bytes`.
fn write_new_owner_only(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes).and_then(|()| file.flush())
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
/// invocation. The owned `CODEX_HOME` excludes user configuration; unmanaged
/// system and enterprise layers are checked separately before launch.
/// Authentication shares the original file so OAuth refreshes persist.
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
            toml_string_literal(duduclaw_core::path_compare::spawn_path(std::path::Path::new(command)).to_str().unwrap())
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
    upper == duduclaw_core::identity_token::ENV_DELEGATION_CONTEXT
        || upper == duduclaw_core::run_credential::ENV_RUN_CREDENTIAL
        || SECRET_SUFFIXES.iter().any(|s| upper.ends_with(s))
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
    mcp_override_args_with_extra_env(agent_id, home_dir, supports_env_vars, &[])
}

fn mcp_override_args_with_extra_env(
    agent_id: &str,
    home_dir: &std::path::Path,
    supports_env_vars: bool,
    extra: &[(String, String)],
) -> McpOverrides {
    if !supports_env_vars {
        // Old Codex cannot forward secrets privately. Do not disclose a new
        // delegation proof through argv; further delegation is refused.
        return McpOverrides {
            args: mcp_override_args(agent_id, home_dir),
            process_env: Vec::new(),
        };
    }
    let Some(mut def) = super::duduclaw_mcp_server_json_for_home(agent_id, home_dir) else {
        return McpOverrides::default();
    };
    if let Some(env) = def.get_mut("env").and_then(|e| e.as_object_mut()) {
        for (key, value) in extra { env.insert(key.clone(), serde_json::json!(value)); }
    }
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
    let mut secret_names = vec![duduclaw_core::run_credential::ENV_RUN_CREDENTIAL.to_string(),
        duduclaw_core::run_credential::ENV_IDENTITY_ENDPOINT.to_string(),
        duduclaw_core::run_credential::ENV_IDENTITY_SERVER.to_string()];
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
        if !secret_names.contains(k) { secret_names.push(k.clone()); }
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

/// Preserve the host container decision through Codex's MCP environment scrub.
fn turn_source_override_args_with_namespace_guard(
    pairs: &[(String, String)],
    namespace_guard_active: bool,
) -> Vec<String> {
    let mut pairs = pairs.to_vec();
    if namespace_guard_active {
        pairs.push(("DUDUCLAW_IN_CONTAINER".into(), "1".into()));
    }
    turn_source_override_args(&pairs)
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
        let val = if k == "DUDUCLAW_HOME" {
            duduclaw_core::path_compare::spawn_path(std::path::Path::new(&val))
                .to_str().unwrap().to_owned()
        } else { val };
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
    let mut cmd = tokio::process::Command::new(duduclaw_core::path_compare::spawn_path(std::path::Path::new(bin)));
    duduclaw_core::spawn_env::apply_agent_cli_env_allowlist(&mut cmd);
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
    super::codex_sandbox::clear(bin);
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
                        if matches!(item.get("phase").and_then(|v| v.as_str()), Some("commentary" | "analysis"))
                            || item.get("channel").and_then(|v| v.as_str()) == Some("analysis")
                        {
                            content.clear();
                            continue;
                        }
                        match item.get("type").and_then(|t| t.as_str()) {
                            Some("message") => {
                                if let Some(text) = item
                                    .get("content")
                                    .and_then(|c| c.as_array())
                                    .and_then(|arr| {
                                        arr.iter().rev().find(|b| {
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
                                        arr.iter().rev().find(|b| {
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

/// Track completed visible messages for timeout diagnostics. Only the final
/// assistant segment is eligible as reply text.
fn completed_codex_messages(stdout: &str) -> Vec<String> {
    stdout.lines().filter_map(|line| {
        let event: CodexEvent = serde_json::from_str(line).ok()?;
        if event.event_type != "item.completed" {
            return None;
        }
        let item = event.extra.get("item")?;
        if matches!(item.get("phase").and_then(|v| v.as_str()), Some("commentary" | "analysis"))
            || item.get("channel").and_then(|v| v.as_str()) == Some("analysis")
        { return None; }
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
                .last().unwrap_or("").to_string()
        };
        (!text.trim().is_empty()).then_some(text)
    }).collect()
}

// Bound every retained representation independently. Pipes are always drained,
// including oversized lines and streams beyond the retention budget.
const MAX_STDOUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_STDOUT_LINE_BYTES: usize = 1024 * 1024;
const MAX_STDERR_BYTES: usize = 64 * 1024;

tokio::task_local! { static CODEX_IDLE_TIMEOUT: u64; }

#[derive(Default)]
struct CodexOutput {
    idle: Option<crate::work_outcome::IdleWatch>,
    terminal_at: Option<tokio::time::Instant>,
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
        let line_bytes = duduclaw_core::run_credential::redact(&String::from_utf8_lossy(&line_bytes))
            .into_owned().into_bytes();
        if self.stdout.len() + line_bytes.len() <= MAX_STDOUT_BYTES {
            self.stdout.extend_from_slice(&line_bytes);
        } else {
            self.truncated = true;
        }
        let line = String::from_utf8_lossy(&line_bytes);
        // Parse each bounded event as it arrives, even after the raw-output
        // retention cap. In particular, final usage must not disappear merely
        // because earlier tool output consumed the raw-log budget.
        if let Ok(event) = serde_json::from_str::<CodexEvent>(&line) {
            if event.event_type == "turn.completed" && self.terminal_at.is_none() {
                self.terminal_at = Some(tokio::time::Instant::now());
            }
            if let Some(idle) = self.idle.as_mut() {
                if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&line) { idle.observe_codex(&raw); }
            }
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
        // The latest assistant event may be blank or reasoning-only; never
        // revive an older narration just because it was non-empty.
        Ok(duduclaw_core::think_tags::strip_reasoning_prefix(&self.content))
    }
}

/// All readers are local futures. Cancellation drops the readers and the
/// managed leader, which kills its group and schedules reaping.
#[cfg(test)]
async fn collect_codex_output(
    cmd: &mut tokio::process::Command,
    timeout: std::time::Duration,
) -> Result<CodexOutput, String> {
    collect_codex_output_with_input(cmd, timeout, None, None).await
}

#[cfg(test)]
async fn collect_codex_output_with_input(
    cmd: &mut tokio::process::Command,
    timeout: std::time::Duration,
    input: Option<&[u8]>,
    work_root: Option<&owned_home::CheckedWorkRoot>,
) -> Result<CodexOutput, String> {
    collect_codex_output_bound(cmd, timeout, input, work_root, None).await
}

async fn collect_codex_output_bound(
    cmd: &mut tokio::process::Command, timeout: std::time::Duration, input: Option<&[u8]>,
    work_root: Option<&owned_home::CheckedWorkRoot>,
    binding: Option<duduclaw_core::run_credential::SpawnBinding>,
) -> Result<CodexOutput, String> {
    if let Some(root) = work_root { root.reverify()?; }
    #[cfg(test)]
    if let Ok(capture) = tests::COMMAND_CAPTURE.try_with(std::sync::Arc::clone) {
        // RM-204: also what the process would have read on stdin and the
        // owned CODEX_HOME's instructions file (removed when the call returns).
        if let Ok(extra) = tests::INPUT_CAPTURE.try_with(std::sync::Arc::clone) {
            let codex_home = cmd.as_std().get_envs().find(|(key, _)| *key == "CODEX_HOME")
                .and_then(|(_, value)| value).map(std::path::PathBuf::from);
            let mut extra = extra.lock().unwrap();
            extra.stdin = input.map(<[u8]>::to_vec);
            extra.agents_md = codex_home.as_ref().and_then(|home| std::fs::read(home.join("AGENTS.md")).ok());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                extra.agents_md_mode = codex_home.as_ref()
                    .and_then(|home| std::fs::metadata(home.join("AGENTS.md")).ok())
                    .map(|meta| meta.permissions().mode() & 0o777);
            }
            extra.codex_home = codex_home;
        }
        *capture.lock().unwrap() = Some(std::mem::replace(cmd, tokio::process::Command::new("captured-command")));
        return Err("codex command captured before backend execution".into());
    }
    #[cfg(test)]
    if let Ok(output) = tests::COLLECT_PROBE.try_with(|probe| probe(cmd)) {
        return output;
    }
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    cmd.stdin(if input.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // After the test probe returns. Hold the lease across spawn only; a failed
    // spawn drops it without marking the round handed off.
    let admission = crate::autonomous_runtime::admit_current().await?;
    // Admission can await. Recheck after it, immediately before the backend
    // spawn call; retain the checked canonical spelling throughout preparation.
    if let Some(root) = work_root { root.reverify()?; }
    let mut managed = duduclaw_core::execution_gate::with_runtime(
        duduclaw_core::execution_gate::RuntimeKind::Codex,
        crate::process_group::BackendChild::spawn_class_bound(cmd, crate::process_group::execution_class(
            duduclaw_core::execution_gate::ExecutionClass::EmployeeMain), binding),
    ).await
        .map_err(|e| crate::process_group::backend_error(e, "Failed to spawn codex"))?;
    crate::autonomous_runtime::mark_handoff();
    drop(admission);
    let mut stdin = managed.child().stdin.take();
    let writer = async move {
        if let (Some(sink), Some(bytes)) = (stdin.as_mut(), input) {
            sink.write_all(bytes).await?;
            sink.shutdown().await?;
        }
        Ok::<(), std::io::Error>(())
    };
    tokio::pin!(writer);
    let mut writer_done = false;
    let mut stdout = managed.child().stdout.take().ok_or("Failed to capture codex stdout")?;
    let mut stderr = managed.child().stderr.take().ok_or("Failed to capture codex stderr")?;
    let mut output = CodexOutput { idle: Some(crate::work_outcome::IdleWatch::new(CODEX_IDLE_TIMEOUT.try_with(|v| *v).unwrap_or(900))), ..Default::default() };
    let mut idle_stopped = false;
    let mut stdout_buffer = [0u8; 8192];
    let mut stderr_buffer = [0u8; 8192];
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut status = None;
    let result = tokio::time::timeout(timeout, async {
        while !writer_done || !stdout_done || !stderr_done || status.is_none() {
            let terminal_at = output.terminal_at;
            tokio::select! {
                _ = async {
                    if let Some(at) = terminal_at {
                        tokio::time::sleep_until(at + std::time::Duration::from_secs(5)).await;
                    } else { std::future::pending::<()>().await; }
                } => { break; }
                _ = output.idle.as_ref().expect("collector idle clock").expired() => { idle_stopped = true; break; }
                written = &mut writer, if !writer_done => {
                    written?;
                    writer_done = true;
                }
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
    if idle_stopped {
        managed.kill_and_reap().await;
        return Err(output.idle.as_ref().expect("collector idle clock").outcome(output.content.clone()).encode());
    }
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
            if output.terminal_at.is_none() {
                return Err(format!("Failed to read codex output: {error}"));
            }
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

/// Preserve API authentication before assigning the owned config home.
/// `--ignore-user-config` does not ignore auth (codex-cli 0.166.0 exec help).
fn apply_codex_auth_env(cmd: &mut tokio::process::Command) {
    if let Some(home) = std::env::var_os("CODEX_HOME") { cmd.env("CODEX_HOME", duduclaw_core::path_compare::spawn_path(std::path::Path::new(&home))); }
    if let Some(key) = std::env::var_os("OPENAI_API_KEY") { cmd.env("OPENAI_API_KEY", key); }
}

#[path = "codex/external_config.rs"]
mod external_config;
pub(crate) use external_config::require_mcp_isolation;
#[cfg(test)]
pub(crate) use external_config::MCP_CONFIG_TEST_PATHS;

#[path = "codex/owned_home.rs"]
pub(crate) mod owned_home;

/// Each launch gets a private config root. Only the auth file is shared;
/// native user, project and profile MCP configuration is never copied.
///
/// `root` is this call's [`owned_home::OwnedCallDir`] under the
/// gateway-private `<DUDUCLAW_HOME>/run/codex/` (H1): not in the temp
/// directory, `/tmp` or a working root, and marked read-only for tools in the
/// employee permission profile, so another employee's tools cannot replace
/// this call's `AGENTS.md` or swap the `auth.json` link.
///
/// **Remaining exposure, stated precisely.** `auth.json` here is a symbolic
/// link to the host credential store (`$CODEX_HOME/auth.json` of the gateway
/// process, or `~/.codex/auth.json`), never a copy: the token is not
/// duplicated into the per-call directory. Any process running as the same
/// OS user that may read files can read that host store — directly, or
/// through this link — and that includes every AI employee's tools, because
/// the employee profile grants `:root = "read"` and has no verified
/// "no access" entry (the protected-root entries only deny writes). The
/// same applies to the `AGENTS.md` next to it while the call runs: readable
/// by same-UID readers, no longer writable by employee tools. Removing that
/// read exposure needs a credential broker (the token outside every
/// employee-readable path), which this change does not add.
fn prepare_owned_codex_home(root: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let host = owned_home::host_codex_home();
    prepare_owned_codex_home_with_auth(root, host.as_deref())
}

fn prepare_owned_codex_home_with_auth(root: &std::path::Path, host: Option<&std::path::Path>) -> Result<std::path::PathBuf, String> {
    let owned = root.join("codex-home");
    std::fs::create_dir(&owned).map_err(|e| format!("cannot create Codex config home: {e}"))?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&owned, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot secure Codex config home: {e}"))?;
    }
    if let Some(host) = host {
        match std::fs::read(host.join("auth.json")) {
            Ok(_) => {
                // Codex 0.166 FileAuthStorage saves with OpenOptions::truncate,
                // not rename. Sharing the file preserves OAuth refresh writes.
                // https://github.com/openai/codex/blob/rust-v0.166.0/codex-rs/login/src/auth/storage.rs
                let auth = std::fs::canonicalize(host.join("auth.json"))
                    .map_err(|_| "cannot resolve Codex authentication store".to_string())?;
                #[cfg(unix)]
                std::os::unix::fs::symlink(&auth, owned.join("auth.json"))
                    .map_err(|_| "cannot share Codex authentication store".to_string())?;
                #[cfg(not(unix))]
                std::fs::hard_link(&auth, owned.join("auth.json"))
                    .map_err(|_| "cannot share Codex authentication store".to_string())?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("cannot read Codex authentication store".into()),
        }
    }
    Ok(owned)
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
    ) -> Result<(tokio::process::Command, owned_home::OwnedCallDir, owned_home::CheckedWorkRoot), String> {
        // H1: a fresh 0700 `utility-<random>` directory under the
        // gateway-private `<home>/run/codex`, never the temp directory. The
        // guard owns cleanup on success, spawn failure, timeout and cancellation.
        let work_dir = owned_home::OwnedCallDir::create(&context.home_dir, owned_home::UTILITY_PREFIX)?;
        let mut cmd = tokio::process::Command::new(duduclaw_core::path_compare::spawn_path(std::path::Path::new(&self.codex_path)));
        duduclaw_core::spawn_env::apply_agent_cli_env_allowlist(&mut cmd);
        // Share the real auth file, preserving refresh writes across launches.
        // CODEX_HOME is an auth location, absent from the generic spawn allowlist.
        let codex_home = prepare_owned_codex_home(work_dir.path())?;
        cmd.env("CODEX_HOME", duduclaw_core::path_compare::spawn_path(&codex_home));
        if let Some(key) = std::env::var_os("OPENAI_API_KEY") { cmd.env("OPENAI_API_KEY", key); }
        cmd.args(utility_args());
        cmd.args(effort_args(context.effort));
        // F1: the schema lives in this call's private directory, removed with it.
        cmd.args(output_schema_args(crate::runtime_dispatch::output_schema_override().as_ref(), work_dir.path()));
        // `-` selects stdin; no conversation bytes enter argv on any OS.
        let _ = prompt;
        cmd.arg("-");
        if !context.model.is_empty() { cmd.arg("-m").arg(&context.model); }
        // Both process cwd and Codex's working root exclude employee/project files.
        // The utility directory is inside `<home>/run/codex`, so it passes.
        let work_root = owned_home::check_work_root(&context.home_dir, work_dir.path())?;
        external_config::check_project_layers(work_root.canonical_path(), None)?;
        work_root.apply_to_command(&mut cmd);
        let caps = utility_native_caps(context.capabilities.as_ref());
        super::apply_native_sandbox(&mut cmd, Some(&caps), Some(work_root.spawn_path()), "codex")?;
        // RM-181 defence in depth, as for employee launches.
        refuse_project_trust_override(&cmd, None)?;
        Ok((cmd, work_dir, work_root))
    }
}

#[cfg(test)]
fn employee_base_command(codex_path: &str, model: &str) -> tokio::process::Command {
    employee_base_command_with_caps(codex_path, model, None)
}

fn employee_base_command_with_caps(
    codex_path: &str, model: &str, caps: Option<&CapabilitiesConfig>,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(duduclaw_core::path_compare::spawn_path(std::path::Path::new(codex_path)));
    crate::runtime_dispatch::apply_claude_fallback_spawn_env(&mut cmd, caps);
    // Auth remains in the host store. These arguments
    // express the desired base but are NOT evidence of complete isolation.
    apply_codex_auth_env(&mut cmd);
    cmd.env_remove("CLAUDE_CODE_MESSAGING_TOKEN");
    cmd.args(["exec", "--json", "--skip-git-repo-check", "--ignore-user-config",
        "-c", "mcp_servers={}",
        // RM-204: no working-directory / ancestor AGENTS.md is merged in; the
        // instructions come from the owned `<CODEX_HOME>/AGENTS.md` only.
        "-c", "project_doc_max_bytes=0",
        // H1 item 3 (defence in depth): the commands Codex runs for the
        // employee do not inherit the variable naming this call's owned home.
        "-c", EXCLUDE_CODEX_HOME_FROM_TOOLS]);
    if !model.is_empty() { cmd.arg("-m").arg(model); }
    cmd
}

/// `shell_environment_policy.exclude` (Codex config reference: patterns
/// removed from the environment of the commands Codex spawns, applied after
/// `inherit` and the default `*KEY*`/`*SECRET*`/`*TOKEN*` excludes). Only the
/// owned-home variable is named; `PATH` and everything else keep their
/// current inheritance.
const EXCLUDE_CODEX_HOME_FROM_TOOLS: &str = "shell_environment_policy.exclude=[\"CODEX_HOME\"]";

/// RM-204: write the employee's system prompt to `<owned CODEX_HOME>/AGENTS.md`
/// (owner-only, 0600). The directory is this call's private home under
/// `<DUDUCLAW_HOME>/run/codex` (H1), so the file is removed with it. Fail
/// closed: a prompt that cannot be written is never silently dropped.
fn write_owned_instructions(codex_home: &std::path::Path, system_prompt: &str) -> Result<(), String> {
    write_new_owner_only(&codex_home.join("AGENTS.md"), system_prompt.as_bytes())
        .map_err(|e| format!("cannot write Codex instructions: {e}"))
}

/// Is `key` (the part of a `-c key=value` override before `=`) the Codex
/// `projects` trust table or one of its entries? Exact segment match.
fn is_projects_key(key: &str) -> bool {
    let key = key.trim().trim_matches('"');
    key == "projects" || key.starts_with("projects.") || key.starts_with("projects[")
}

/// RM-181 defence in depth: Codex loads a project-layer `.codex/config.toml`
/// only for projects marked trusted (`[projects]` in the config home or a
/// `-c projects…` override). DuDuClaw never grants trust; refuse a launch
/// whose argv or owned config would.
fn refuse_project_trust_override(
    cmd: &tokio::process::Command,
    owned_config: Option<&std::path::Path>,
) -> Result<(), String> {
    let refusal = || duduclaw_agent::mcp_spawn_gate::spawn_gate_error(
        "Codex launch refused: a project trust override must never be passed");
    let args: Vec<String> = cmd.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let value = if arg == "-c" || arg == "--config" {
            index += 1;
            args.get(index).map(String::as_str)
        } else {
            arg.strip_prefix("--config=")
        };
        if let Some(value) = value {
            let key = value.split_once('=').map_or(value, |(key, _)| key);
            if is_projects_key(key) {
                return Err(refusal());
            }
        }
        index += 1;
    }
    if let Some(path) = owned_config {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let table: toml::Table = text.parse().map_err(|_| refusal())?;
                if table.contains_key("projects") {
                    return Err(refusal());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(refusal()),
        }
    }
    Ok(())
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
        // Check unmanaged layers before preparing files or collecting output.
        require_mcp_isolation()?;
        #[cfg(not(target_os = "linux"))]
        super::isolation_gate::require_integrated(super::isolation_gate::PendingRuntime::Codex)?;
        #[cfg(target_os = "linux")]
        if duduclaw_core::execution_gate::isolation_required() {
            // RM-103 S3: the employee's native write/shell restrictions hold in
            // protected mode too — refused before the isolated route.
            super::protected_coarse_native_gate(context.capabilities.as_ref(), "codex", &context.agent_id)?;
            let payload = super::generic_cli::build_prompt(context, prompt, "codex");
            let out = crate::isolation_mcp::runtime::execute_native(
                duduclaw_isolation::execution::RuntimeKind::Codex, payload, context).await?;
            let stdout = std::str::from_utf8(&out).map_err(|_| "invalid isolated Codex output")?;
            let (content, input_tokens, output_tokens, _) = parse_codex_stdout(stdout);
            if content.trim().is_empty() { return Err("isolated Codex returned no answer".into()); }
            return Ok(RuntimeResponse { partial_timeout_secs: None, content, input_tokens, output_tokens,
                cache_read_tokens: 0, model_used: context.model.clone(), runtime_name: "codex".into() });
        }
        if crate::runtime_dispatch::in_utility_call() {
            let (mut cmd, _work_dir, work_root) = self.prepare_utility_command(prompt, context)?;
            let payload = utility_stdin_payload(prompt, &context.system_prompt);
            return execute_codex_command_with_input(&mut cmd, context, Some(payload.as_bytes()), Some(&work_root)).await;
        }
        // Explore lane: `-s read-only` whatever the capability level, and the
        // duduclaw MCP server registered with the lane variable (below).
        let explore = crate::explore_lane::in_explore();
        info!(agent = %context.agent_id, "CodexRuntime: executing via codex exec --json");

        // RM-204: the system prompt goes to the per-call owned
        // `CODEX_HOME/AGENTS.md` (below), which Codex loads whole — no argv,
        // no 32 KiB project-doc cap — so it is not truncated here any more.
        let system_prompt: &str = &context.system_prompt;

        // W1 (capability enforcement): derive sandbox/approval flags from the
        // agent's capabilities instead of the former blanket `--full-auto`.
        //
        // RM-103 S3 (Daybreak #2): Codex's own `apply_patch` and shell never
        // reach the MCP gate, so a native write/shell restriction it cannot
        // express (an inherited `modify = ask`, `Bash(git:*)`, …) forces
        // `-s read-only` or refuses the spawn — before anything is prepared.
        // `-s read-only` really blocks writes, so it is enforceable here.
        let coarse_caps = super::coarse_native_caps(
            context.capabilities.as_ref(),
            "codex",
            &context.agent_id,
            true,
        )?;
        let caps = coarse_caps.as_ref().or(context.capabilities.as_ref());
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
        // 2) Owned per-agent `.codex/config.toml` for operators who run
        //    codex manually in the agent dir with CODEX_HOME pointed there.
        //    Failure is fatal: a stale native table must never reach Codex.
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
            written.map_err(|e| duduclaw_agent::mcp_spawn_gate::spawn_gate_error(
                &format!("{}: cannot prepare owned MCP config: {e}", dir.join(".codex/config.toml").display())))?;
        }

        // Working root. Normally the agent's own directory; a caller may
        // override it via `super::SPAWN_OVERRIDE` (today: the team composer,
        // putting a role member in the employee's workspace so its files
        // outlive the throwaway scaffold — design §4.3 E3). Identity is NOT
        // affected: the `-c mcp_servers.duduclaw.env.DUDUCLAW_AGENT_ID`
        // override carries the member id independently of cwd.
        //
        // Select the configured directory once, then discard that spelling.
        // Even the inherited-cwd case must pass the gate and be pinned to an
        // absolute canonical path, rather than resolving again in the child.
        let work_root = super::resolve_spawn_work_dir(context.agent_dir.as_deref(), &context.agent_id)
            .or_else(|| std::env::current_dir().ok())
            .ok_or("cannot resolve Codex project root")?;
        let work_root = owned_home::check_work_root(&context.home_dir, &work_root)?;
        let owned_config = context.agent_dir.as_ref().map(|dir| dir.join(".codex/config.toml"));
        external_config::check_project_layers(work_root.canonical_path(), owned_config.as_deref())?;

        // H1: `<home>/run/codex/call-<random>` (0700, gateway-private, read-only
        // for tools via the profile below), removed when `config_root` drops —
        // success, error, timeout or cancellation.
        let config_root = owned_home::OwnedCallDir::create(&context.home_dir, owned_home::CALL_PREFIX)?;
        let codex_home = prepare_owned_codex_home(config_root.path())?;
        // RM-204: the employee's instructions live in this call's own
        // CODEX_HOME (removed with `config_root`), never in the employee
        // directory: Codex always loads `<CODEX_HOME>/AGENTS.md` (also under
        // `--ignore-user-config`, without the project-doc size cap), and
        // concurrent conversations of one employee no longer overwrite each
        // other. `-c project_doc_max_bytes=0` (employee_base_command) keeps
        // any working-directory / ancestor AGENTS.md out of the request.
        if !system_prompt.is_empty() {
            write_owned_instructions(&codex_home, system_prompt)?;
        }
        let mut cmd = employee_base_command_with_caps(&self.codex_path, &context.model, caps);
        cmd.env("CODEX_HOME", duduclaw_core::path_compare::spawn_path(&codex_home));
        let egress_mode = duduclaw_core::egress_guard::effective_mode(&context.home_dir, context.agent_dir.as_deref());
        let effective_caps = egress_sandbox_caps(caps, egress_mode);
        if employee_needs_namespace_guard(effective_caps, explore) {
            duduclaw_core::ns_guard::guard_command(&mut cmd);
        }
        #[cfg(unix)]
        {
            let needs_permissions = !explore
                && sandbox_level_for(effective_caps) == duduclaw_core::types::SandboxLevel::WorkspaceWrite;
            let supports_permissions = needs_permissions
                && codex_supports_employee_permissions(&self.codex_path).await;
            let private_roots = owned_home::protected_roots(&context.home_dir);
            let args = employee_egress_sandbox_args(caps, explore, egress_mode, context.agent_dir.as_deref(), Some(work_root.spawn_path()), supports_permissions, &private_roots)
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
        // F1: written into `config_root` (this call's private directory),
        // which is held to the end of this function and removes it on drop.
        let schema_args =
            output_schema_args(crate::runtime_dispatch::output_schema_override().as_ref(), config_root.path());
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
        if !supports_env_vars {
            return Err(duduclaw_agent::mcp_spawn_gate::spawn_gate_error(
                "Codex must support MCP env_vars for private run identity forwarding"));
        }
        let delegation_env = crate::claude_runner::delegation_context_env(&context.home_dir, &context.agent_id,
            crate::runtime_config::call_timeout_secs(context.agent_dir.as_deref()));
        let mcp_overrides = mcp_override_args_with_extra_env(
            &context.agent_id, &context.home_dir, supports_env_vars, &delegation_env,
        );
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
        // Codex clears the MCP child environment. Carry our host decision
        // explicitly so marker-only containers also self-guard their MCP.
        cmd.args(turn_source_override_args_with_namespace_guard(
            &source_pairs,
            duduclaw_core::ns_guard::is_active(),
        ));

        // The system prompt was written to the owned CODEX_HOME above, for an
        // overridden working root too (it used to travel inline there so the
        // employee's own AGENTS.md was not clobbered; the owned home has no
        // such conflict). An override only moves `--cd` now.

        // Prepend conversation history to prompt (Codex exec has no native multi-turn)
        let augmented_prompt = if context.conversation_history.is_empty() {
            prompt.to_string()
        } else {
            super::format_history_as_prompt(&context.conversation_history, prompt)
        };

        // RM-204: `-` makes `codex exec` read the prompt from stdin, byte-exact
        // (history + recap + message can exceed the Linux 128 KiB
        // single-argument limit). No conversation bytes enter argv, so a
        // prompt starting with '-' can no longer be parsed as a flag either.
        cmd.arg("-");

        // One gate result supplies both CLI and OS working directories.
        work_root.apply_to_command(&mut cmd);

        // stdin carries the prompt and is closed after it (the collector
        // pipes it); an inherited open pipe would stall the run until EOF.
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Native OS sandbox (opt-in). Layered on top of the CLI `--sandbox`
        // flag; fail-closed if required but unavailable. Scoped to the
        // working root, so an overridden cwd is the directory that gets
        // write access (a sandbox scoped to the scaffold would forbid exactly
        // the writes the override exists to allow).
        super::apply_native_sandbox(&mut cmd, caps, Some(work_root.spawn_path()), "codex")?;

        // RM-181 defence in depth: Codex loads project-layer configuration
        // only for trusted projects; DuDuClaw never grants trust.
        refuse_project_trust_override(&cmd, owned_config.as_deref())?;

        execute_codex_command_with_input(&mut cmd, context, Some(augmented_prompt.as_bytes()), Some(&work_root)).await
    }

    async fn is_available(&self) -> bool {
        if duduclaw_core::execution_gate::isolation_required() {
            #[cfg(target_os = "linux")]
            { return crate::isolation_mcp::runtime::available(duduclaw_isolation::execution::RuntimeKind::Codex).await; }
            #[cfg(not(target_os = "linux"))]
            return false;
        }
        probe_codex_version_output(&self.codex_path).await.is_some()
    }
}

// Shared by conversational and utility calls: retain bounded incremental output,
// process-group cleanup, timeout partials, usage and native-event reporting.
#[cfg(test)]
async fn execute_codex_command(
    cmd: &mut tokio::process::Command,
    context: &RuntimeContext,
) -> Result<RuntimeResponse, String> {
    execute_codex_command_with_input(cmd, context, None, None).await
}

fn utility_stdin_payload(prompt: &str, system: &str) -> String {
    let system = duduclaw_core::truncate_bytes(system, 65536);
    if system.is_empty() { prompt.to_owned() } else {
        format!("<role_system_prompt>\n{}\n</role_system_prompt>\n\n{prompt}",
            system.replace("</role_system_prompt>", "&lt;/role_system_prompt&gt;"))
    }
}

async fn execute_codex_command_with_input(
    cmd: &mut tokio::process::Command,
    context: &RuntimeContext,
    input: Option<&[u8]>,
    work_root: Option<&owned_home::CheckedWorkRoot>,
) -> Result<RuntimeResponse, String> {
    let timeout_secs = crate::runtime_config::call_timeout_secs(context.agent_dir.as_deref());
    let agent = if context.agent_id.is_empty() && crate::runtime_dispatch::in_utility_call() { "system" } else { &context.agent_id };
    let _run_identity = crate::run_identity::issue(&context.home_dir, agent, "codex", timeout_secs)?;
    _run_identity.apply(cmd);
    // Include lists are restrictive; inherit all keeps the existing shell
    // environment and its default KEY/SECRET/TOKEN exclusions intact.
    cmd.args(["-c", "shell_environment_policy.inherit=\"all\""]);
    let idle = crate::runtime_config::idle_timeout_secs(&context.home_dir, context.agent_dir.as_deref());
    let output = CODEX_IDLE_TIMEOUT.scope(idle, collect_codex_output_bound(
        cmd, std::time::Duration::from_secs(timeout_secs), input, work_root, Some(_run_identity.spawn_binding()),
    )).await?;

    if let Some(status) = output.status {
        if output.terminal_at.is_none() && !status.success() {
            let stderr_raw = String::from_utf8_lossy(&output.stderr);
            let stderr = duduclaw_core::run_credential::redact(&stderr_raw);
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
        partial_timeout_secs: (output.status.is_none() && output.terminal_at.is_none()).then_some(timeout_secs),
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
    if output.status.is_none() && output.terminal_at.is_none() {
        crate::runtime_dispatch::record_discarded_runtime_usage(context, &response).await;
        return Err(crate::work_outcome::WorkOutcome::TimeoutTotal { seconds: timeout_secs,
            partial: output.completed.last().cloned().unwrap_or_default() }.encode());
    }

    // JSONL metadata/reasoning lines are never fallback answers.
    response.content = duduclaw_core::think_tags::strip_reasoning_prefix(&response.content);

    // Still empty ⇒ FAILURE, not success: Ok("") would be silently dropped
    // by every channel and poison the session with an empty assistant turn.
    if response.content.trim().is_empty() {
        let stderr_raw = String::from_utf8_lossy(&output.stderr);
        let stderr = duduclaw_core::run_credential::redact(&stderr_raw);
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
pub(crate) mod tests {
    use super::*;

    #[test]
    fn delegation_proof_uses_secret_env_vars() {
        let home = tempfile::tempdir().unwrap();
        let _key = zeroize::Zeroizing::new(duduclaw_core::identity_token::ensure_key(home.path()).unwrap());
        let baseline = mcp_override_args_with("tester", home.path(), true);
        assert!(!baseline.args.is_empty(), "MCP server must be resolvable");
        assert!(baseline.process_env.iter().any(|(key, _)| key == duduclaw_core::ENV_AGENT_TOKEN));
        let name = duduclaw_core::identity_token::ENV_DELEGATION_CONTEXT;
        let proof = "host-proof-do-not-expose";
        let extra = vec![(name.to_string(), proof.to_string()),
                         (duduclaw_core::ENV_DELEGATION_SENDER.to_string(), "researcher".to_string())];
        let merged = mcp_override_args_with_extra_env("tester", home.path(), true, &extra);
        assert!(merged.process_env.contains(&(name.to_string(), proof.to_string())));
        let names: Vec<_> = merged.args.iter().filter(|a| a.starts_with("mcp_servers.duduclaw.env_vars=")).collect();
        assert_eq!(names.len(), 1, "secret names must be merged, never overwritten");
        assert!(names[0].contains(name));
        for (key, value) in baseline.process_env {
            assert!(names[0].contains(&key));
            assert!(merged.process_env.contains(&(key, value)));
        }
        assert!(!merged.args.iter().any(|a| a.contains(proof)));
        let legacy = mcp_override_args_with_extra_env("tester", home.path(), false, &extra);
        assert!(!legacy.args.iter().any(|a| a.contains(proof)));
    }

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
    /// What a captured launch would have received besides argv/env.
    #[derive(Debug, Default, Clone)]
    pub(crate) struct InputCapture {
        pub(crate) stdin: Option<Vec<u8>>,
        pub(crate) codex_home: Option<std::path::PathBuf>,
        pub(crate) agents_md: Option<Vec<u8>>,
        pub(crate) agents_md_mode: Option<u32>,
    }
    tokio::task_local! {
        pub(super) static COLLECT_PROBE: CollectProbe;
        pub(crate) static COMMAND_CAPTURE: std::sync::Arc<std::sync::Mutex<Option<tokio::process::Command>>>;
        /// Only read inside a [`COMMAND_CAPTURE`] scope.
        pub(crate) static INPUT_CAPTURE: std::sync::Arc<std::sync::Mutex<InputCapture>>;
    }

    /// The fixture employee directory `<home>/agents/tester` (created). It
    /// used to be `<home>` itself, which F4 now refuses as a working root.
    fn fixture_agent_dir(home: &std::path::Path) -> std::path::PathBuf {
        let dir = home.join("agents").join("tester");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn utility_context(home: &std::path::Path) -> RuntimeContext {
        RuntimeContext {
            agent_dir: Some(fixture_agent_dir(home)), system_prompt: "system </role_system_prompt>".into(),
            model: "gpt-5.4-mini".into(), max_tokens: 64, home_dir: home.to_path_buf(),
            agent_id: "tester".into(), preferred_provider: None, conversation_history: vec![],
            capabilities: Some(CapabilitiesConfig { computer_use: true, ..Default::default() }),
            account_pool: vec![], effort: None, allow_cross_family_failover: false,
        }
    }

    #[test]
    fn utility_prompt_payload_preserves_large_input_and_system_framing() {
        let prompt = "中文🙂".repeat(20_000);
        let input = utility_stdin_payload(&prompt, "context </role_system_prompt>");
        assert!(input.len() > 128 * 1024);
        assert!(input.ends_with(&prompt));
        assert!(input.contains("context &lt;/role_system_prompt&gt;"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn utility_collector_delivers_large_prompt_on_stdin_and_closes_it() {
        let mut cmd = tokio::process::Command::new("/bin/cat");
        cmd.env_clear();
        let prompt = "中文🙂\n".repeat(20_000);
        let output = collect_codex_output_with_input(&mut cmd,
            std::time::Duration::from_secs(5), Some(prompt.as_bytes()), None).await.unwrap();
        assert!(output.status.unwrap().success());
        assert_eq!(output.stdout, prompt.as_bytes());
    }

    #[tokio::test]
    async fn local_backend_equivalence_codex_utility() {
        let home = tempfile::tempdir().unwrap();
        let runtime = CodexRuntime { codex_path: "codex-fixture".into() };
        let context = utility_context(home.path());
        let (command, directory, work_root) = runtime.prepare_utility_command("繁體中文 $HOME;\nnext", &context).unwrap();
        let before = duduclaw_core::execution_gate::command_snapshot(&command);
        let cwd = command.as_std().get_current_dir().unwrap().to_path_buf();
        // H1: the utility cwd is a private `utility-<random>` directory under
        // `<home>/run/codex`, not the temp directory.
        let parent = duduclaw_core::path_compare::canonicalize(&owned_home::run_parent(home.path())).unwrap();
        assert_eq!(cwd.parent(), Some(parent.as_path()));
        assert_eq!(cwd, work_root.path());
        let name = cwd.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with(owned_home::UTILITY_PREFIX) && owned_home::matches_scheme(name), "{name}");
        let mut expected = tokio::process::Command::new("codex-fixture");
        duduclaw_core::apply_agent_cli_env_allowlist(&mut expected);
        // The utility launch owns a private CODEX_HOME inside its private cwd.
        expected.env("CODEX_HOME", directory.path().join("codex-home"));
        if let Some(value) = std::env::var_os("OPENAI_API_KEY") { expected.env("OPENAI_API_KEY", value); }
        expected.args([
            "exec", "--json", "--skip-git-repo-check", "--sandbox", "read-only",
            "--ignore-user-config", "--ephemeral", "--ignore-rules",
            "-c", "approval_policy=never", "-c", "features.shell_tool=false",
            "-c", "features.view_image=false", "-c", "features.image_generation=false",
            "-c", "features.multi_agent=false", "-c", "features.multi_agent_v2=false",
            "-c", "agents.enabled=false", "-c", "web_search=\"disabled\"",
            "-c", "features.hooks=false", "-c", "features.plugins=false",
            "-c", "features.apps=false", "-c", "features.skip_host_skill_discovery=true",
            "-c", "project_doc_max_bytes=0",
            "-",
            "-m", "gpt-5.4-mini", "--cd",
        ]).arg(&cwd).current_dir(&cwd);
        assert_eq!(before, duduclaw_core::execution_gate::command_snapshot(&expected));
        assert_eq!(duduclaw_core::execution_gate::local_command_roundtrip(command).await.unwrap(), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_result_survives_hung_codex_cleanup() {
        let home = tempfile::tempdir().unwrap();
        let context = utility_context(home.path());
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", r#"printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"完成"}}' '{"type":"turn.completed","usage":{"input_tokens":2,"output_tokens":3}}'; exec /bin/sleep 300"#]);
        let output = CODEX_IDLE_TIMEOUT.scope(1,
            tokio::time::timeout(std::time::Duration::from_secs(10),
                collect_codex_output(&mut cmd, std::time::Duration::from_secs(300))))
            .await.expect("terminal cleanup must be bounded").expect("completed result");
        assert!(output.terminal_at.is_some());
        assert_eq!(output.content, "完成");
        assert_eq!(output.output_tokens, 3);
        // The production response path must also accept a completed turn
        // whose cleanup needed to kill the still-running process.
        let output = std::sync::Mutex::new(Some(output));
        let probe: CollectProbe = Box::new(move |_| Ok(output.lock().unwrap().take().unwrap()));
        let mut cmd = tokio::process::Command::new("fixture-never-spawned");
        let response = COLLECT_PROBE.scope(probe, execute_codex_command(&mut cmd, &context)).await.unwrap();
        assert_eq!(response.content, "完成");
        assert_eq!(response.partial_timeout_secs, None);
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
            let error = if utility {
                crate::runtime_dispatch::CHANNEL_REPLY_PARTIAL.scope((), crate::runtime_dispatch::UTILITY_CALL.scope((), call)).await.unwrap_err()
            } else { crate::runtime_dispatch::CHANNEL_REPLY_PARTIAL.scope((), call).await.unwrap_err() };
            assert_eq!(crate::work_outcome::WorkOutcome::decode(&error), Some(crate::work_outcome::WorkOutcome::TimeoutTotal {
                seconds: crate::runtime_config::DEFAULT_RUN_TIMEOUT_SECS, partial:"PASS".into() }));
        }
    }

    #[tokio::test]
    async fn local_backend_equivalence_codex_conversation() {
        let home = tempfile::tempdir().unwrap();
        let mut context = utility_context(home.path());
        context.capabilities = None;
        let bin = "/nonexistent/codex-backend-equivalence";
        cached_codex_support(bin, || async { Some("codex-cli 0.164.0".into()) }).await;
        let runtime = CodexRuntime { codex_path: bin.into() };
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let input = std::sync::Arc::new(std::sync::Mutex::new(InputCapture::default()));
        // No unmanaged host Codex layers participate in this fixture.
        let error = MCP_CONFIG_TEST_PATHS.scope(vec![], INPUT_CAPTURE.scope(input.clone(),
            COMMAND_CAPTURE.scope(captured.clone(), runtime.execute("繁體中文 $HOME;\nnext", &context)))).await.unwrap_err();
        assert_eq!(error, "codex command captured before backend execution");
        let input = input.lock().unwrap().clone();
        let command = captured.lock().unwrap().take().unwrap();
        let before = duduclaw_core::execution_gate::command_snapshot(&command);
        let captured_env = |key: &str| command.as_std().get_envs().find(|(name, _)| *name == key)
            .and_then(|(_, value)| value).map(std::ffi::OsStr::to_os_string);
        // The owned per-launch CODEX_HOME is a fresh private directory.
        let codex_home = captured_env("CODEX_HOME").expect("owned CODEX_HOME");
        let mut expected = tokio::process::Command::new(bin);
        if let Some(key) = std::env::var_os("OPENAI_API_KEY") { expected.env("OPENAI_API_KEY", key); }
        expected.env("CODEX_HOME", &codex_home);
        expected.env_remove("CLAUDE_CODE_MESSAGING_TOKEN");
        expected.args(["exec", "--json", "--skip-git-repo-check", "--ignore-user-config",
            "-c", "mcp_servers={}", "-c", "project_doc_max_bytes=0",
            "-c", "shell_environment_policy.exclude=[\"CODEX_HOME\"]", "-m", "gpt-5.4-mini"]);
        expected.args(["--approve-for-me",
            "-c", "approval_policy=never", "-c", "sandbox_mode=\"workspace-write\""]);
        let agent = fixture_agent_dir(home.path());
        let canonical_agent = duduclaw_core::path_compare::canonicalize(&agent).unwrap();
        #[cfg(unix)]
        expected.args(employee_permission_args(&agent, Some(canonical_agent.as_path()),
            &owned_home::protected_roots(home.path())).unwrap());
        // Resolve the dynamic MCP registration independently of the captured command.
        let delegation_env = crate::claude_runner::delegation_context_env(home.path(), "tester",
            crate::runtime_config::call_timeout_secs(context.agent_dir.as_deref()));
        let mcp = mcp_override_args_with_extra_env("tester", home.path(), true, &delegation_env);
        expected.args(mcp.args).envs(mcp.process_env);
        // The delegation proof embeds a wall-clock expiry; compare its presence, not its second.
        let delegation = duduclaw_core::identity_token::ENV_DELEGATION_CONTEXT;
        if let Some(value) = captured_env(delegation) { expected.env(delegation, value); }
        expected.args(turn_source_override_args_with_namespace_guard(&[],
            duduclaw_core::ns_guard::is_active()));
        // RM-204: the prompt is read from stdin (`-`), never argv.
        expected.args(["-", "--cd"]).arg(&canonical_agent).current_dir(&canonical_agent);
        expected.args(["-c", "shell_environment_policy.inherit=\"all\""]);
        let _proof = crate::run_identity::assert_command_identity(&command, false);
        for name in [duduclaw_core::run_credential::ENV_RUN_CREDENTIAL,
            duduclaw_core::run_credential::ENV_IDENTITY_URL,
            duduclaw_core::run_credential::ENV_IDENTITY_ENDPOINT,
            duduclaw_core::run_credential::ENV_IDENTITY_SERVER] {
            if let Some(value) = captured_env(name) { expected.env(name, value); }
            else { expected.env_remove(name); }
        }
        assert_eq!(before.cwd.as_deref(), Some(canonical_agent.as_path()));
        // RM-204: the instructions live in the owned CODEX_HOME, owner-only,
        // and nothing is written into the employee directory.
        assert!(!agent.join("AGENTS.md").exists());
        assert_eq!(input.codex_home.as_deref().map(std::path::Path::as_os_str), Some(codex_home.as_os_str()));
        assert_eq!(input.agents_md.as_deref(), Some("system </role_system_prompt>".as_bytes()));
        #[cfg(unix)]
        assert_eq!(input.agents_md_mode, Some(0o600));
        assert_eq!(input.stdin.as_deref(), Some("繁體中文 $HOME;\nnext".as_bytes()));
        // H1: `<home>/run/codex/call-<random>/codex-home`, removed after an error too.
        let call_dir = std::path::Path::new(&codex_home).parent().unwrap();
        assert_eq!(call_dir.parent(), Some(owned_home::run_parent(home.path()).as_path()));
        let name = call_dir.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with(owned_home::CALL_PREFIX) && owned_home::matches_scheme(name), "{name}");
        assert!(!std::path::Path::new(&codex_home).exists(), "owned home removed with the call");
        assert!(!call_dir.exists(), "call directory removed with the call");
        assert_eq!(before, duduclaw_core::execution_gate::command_snapshot(&expected));
        assert_eq!(duduclaw_core::execution_gate::local_command_roundtrip(command).await.unwrap(), before);
    }

    /// Run the real conversational Codex path up to the spawn and return the
    /// captured argv, stdin and owned-home instructions.
    pub(crate) async fn capture_conversation(context: &RuntimeContext, prompt: &str)
        -> (Vec<String>, InputCapture) {
        let bin = "/nonexistent/codex-capture";
        cached_codex_support(bin, || async { Some("codex-cli 0.164.0".into()) }).await;
        let runtime = CodexRuntime { codex_path: bin.into() };
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let input = std::sync::Arc::new(std::sync::Mutex::new(InputCapture::default()));
        let error = MCP_CONFIG_TEST_PATHS.scope(vec![], INPUT_CAPTURE.scope(input.clone(),
            COMMAND_CAPTURE.scope(captured.clone(), runtime.execute(prompt, context)))).await.unwrap_err();
        assert_eq!(error, "codex command captured before backend execution");
        let command = captured.lock().unwrap().take().unwrap();
        let args = command.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let input = input.lock().unwrap().clone();
        (args, input)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_work_root_uses_gate_canonical_path_in_argv_and_cwd() {
        let home = tempfile::tempdir().unwrap();
        let mut context = utility_context(home.path());
        context.capabilities = None;
        let workspace = tempfile::tempdir().unwrap();
        let alias = home.path().join("workspace-alias");
        std::os::unix::fs::symlink(workspace.path(), &alias).unwrap();
        let expected = owned_home::check_work_root(home.path(), &alias).unwrap();
        assert_ne!(expected.path(), alias);
        let bin = "/nonexistent/codex-canonical-work-root";
        cached_codex_support(bin, || async { Some("codex-cli 0.164.0".into()) }).await;
        let runtime = CodexRuntime { codex_path: bin.into() };
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let error = super::super::SPAWN_OVERRIDE.scope(
            super::super::SpawnOverride { work_dir: Some(alias.clone()) },
            MCP_CONFIG_TEST_PATHS.scope(vec![], COMMAND_CAPTURE.scope(
                captured.clone(), runtime.execute("hi", &context))),
        ).await.unwrap_err();
        assert_eq!(error, "codex command captured before backend execution");
        let command = captured.lock().unwrap().take().unwrap();
        let args: Vec<_> = command.as_std().get_args().collect();
        let cd: Vec<_> = args.windows(2).filter(|pair| pair[0] == "--cd").collect();
        assert_eq!(cd.len(), 1);
        assert_eq!(cd[0][1], expected.path().as_os_str());
        assert_eq!(command.as_std().get_current_dir(), Some(expected.path()));
        let permissions = employee_permission_args(context.agent_dir.as_deref().unwrap(),
            Some(expected.path()), &owned_home::protected_roots(home.path())).unwrap();
        assert!(permissions.iter().all(|arg| args.contains(&std::ffi::OsStr::new(arg))));
        assert!(!args.iter().any(|arg| arg.to_string_lossy().contains(alias.to_str().unwrap())));
    }

    #[tokio::test]
    async fn collector_refuses_a_work_root_replaced_after_command_preparation() {
        let home = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let workspace = base.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let checked = owned_home::check_work_root(home.path(), &workspace).unwrap();
        let mut command = employee_base_command("fixture-never-spawned", "");
        checked.apply_to_command(&mut command);
        std::fs::rename(&workspace, base.path().join("original")).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        let probe: CollectProbe = Box::new(|_| panic!("replaced root must not reach spawn"));
        let error = COLLECT_PROBE.scope(probe, collect_codex_output_with_input(
            &mut command, std::time::Duration::from_secs(5), None, Some(&checked),
        )).await.err().expect("replaced root must refuse");
        assert!(duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error(&error));
    }

    #[tokio::test]
    async fn large_system_prompt_reaches_the_owned_home_whole_and_prompt_rides_stdin() {
        let home = tempfile::tempdir().unwrap();
        let mut context = utility_context(home.path());
        context.capabilities = None;
        // > 64 KiB (the old cut) and > 32 KiB (Codex's project-doc cap), with
        // the summary / pinned blocks at the tail where truncation hit.
        let tail = "<pinned_instructions>永遠用繁體中文</pinned_instructions>\nTAIL-MARKER-🙂";
        context.system_prompt = format!("{}{tail}", "基本提示 ".repeat(12_000));
        assert!(context.system_prompt.len() > 128 * 1024);
        context.conversation_history = vec![
            crate::runtime::ConversationTurn { role: "user".into(), content: "第一輪 TURN1-USER".into() },
            crate::runtime::ConversationTurn { role: "assistant".into(), content: "TURN1-REPLY".into() },
        ];
        let message = format!("-starts-with-dash {}", "長訊息".repeat(50_000));
        let (args, input) = capture_conversation(&context, &message).await;
        assert!(args.windows(2).any(|pair| pair == ["-c", "project_doc_max_bytes=0"]), "{args:?}");
        assert!(args.iter().any(|a| a == "--ignore-user-config"));
        assert!(args.iter().any(|a| a == "-"), "prompt selected from stdin");
        assert!(args.iter().all(|a| !a.contains("TURN1-USER") && !a.contains("長訊息")), "no conversation bytes in argv");
        assert!(args.iter().all(|a| a.len() < 128 * 1024));
        let agents_md = String::from_utf8(input.agents_md.expect("owned AGENTS.md")).unwrap();
        assert_eq!(agents_md, context.system_prompt, "full prompt, byte-exact");
        assert!(agents_md.ends_with("TAIL-MARKER-🙂"));
        let stdin = String::from_utf8(input.stdin.expect("stdin payload")).unwrap();
        assert_eq!(stdin, crate::runtime::format_history_as_prompt(&context.conversation_history, &message));
        assert!(stdin.contains("TURN1-USER") && stdin.contains("TURN1-REPLY") && stdin.ends_with(&message));
        assert!(!fixture_agent_dir(home.path()).join("AGENTS.md").exists(), "nothing written to the employee directory");
    }

    #[tokio::test]
    async fn an_existing_employee_agents_md_is_left_alone() {
        let home = tempfile::tempdir().unwrap();
        let mut context = utility_context(home.path());
        context.capabilities = None;
        let agent = fixture_agent_dir(home.path());
        std::fs::write(agent.join("AGENTS.md"), "scaffold").unwrap();
        let (_args, input) = capture_conversation(&context, "hi").await;
        assert_eq!(std::fs::read_to_string(agent.join("AGENTS.md")).unwrap(), "scaffold");
        assert_eq!(input.agents_md.as_deref(), Some(context.system_prompt.as_bytes()));
    }

    /// F4 (v1.84 security re-review): an employee whose working root is the
    /// system data folder (or contains it) is refused with a typed
    /// engine-local refusal before any call directory is prepared.
    #[tokio::test]
    async fn employee_working_root_containing_the_system_folder_is_refused_before_preparation() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let mut context = utility_context(&home);
        context.capabilities = None;
        let bin = "/nonexistent/codex-work-root-refusal";
        cached_codex_support(bin, || async { Some("codex-cli 0.164.0".into()) }).await;
        let runtime = CodexRuntime { codex_path: bin.into() };
        // The home itself and an ancestor of it.
        for root in [home.clone(), base.path().to_path_buf()] {
            context.agent_dir = Some(root.clone());
            let probe: CollectProbe = Box::new(|_| panic!("a refused Codex call must not run"));
            let error = MCP_CONFIG_TEST_PATHS.scope(vec![],
                COLLECT_PROBE.scope(probe, runtime.execute("hi", &context))).await.unwrap_err();
            assert!(duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error(&error), "{root:?}: {error}");
            assert!(error.contains(owned_home::WORK_ROOT_CONTAINS_HOME), "{error}");
            assert!(!owned_home::run_parent(&home).exists(), "nothing prepared");
        }
        // The same employee in its own directory reaches the spawn.
        context.agent_dir = Some(fixture_agent_dir(&home));
        let (args, _input) = capture_conversation(&context, "hi").await;
        assert!(args.iter().any(|a| a == "--cd"));
    }

    #[test]
    fn project_trust_overrides_are_refused_in_argv_and_owned_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let mut cmd = employee_base_command("fixture-never-spawned", "gpt-5.4-mini");
        cmd.arg("-");
        assert!(refuse_project_trust_override(&cmd, None).is_ok());
        assert!(refuse_project_trust_override(&cmd, Some(&config)).is_ok(), "absent owned config");
        for bad in ["projects={}", "projects.\"/tmp/x\".trust_level=\"trusted\"", " projects = {}"] {
            let mut cmd = employee_base_command("fixture-never-spawned", "");
            cmd.args(["-c", bad]);
            let error = refuse_project_trust_override(&cmd, None).unwrap_err();
            assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error), "{bad}");
            let mut cmd = employee_base_command("fixture-never-spawned", "");
            cmd.arg(format!("--config={bad}"));
            assert!(refuse_project_trust_override(&cmd, None).is_err(), "{bad}");
        }
        // A key that merely starts with the letters is not the trust table.
        let mut cmd = employee_base_command("fixture-never-spawned", "");
        cmd.args(["-c", "projects_note=1"]);
        assert!(refuse_project_trust_override(&cmd, None).is_ok());
        std::fs::write(&config, "[projects.\"/tmp/x\"]\ntrust_level = \"trusted\"\n").unwrap();
        assert!(refuse_project_trust_override(&cmd, Some(&config)).is_err());
        std::fs::write(&config, "[broken").unwrap();
        assert!(refuse_project_trust_override(&cmd, Some(&config)).is_err(), "fail closed");
        std::fs::write(&config, "[mcp_servers.duduclaw]\ncommand = \"/x\"\n").unwrap();
        assert!(refuse_project_trust_override(&cmd, Some(&config)).is_ok());
    }

    #[test]
    fn employee_command_keeps_auth_model_and_places_managed_mcp_after_empty_base() {
        for model in ["", "gpt-5.4-mini"] {
            let mut cmd = employee_base_command("fixture-never-spawned", model);
            cmd.args(mcp_override_base_args(&serde_json::json!({"command": "/host/duduclaw"})));
            cmd.args(["-c", "mcp_servers.crm.command=\"/host/duduclaw\"",
                "-c", "mcp_servers.crm.args=[\"mcp-proxy\",\"--server\",\"crm\",\"--\",\"provider\"]"]);
            let args: Vec<_> = cmd.as_std().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            assert_eq!(&args[..6], &["exec", "--json", "--skip-git-repo-check",
                "--ignore-user-config", "-c", "mcp_servers={}"]);
            let base = args.iter().position(|a| a == "mcp_servers={}").unwrap();
            assert!(args.iter().enumerate().filter(|(_, a)| a.starts_with("mcp_servers."))
                .all(|(index, _)| index > base));
            assert_eq!(args.iter().filter(|a| *a == "-m").count(), usize::from(!model.is_empty()));
            if !model.is_empty() { assert!(args.windows(2).any(|pair| pair == ["-m", model])); }
            for key in ["CODEX_HOME", "OPENAI_API_KEY"] {
                let actual = cmd.as_std().get_envs().find(|(name, _)| *name == key)
                    .and_then(|(_, value)| value).map(std::ffi::OsStr::to_os_string);
                assert_eq!(actual, std::env::var_os(key));
            }
        }
    }

    #[tokio::test]
    async fn codex_external_mcp_refuses_employee_and_utility_before_preparation() {
        let home = tempfile::tempdir().unwrap();
        let layer = home.path().join("system-config.toml");
        std::fs::write(&layer, "[mcp_servers.native]\ncommand='unsafe'\n").unwrap();
        for utility in [false, true] {
            let runtime = CodexRuntime { codex_path: "/never-spawn-fixture".into() };
            let context = utility_context(home.path());
            let probe: CollectProbe = Box::new(|_| panic!("external MCP must not spawn"));
            let call = COLLECT_PROBE.scope(probe, runtime.execute("blocked", &context));
            let error = MCP_CONFIG_TEST_PATHS.scope(vec![layer.clone()], async {
                if utility { crate::runtime_dispatch::UTILITY_CALL.scope((), call).await }
                else { call.await }
            }).await.unwrap_err();
            assert!(error.contains("system-config.toml"), "{error}");
            assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error));
            // The layer file and the fixture's `agents/` directory: nothing prepared.
            assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 2);
            assert!(!home.path().join("run").exists());
        }
    }

    #[tokio::test]
    async fn absent_external_layers_allow_utility_collector_with_owned_home() {
        let home = tempfile::tempdir().unwrap();
        let runtime = CodexRuntime { codex_path: "/never-spawn-fixture".into() };
        let context = utility_context(home.path());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None::<std::path::PathBuf>));
        let seen_in_probe = seen.clone();
        let probe: CollectProbe = Box::new(move |cmd| {
            let owned = cmd.as_std().get_envs().find(|(key, _)| *key == "CODEX_HOME")
                .and_then(|(_, value)| value).unwrap();
            assert!(std::path::Path::new(owned).is_dir());
            assert!(!std::path::Path::new(owned).join("config.toml").exists());
            *seen_in_probe.lock().unwrap() = Some(std::path::PathBuf::from(owned));
            let mut output = collected_fixture("{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"PASS\"}}\n");
            output.status = Some(std::process::ExitStatus::default());
            Ok(output)
        });
        let response = MCP_CONFIG_TEST_PATHS.scope(vec![],
            COLLECT_PROBE.scope(probe, crate::runtime_dispatch::UTILITY_CALL.scope((),
                runtime.execute("fixture", &context)))).await.unwrap();
        assert_eq!(response.content, "PASS");
        // H1: under `<home>/run/codex`, and removed after a successful call.
        let owned = seen.lock().unwrap().clone().expect("probe saw the owned home");
        let call_dir = owned.parent().unwrap();
        assert_eq!(call_dir.parent(), Some(owned_home::run_parent(home.path()).as_path()));
        assert!(!call_dir.exists(), "utility directory removed after success");
    }

    /// H1: the same for an employee call that succeeds, and for one that is
    /// cancelled while Codex would be running.
    #[tokio::test]
    async fn employee_call_directory_is_removed_after_success_and_after_cancellation() {
        let home = tempfile::tempdir().unwrap();
        let mut context = utility_context(home.path());
        context.capabilities = None;
        let bin = "/nonexistent/codex-owned-home-lifetime";
        cached_codex_support(bin, || async { Some("codex-cli 0.164.0".into()) }).await;
        let runtime = CodexRuntime { codex_path: bin.into() };
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None::<std::path::PathBuf>));
        let seen_in_probe = seen.clone();
        let probe: CollectProbe = Box::new(move |cmd| {
            let owned = cmd.as_std().get_envs().find(|(key, _)| *key == "CODEX_HOME")
                .and_then(|(_, value)| value).unwrap();
            *seen_in_probe.lock().unwrap() = Some(std::path::PathBuf::from(owned));
            let mut output = collected_fixture("{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"PASS\"}}\n");
            output.status = Some(std::process::ExitStatus::default());
            Ok(output)
        });
        let response = MCP_CONFIG_TEST_PATHS.scope(vec![],
            COLLECT_PROBE.scope(probe, runtime.execute("fixture", &context))).await.unwrap();
        assert_eq!(response.content, "PASS");
        let owned = seen.lock().unwrap().take().expect("probe saw the owned home");
        assert_eq!(owned.parent().unwrap().parent(), Some(owned_home::run_parent(home.path()).as_path()));
        assert!(!owned.parent().unwrap().exists(), "call directory removed after success");
        // Cancellation: the call future is dropped while it holds the guard.
        let parent = owned_home::run_parent(home.path());
        let listed = |dir: &std::path::Path| std::fs::read_dir(dir).unwrap()
            .filter_map(|e| e.ok()).map(|e| e.path()).collect::<Vec<_>>();
        let (sender, receiver) = tokio::sync::oneshot::channel::<std::path::PathBuf>();
        let task = tokio::spawn({
            let home = home.path().to_path_buf();
            async move {
                let dir = owned_home::OwnedCallDir::create(&home, owned_home::CALL_PREFIX).unwrap();
                prepare_owned_codex_home_with_auth(dir.path(), None).unwrap();
                sender.send(dir.path().to_path_buf()).unwrap();
                std::future::pending::<()>().await;
                drop(dir);
            }
        });
        let path = receiver.await.unwrap();
        assert_eq!(listed(&parent), vec![path.clone()]);
        task.abort();
        let _ = task.await;
        assert!(listed(&parent).is_empty(), "cancelled call left nothing behind");
    }

    /// H1 item 1: with the gateway's own data directory outside the temp
    /// directory (the installed layout), the owned-home parent is outside
    /// every write root the employee profile grants, and item 2: the profile
    /// explicitly marks it and the host credential store read-only, which
    /// keeps it unwritable even when an overridden working root contains it.
    #[cfg(unix)]
    #[test]
    fn owned_home_parent_is_outside_every_profile_write_root_and_marked_read_only() {
        let home = std::path::Path::new("/Users/operator/.duduclaw");
        let agent = home.join("agents/worker");
        let host_store = std::path::PathBuf::from("/Users/operator/.codex");
        let parent = owned_home::run_parent(home);
        let private = vec![parent.clone(), host_store.clone()];
        for work_root in [agent.clone(), home.to_path_buf(), std::path::PathBuf::from("/Users/operator")] {
            let args = employee_permission_args(&agent, Some(work_root.as_path()), &private).unwrap();
            let (_, value) = args.last().unwrap().split_once('=').unwrap();
            let parsed: toml::Value = format!("value={value}").parse().unwrap();
            let fs = parsed["value"].as_table().unwrap();
            for root in &private {
                assert_eq!(fs[root.to_str().unwrap()].as_str(), Some("read"), "{root:?}");
            }
            // Every write grant of the profile, resolved.
            let mut write_roots = Vec::new();
            for (key, access) in fs {
                match (key.as_str(), access) {
                    (":tmpdir", a) if a.as_str() == Some("write") => {
                        write_roots.push(std::env::temp_dir());
                        if let Ok(c) = std::fs::canonicalize(std::env::temp_dir()) { write_roots.push(c); }
                    }
                    (":project_roots", toml::Value::Table(t)) => {
                        assert_eq!(t.len(), 1);
                        assert_eq!(t["."].as_str(), Some("write"));
                        // `.` is the spawn's working root. An overridden root that
                        // contains the parent is covered by the explicit "read"
                        // entry asserted above (the more specific path wins, as
                        // for the control files).
                        if work_root == agent { write_roots.push(work_root.clone()); }
                    }
                    (path, a) if a.as_str() == Some("write") => write_roots.push(path.into()),
                    (_, a) => assert_eq!(a.as_str(), Some("read"), "{key}"),
                }
            }
            assert!(write_roots.contains(&std::path::PathBuf::from("/tmp")));
            for root in &write_roots {
                assert!(!parent.starts_with(root), "{parent:?} under write root {root:?}");
            }
        }
        // Not absolute / not UTF-8 private roots fail closed.
        assert!(employee_permission_args(&agent, None, &[std::path::PathBuf::from("run/codex")]).is_err());
        use std::os::unix::ffi::OsStrExt;
        let invalid = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/run/\xff"));
        assert!(employee_permission_args(&agent, None, &[invalid]).is_err());
    }

    /// H1 items 2–3 on the real employee argv: the protected roots of this
    /// home are in the profile and tools do not inherit `CODEX_HOME`.
    #[cfg(unix)]
    #[tokio::test]
    async fn employee_argv_protects_private_roots_and_hides_codex_home_from_tools() {
        let home = tempfile::tempdir().unwrap();
        let mut context = utility_context(home.path());
        context.capabilities = None;
        let (args, _input) = capture_conversation(&context, "hi").await;
        assert!(args.windows(2).any(|pair| pair == ["-c", "shell_environment_policy.exclude=[\"CODEX_HOME\"]"]), "{args:?}");
        let profile = args.iter().find(|a| a.starts_with("permissions.duduclaw-employee.filesystem=")).unwrap();
        let (_, value) = profile.split_once('=').unwrap();
        let parsed: toml::Value = format!("value={value}").parse().unwrap();
        let fs = parsed["value"].as_table().unwrap();
        let parent = owned_home::run_parent(home.path());
        assert_eq!(fs[parent.to_str().unwrap()].as_str(), Some("read"));
        // F4 (b): `<home>/run` itself, literal and canonical.
        let run = home.path().join("run");
        assert_eq!(fs[run.to_str().unwrap()].as_str(), Some("read"), "{fs:?}");
        let canonical = std::fs::canonicalize(&run).unwrap();
        assert_eq!(fs[canonical.to_str().unwrap()].as_str(), Some("read"), "{fs:?}");
        if let Some(host) = owned_home::host_codex_home().filter(|h| h.is_absolute()) {
            assert_eq!(fs[host.to_str().unwrap()].as_str(), Some("read"));
        }
    }

    #[test]
    fn owned_codex_home_shares_auth_refresh_without_native_configuration() {
        let host = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(host.path().join("auth.json"), "fixture-token-before").unwrap();
        std::fs::write(host.path().join("config.toml"), "[mcp_servers.native]\ncommand='unsafe'").unwrap();
        let owned = prepare_owned_codex_home_with_auth(root.path(), Some(host.path())).unwrap();
        assert!(!owned.join("config.toml").exists());
        // Same truncate-write primitive as Codex FileAuthStorage::save.
        std::fs::write(owned.join("auth.json"), "fixture-token-after").unwrap();
        assert_eq!(std::fs::read_to_string(host.path().join("auth.json")).unwrap(), "fixture-token-after");
        std::fs::remove_dir_all(root.path().join("codex-home")).unwrap();
        assert!(host.path().join("auth.json").exists());
    }

    #[tokio::test]
    async fn hinted_team_verifier_and_judge_refuse_codex() {
        use crate::runtime_dispatch::{UtilityModelHint, run_utility_prompt_with_hint};
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents/tester");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[agent]\nname='tester'\nstatus='active'\n[runtime]\nprovider='codex'\n[model]\nutility='gpt-5.4-mini'\n").unwrap();
        // No host CLI detection or credential-file reads: use the real Codex
        // runtime, but intercept its collector before any subprocess is spawned.
        let registry = super::super::RuntimeRegistry::with_runtimes(std::collections::HashMap::from([
            (duduclaw_core::types::RuntimeType::Codex,
                Box::new(CodexRuntime { codex_path: "/nonexistent/vendor-cli".into() }) as Box<dyn AgentRuntime>),
        ]));
        let _guard = crate::runtime_dispatch::testing::RegistryGuard::install(home.path(), registry).await;
        for owner in [Some(agent.as_path()), None] {
            let probe: CollectProbe = Box::new(|_| panic!("hinted Codex must not spawn"));
            let hint = UtilityModelHint { provider: Some(duduclaw_core::types::RuntimeType::Codex),
                model: Some("gpt-5.4-mini".into()), ..Default::default() };
            // Team verifiers and judges run inside the goal round's autonomous scope.
            let layer = home.path().join("managed_config.toml");
            std::fs::write(&layer, "[mcp_servers.native]\ncommand='unsafe'\n").unwrap();
            let result = MCP_CONFIG_TEST_PATHS.scope(vec![layer], COLLECT_PROBE.scope(probe, crate::autonomous_runtime::scope(
                home.path(), "tester", &[],
                run_utility_prompt_with_hint(
                    home.path(), owner, "tester", "system", "verifier/judge material", 64, Some(&hint),
                ),
            ))).await.unwrap_err();
            assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&result), "{result}");
            assert!(result.contains("managed_config.toml"), "{result}");
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
    fn timeout_uses_only_last_assistant_segment_from_truncated_jsonl() {
        let stdout = concat!(
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"查詢完成\"}}\n",
            "{\"type\":\"item.started\",\"item\":{\"type\":\"agent_message\",\"text\":\"未完成\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"已整理\"},{\"type\":\"output_text\",\"text\":\"細節\"}]}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"  \"}}\n",
            "{\"type\":\"item.completed\",\"item\":{"
        );
        assert_eq!(completed_codex_messages(stdout), vec!["查詢完成", "細節"]);
        assert_eq!(collected_fixture(stdout).timeout_content(300).unwrap(),
            "  ");
        // Normal parsing still returns only the final completed message (even blank).
        assert_eq!(parse_codex_stdout(stdout).0, "  ");
    }

    #[test]
    fn reasoning_and_commentary_are_not_final_answers() {
        let stdout = concat!(
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"reasoning\",\"text\":\"private\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"phase\":\"commentary\",\"text\":\"Let me check\"}}\n",
            "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}\n"
        );
        assert_eq!(parse_codex_stdout(stdout).0, "");
        assert_eq!(collected_fixture(stdout).content, "");
        assert!(completed_codex_messages(stdout).is_empty());
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
    fn employee_namespace_guard_only_when_bubblewrap_will_not_run() {
        let full = caps(true, false, &[], &[]);
        let read_only = caps(false, false, &["Read"], &[]);
        assert_eq!(sandbox_level_for(Some(&full)), duduclaw_core::types::SandboxLevel::FullAccess);
        for caps in [None, Some(&read_only), Some(&full)] {
            assert!(!employee_needs_namespace_guard(caps, true));
            assert_eq!(employee_needs_namespace_guard(caps, false),
                sandbox_level_for(caps) == duduclaw_core::types::SandboxLevel::FullAccess);
        }
        use duduclaw_core::egress_guard::Mode;
        for mode in [Mode::Off, Mode::Shadow, Mode::Ask, Mode::Block] {
            assert_eq!(employee_needs_namespace_guard(egress_sandbox_caps(Some(&full), mode), false),
                mode < Mode::Ask);
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
                let private = [std::path::PathBuf::from("/home/run/codex")];
                let args = employee_egress_sandbox_args(caps, false, mode, Some(agent), Some(root), true, &private).unwrap();
                let mut expected = sandbox_args(None);
                expected.extend(employee_permission_args(agent, Some(root), &private).unwrap());
                expected.extend(egress_network_args(mode));
                assert_eq!(args, expected);
                assert!(!args.iter().any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox" || arg == "-s"));
                assert!(employee_egress_sandbox_args(caps, false, mode, Some(agent), Some(root), false, &[]).unwrap_err().contains("0.164.0"));
                assert!(employee_egress_sandbox_args(caps, false, mode, None, Some(root), true, &[]).is_err());
            }
            assert_eq!(employee_egress_sandbox_args(Some(&read_only), false, mode, None, None, false, &[]).unwrap(), egress_sandbox_args(Some(&read_only), false, mode));
            assert_eq!(employee_egress_sandbox_args(Some(&full), true, mode, None, None, false, &[]).unwrap(), egress_sandbox_args(Some(&full), true, mode));
        }
        for mode in [Mode::Off, Mode::Shadow] {
            assert_eq!(employee_egress_sandbox_args(Some(&full), false, mode, None, None, false, &[]).unwrap(), sandbox_args(Some(&full)));
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

    /// RM-103 S3 (Daybreak #2): the sandbox Codex spawns with is chosen from
    /// the member's effective config through `coarse_native_caps`, exactly as
    /// `execute` does.
    fn codex_spawn_sandbox(c: &CapabilitiesConfig) -> Result<Vec<String>, String> {
        let view = super::super::coarse_native_caps(Some(c), "codex", "eph-test", true)?;
        Ok(sandbox_args(view.as_ref().or(Some(c))))
    }

    /// An inherited `modify = ask` (or an approval list naming a write tool)
    /// used to spawn an executor `workspace-write` because its allowlist holds
    /// `Write`, so `apply_patch` skipped the approval. It is now `-s read-only`.
    #[test]
    fn inherited_modify_ask_forces_read_only() {
        let mut executor = caps(false, false, &["team_handoff", "Read", "Write", "Edit", "Grep"], &[]);
        assert_eq!(declared_sandbox(&sandbox_args(Some(&executor))).as_deref(), Some("workspace-write"));
        executor.action_rules =
            duduclaw_core::ActionRules::from_value(serde_json::json!([{ "effect": "modify", "verdict": "ask" }]));
        assert_eq!(
            codex_spawn_sandbox(&executor).unwrap(),
            vec!["-s", "read-only", "-c", "approval_policy=never"]
        );
        let mut approval = caps(false, false, &[], &[]);
        approval.approval_required_tools = vec!["Edit".into()];
        assert_eq!(declared_sandbox(&codex_spawn_sandbox(&approval).unwrap()).as_deref(), Some("read-only"));
        // A full-access grant is narrowed too (most restrictive wins).
        let mut full = caps(true, false, &[], &[]);
        full.irreversible_tools = vec!["Write".into()];
        assert_eq!(declared_sandbox(&codex_spawn_sandbox(&full).unwrap()).as_deref(), Some("read-only"));
    }

    /// `Bash(git:*)` (a packet narrowing) used to collapse into
    /// `workspace-write` with Codex's arbitrary native shell. Codex has no
    /// mode without a shell, so the spawn is refused with a spawn-gate error.
    #[test]
    fn qualified_bash_or_shell_approval_refuses_codex() {
        let git_only = caps(false, false, &["team_handoff", "Read", "Write", "Bash(git:*)"], &[]);
        let mut admin_ask = caps(false, false, &[], &[]);
        admin_ask.action_rules =
            duduclaw_core::ActionRules::from_value(serde_json::json!([{ "effect": "admin", "verdict": "ask" }]));
        let mut policy = caps(false, false, &[], &[]);
        policy.policy = vec![duduclaw_core::types::ToolPolicy {
            tool: "shell_exec".into(),
            effect: duduclaw_core::types::PolicyEffect::Forbid,
            when: vec![],
        }];
        let packet_denied = caps(false, false, &["Read", "Write", "Bash"], &["Bash"]);
        let qualified_deny = caps(false, false, &[], &["Bash(rm:*)"]);
        for c in [git_only, admin_ask, policy, packet_denied, qualified_deny] {
            let error = codex_spawn_sandbox(&c).unwrap_err();
            assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error), "{error}");
            assert!(error.contains("codex"), "{error}");
        }
    }

    /// Daybreak round 2 #2/#3/#4 at the Codex spawn: a scoped native tool or a
    /// policy that reaches the native tools restricts Codex; a policy whose
    /// rules only name MCP tools does not (it used to refuse every spawn).
    #[test]
    fn scoped_tools_and_native_policy_rules_restrict_codex_mcp_only_policy_does_not() {
        use duduclaw_core::types::{PolicyEffect, ToolPolicy};
        let rule = |tool: &str, effect: PolicyEffect| ToolPolicy { tool: tool.into(), effect, when: vec![] };
        let executor = || caps(false, false, &["team_handoff", "Read", "Write", "Edit", "Bash"], &[]);

        // MCP-only policies: the executor keeps its workspace-write argv.
        for policy in [
            vec![rule("*", PolicyEffect::Allow), rule("mail_send", PolicyEffect::Forbid)],
            vec![rule("memory_search", PolicyEffect::Allow)],
        ] {
            let mut c = executor();
            c.policy = policy;
            assert_eq!(codex_spawn_sandbox(&c).unwrap(), sandbox_args(Some(&c)), "{:?}", c.policy);
            assert_eq!(
                declared_sandbox(&codex_spawn_sandbox(&c).unwrap()).as_deref(),
                Some("workspace-write"),
                "{:?}",
                c.policy
            );
        }

        // Shell restricted ⇒ refused: scoped `Bash`, a `" * "` allow (not a
        // kernel wildcard), a shell forbid.
        let mut scoped_bash = executor();
        scoped_bash.scoped_tools = vec!["Bash".into()];
        let mut spaced = executor();
        spaced.policy = vec![rule(" * ", PolicyEffect::Allow)];
        let mut shell_forbid = executor();
        shell_forbid.policy = vec![rule("*", PolicyEffect::Allow), rule("shell_exec", PolicyEffect::Forbid)];
        for c in [scoped_bash, spaced, shell_forbid] {
            let error = codex_spawn_sandbox(&c).unwrap_err();
            assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error), "{error}");
        }

        // Writes restricted only ⇒ `-s read-only`.
        let mut scoped_write = executor();
        scoped_write.scoped_tools = vec!["Write".into()];
        let mut write_ask = executor();
        write_ask.policy = vec![rule("*", PolicyEffect::Allow), rule("fs_write", PolicyEffect::Ask)];
        for c in [scoped_write, write_ask] {
            assert_eq!(declared_sandbox(&codex_spawn_sandbox(&c).unwrap()).as_deref(), Some("read-only"));
        }
    }

    /// Unconstrained configs keep exactly the argv they had.
    #[test]
    fn unconstrained_caps_keep_their_codex_sandbox() {
        for c in [
            caps(false, false, &[], &[]),
            caps(false, false, &["Read", "Grep"], &[]),
            caps(true, false, &[], &[]),
            caps(false, false, &["Read", "Write", "Bash", "Bash(git:*)"], &[]),
            // All write tools denied: the documented read-only mapping.
            caps(false, false, &[], &["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit"]),
        ] {
            assert!(super::super::coarse_native_caps(Some(&c), "codex", "a", true).unwrap().is_none(), "{c:?}");
            assert_eq!(codex_spawn_sandbox(&c).unwrap(), sandbox_args(Some(&c)));
        }
        assert!(super::super::coarse_native_caps(None, "codex", "a", true).unwrap().is_none());
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
        let home = tempfile::tempdir().unwrap();
        let call = owned_home::OwnedCallDir::create(home.path(), owned_home::CALL_PREFIX).unwrap();
        let args = output_schema_args(None, call.path());
        assert!(
            args.is_empty(),
            "argv must be byte-identical without a schema"
        );
        assert!(!call.path().join(OUTPUT_SCHEMA_FILE).exists());
    }

    #[test]
    fn an_output_schema_is_staged_to_a_file_the_flag_points_at() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "decision": { "type": "string", "enum": ["continue"] } },
            "required": ["decision"],
        });
        let home = tempfile::tempdir().unwrap();
        let call = owned_home::OwnedCallDir::create(home.path(), owned_home::CALL_PREFIX).unwrap();
        let args = output_schema_args(Some(&schema), call.path());
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "--output-schema");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
        assert_eq!(written, schema, "the file must carry the schema verbatim");
    }

    /// F1 (v1.84 security re-review): the schema is created inside the
    /// call's private directory — never in the shared system temp directory
    /// an employee's tools may write — owner-only, and removed with the call
    /// directory.
    #[test]
    fn output_schema_file_lives_in_the_call_directory_not_the_temp_dir() {
        let home = tempfile::tempdir().unwrap();
        let call = owned_home::OwnedCallDir::create(home.path(), owned_home::UTILITY_PREFIX).unwrap();
        let args = output_schema_args(Some(&serde_json::json!({"type": "object"})), call.path());
        let path = std::path::PathBuf::from(&args[1]);
        assert_eq!(path.parent(), Some(call.path()), "{path:?}");
        assert!(path.starts_with(owned_home::run_parent(home.path())));
        let temp = std::env::temp_dir();
        let temp_canonical = std::fs::canonicalize(&temp).unwrap_or(temp.clone());
        // The fixture home itself is a temp directory, so "not under the temp
        // dir" means: not a direct child of it, the old `duduclaw-codex-schema-*` spot.
        for temp in [&temp, &temp_canonical] {
            assert_ne!(path.parent(), Some(temp.as_path()), "{path:?}");
            assert_ne!(std::fs::canonicalize(&path).unwrap().parent(), Some(temp.as_path()), "{path:?}");
        }
        assert!(!path.file_name().unwrap().to_str().unwrap().starts_with("duduclaw-codex-schema-"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::symlink_metadata(&path).unwrap();
            assert!(meta.file_type().is_file());
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
        // An entry already there (a planted file or link) is never reused.
        let again = output_schema_args(Some(&serde_json::json!({"type": "string"})), call.path());
        assert!(again.is_empty(), "create_new refuses an existing entry");
        let written: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written, serde_json::json!({"type": "object"}));
        drop(call);
        assert!(!path.exists(), "removed with the call directory");
    }

    /// The flag has to coexist with the rest of the 0.156.1 argv — it is added
    /// alongside `--approve-for-me` / `-c sandbox_mode=…`, never in place of
    /// the `--sandbox` shape that CLI rejects.
    #[test]
    fn output_schema_composes_with_the_sandbox_and_effort_flags() {
        let schema = serde_json::json!({"type": "object"});
        let home = tempfile::tempdir().unwrap();
        let call = owned_home::OwnedCallDir::create(home.path(), owned_home::CALL_PREFIX).unwrap();
        let schema_args = output_schema_args(Some(&schema), call.path());
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

    #[test]
    fn marker_only_container_reaches_builtin_mcp_after_env_scrub() {
        // Inject the host decision rather than mutating the process environment
        // or relying on /.dockerenv on the test machine.
        let source_pairs = vec![("DUDUCLAW_TURN_ID".into(), "t-marker".into())];
        let marker = "mcp_servers.duduclaw.env.DUDUCLAW_IN_CONTAINER=\"1\"";
        let args = turn_source_override_args_with_namespace_guard(&source_pairs, true);
        assert!(args.iter().any(|arg| arg == marker), "{args:?}");
        assert!(args.iter().any(|arg| arg == "mcp_servers.duduclaw.env.DUDUCLAW_TURN_ID=\"t-marker\""));
        assert!(!turn_source_override_args_with_namespace_guard(&source_pairs, false)
            .iter().any(|arg| arg.contains("DUDUCLAW_IN_CONTAINER")));
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
            "DUDUCLAW_RUN_CREDENTIAL",
            "DUDUCLAW_IDENTITY_ENDPOINT",
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
            "DUDUCLAW_DELEGATION_CONTEXT",
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
        let args = employee_sandbox_args(None, false, Some(dir), Some(dir), true, &[]).unwrap();
        let mut expected = sandbox_args(None);
        expected.extend(employee_permission_args(dir, Some(dir), &[]).unwrap());
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
    fn employee_permissions_protect_missing_identity_and_user_files() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("employee");
        std::fs::create_dir(&dir).unwrap();
        for path in ["IDENTITY.md", "USER.md"] { assert!(!dir.join(path).exists()); }
        let args = employee_permission_args(&dir, None, &[]).unwrap();
        let (_, value) = args.last().unwrap().split_once('=').unwrap();
        let parsed: toml::Value = format!("value={value}").parse().unwrap();
        for path in ["IDENTITY.md", "USER.md"] {
            assert_eq!(parsed["value"][dir.join(path).to_str().unwrap()].as_str(), Some("read"));
            assert!(!dir.join(path).exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn employee_permissions_protect_the_member_and_parent_work_root() {
        let agent = std::path::Path::new("/agents/member");
        let root = std::path::Path::new("/agents/parent");
        let args = employee_sandbox_args(None, false, Some(agent), Some(root), true, &[]).unwrap();
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
        assert!(employee_sandbox_args(None, false, Some(agent), Some(std::path::Path::new("relative/root")), true, &[]).is_err());
        use std::os::unix::ffi::OsStrExt;
        let invalid = std::path::Path::new(std::ffi::OsStr::from_bytes(b"/agents/\xff"));
        assert!(employee_permission_args(agent, Some(invalid), &[]).is_err());
        assert!(employee_permission_args(invalid, Some(root), &[]).is_err());
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
            assert_eq!(employee_sandbox_args(caps, false, None, None, false, &[]).unwrap(), sandbox_args(caps));
        }
        for caps in [None, Some(&read_only), Some(&full_access)] {
            assert_eq!(employee_sandbox_args(caps, true, None, None, false, &[]).unwrap(), sandbox_args_for(caps, true));
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
            let result = employee_sandbox_args(None, false, Some(std::path::Path::new("/private/agent")), None, supported, &[]);
            assert_eq!(result.is_ok(), accepted);
            if let Err(error) = result {
                assert!(error.contains("Codex CLI 0.164.0"));
                assert!(!error.contains("/private/agent"));
            }
        }
        let error = employee_sandbox_args(None, false, None, None, true, &[]).unwrap_err();
        assert!(error.contains("no agent directory"));
        assert!(error.contains("refusing to spawn"));
        assert!(employee_permission_args(std::path::Path::new("relative/agent"), None, &[]).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn employee_permissions_share_version_probe_and_refuse_unprobeable_cli() {
        let bin = "/nonexistent/codex-employee-permissions-cache-test";
        assert!(cached_env_vars_support(bin, || async { Some("codex-cli 0.164.0".into()) }).await);
        assert!(cached_codex_support(bin, || async { panic!("version probe repeated") }).await.employee_permissions);
        assert!(!codex_supports_employee_permissions("/nonexistent/codex-employee-permissions-failed-test").await);
        let error = employee_sandbox_args(None, false, Some(std::path::Path::new("/agent")), None, false, &[]).unwrap_err();
        assert!(error.contains("Codex CLI 0.164.0"));
        let error = employee_sandbox_args(None, false, None, None, true, &[]).unwrap_err();
        assert!(error.contains("no agent directory"));
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
