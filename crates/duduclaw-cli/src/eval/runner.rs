//! Case execution — live agent runs and recorded-transcript replay.
//!
//! **Live** mirrors the gateway's harness invocation
//! (`channel_reply::build_claude_cli_args`, `pub(crate)` there): spawn the
//! `claude` CLI inside the agent directory with `--output-format stream-json`,
//! honouring the agent's `[capabilities]` allow/deny tool lists and per-agent
//! `.mcp.json`. The run uses the ambient `claude` credentials of whoever runs
//! `duduclaw eval` (no account rotation — evals are an operator/CI tool).
//!
//! **Replay** re-parses a previously recorded `*.transcript.jsonl` so the
//! deterministic assertion layer works offline in CI with zero credentials —
//! that is the regression half of the suite (`--record` refreshes baselines).
//!
//! ## P2: one runner, every runtime
//!
//! Team-as-Agent P2 needs the same case run on a *different vendor's* model, so
//! `--runtime`/`--model` select the backend. Two paths, on purpose:
//!
//! * **`claude` (the default, and what every pre-P2 run took)** — spawn the
//!   `claude` CLI directly as before. `--output-format stream-json` is the only
//!   transcript format the assertion layer natively speaks, and this path is
//!   the one whose recorded baselines already exist, so it stays byte-identical.
//! * **every other runtime** — go through the gateway's runtime abstraction
//!   (`runtime_dispatch::run_agent_prompt`), collecting the runtime's own
//!   [`NativeToolEvent`]s through the existing `NATIVE_TOOL_COLLECTOR`
//!   task-local (the same mechanism `team_composer` uses), then **synthesize** a
//!   stream-json transcript from `(final text, tool events)` so
//!   `must_use_tools` / `[[expect.grounded]]` / `max_tool_calls` keep working
//!   through the one existing parser instead of a second transcript builder.
//!   The synthesized file is self-labelled (see [`SYNTHETIC_MARKER_SUBTYPE`]) so
//!   nobody mistakes it for a real CLI recording.
//!
//! **Fidelity caveat, stated once here and in `docs/guides/evals.md`.** A
//! synthesized transcript carries exactly what the runtime's event stream
//! carried: one text block (so `min_text_blocks` can only ever observe `1`), no
//! thinking blocks, and tool inputs as the masked/capped `input_text` string the
//! collector recorded rather than the original JSON. Assertions that depend on
//! those signals are not comparable across the two paths, which is why a matrix
//! cell never mixes them.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use duduclaw_core::types::RuntimeType;
use duduclaw_gateway::runtime::NativeToolEvent;

use super::case::EvalCaseFile;
use super::transcript::{EvalTranscript, parse_stream_json};

/// How to obtain the transcript for a case.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RunMode {
    /// Spawn the agent live; `record: true` writes the raw stream-json next
    /// to the case file for future `--replay` runs.
    Live { record: bool },
    /// Parse the recorded transcript instead of running the agent.
    Replay,
}

/// `system` event subtype stamped as the first line of a synthesized
/// (non-Claude) transcript. `parse_stream_json` ignores `system` events, so the
/// marker costs nothing at parse time and makes the provenance of a recorded
/// file obvious to a human and greppable to a script.
pub const SYNTHETIC_MARKER_SUBTYPE: &str = "duduclaw_eval_synthetic";

/// Output-token ceiling for a non-Claude live run. The Claude CLI path has no
/// equivalent knob (it is bounded by `--max-turns`), so this only shapes the
/// runtime-abstraction path.
const RUNTIME_PATH_MAX_TOKENS: u32 = 8192;

/// No runtime in this build accepts a sampling seed through the `AgentRuntime`
/// abstraction (no CLI exposes one, and `duduclaw-llm`'s `ChatRequest` has no
/// seed field either — verified by grep, 2026-09-25). `--paired-seeds` therefore
/// *derives and records* a per-`(case, repeat)` seed so cells line up and a
/// future runtime that grows the knob needs no report-schema change; it does not
/// claim to have pinned sampling. [`RunFacts::seed_applied`] says so per run.
pub const SEED_APPLIED_ANYWHERE: bool = false;

/// Which `(runtime, model)` a live run must use, plus its paired seed.
///
/// `Default` (every field `None`) is the pre-P2 behavior exactly: the Claude CLI
/// path, the case's own `[case] model`, no seed recorded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunOverrides {
    /// `None` ⇒ [`RuntimeType::Claude`] (the default), taking the direct
    /// `claude` CLI path.
    pub runtime: Option<RuntimeType>,
    /// `None` ⇒ the case's `[case] model`.
    pub model: Option<String>,
    /// `--agent`: run every case under THIS provisioned agent instead of the
    /// case's own `[case] agent`. `None` ⇒ the case's own.
    ///
    /// A matrix measures the **model**, not the persona, so borrowing one
    /// provisioned agent to carry a suite authored for another is an acceptable
    /// probe-run compromise — but it changes the system prompt every case runs
    /// under, so it is never inferred and always declared (`agent_override` in
    /// the report header, `agent` per run).
    pub agent: Option<String>,
    /// Deterministic per-`(case_id, repeat)` seed, recorded not applied — see
    /// [`SEED_APPLIED_ANYWHERE`].
    pub seed: Option<u64>,
}

impl RunOverrides {
    /// The runtime this run uses: the CLI override, else the case's own
    /// `[case] runtime`, else Claude.
    ///
    /// Symmetric with [`Self::effective_model`] — review finding 10: it used to
    /// take no `case` parameter at all, so a case that pinned
    /// `runtime = "codex"` (validated at load time, and the only sanctioned way
    /// to record a non-Claude baseline, since CLI overrides are refused with
    /// `--record`) was executed on the Claude CLI with the case's codex model
    /// passed as `--model`. The field was checked and then consumed by nobody.
    pub fn effective_runtime(&self, case: &EvalCaseFile) -> RuntimeType {
        self.runtime
            .or_else(|| case.runtime())
            .unwrap_or(RuntimeType::Claude)
    }

    /// The model this run uses: the override, else the case's own.
    pub fn effective_model<'a>(&'a self, case: &'a EvalCaseFile) -> &'a str {
        self.model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| case.model())
    }

    /// The agent directory this run uses: the `--agent` override, else the
    /// case's own `[case] agent`.
    pub fn effective_agent<'a>(&'a self, case: &'a EvalCaseFile) -> &'a str {
        self.agent
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .unwrap_or(case.case.agent.as_str())
    }

    /// True when this run takes the pre-P2 direct `claude` CLI path.
    pub fn is_claude_cli_path(&self, case: &EvalCaseFile) -> bool {
        self.effective_runtime(case) == RuntimeType::Claude
    }
}

/// Token usage a runtime reported for one run. `Some(0, 0, 0)` is never
/// constructed — a runtime that reports nothing yields `None` on
/// [`RunFacts::usage`], so "free" and "unmeasured" stay distinguishable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReportedUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
}

impl ReportedUsage {
    fn from_response(r: &duduclaw_gateway::runtime::RuntimeResponse) -> Option<Self> {
        if r.input_tokens == 0 && r.output_tokens == 0 && r.cache_read_tokens == 0 {
            return None;
        }
        Some(ReportedUsage {
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            cache_read_tokens: r.cache_read_tokens,
        })
    }
}

/// What a run reports about itself beyond the transcript — the columns a matrix
/// cell must carry so a substituted or unmeasured run can never be silently
/// folded into a model's score.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunFacts {
    /// Runtime id this run asked for.
    pub runtime: String,
    /// Model id this run asked for.
    pub model: String,
    /// Agent directory this run actually used (the `--agent` override when one
    /// was given, else the case's own `[case] agent`).
    pub agent: String,
    pub seed: Option<u64>,
    /// Always [`SEED_APPLIED_ANYWHERE`] today.
    pub seed_applied: bool,
    pub usage: Option<ReportedUsage>,
    /// `(runtime, model)` that ACTUALLY answered, when the gateway's failover
    /// substituted something else.
    ///
    /// `None` means "no substitution was observed" — which on the Claude CLI
    /// path also covers "nothing observes it there at all" (that path has no
    /// `RUNTIME_OUTCOME` scope). A matrix cell excludes any run with `Some`
    /// rather than crediting one model with another's answer.
    pub substituted: Option<(String, String)>,
}

impl RunFacts {
    fn requested(overrides: &RunOverrides, case: &EvalCaseFile) -> Self {
        RunFacts {
            runtime: overrides.effective_runtime(case).as_str().to_string(),
            model: overrides.effective_model(case).to_string(),
            agent: overrides.effective_agent(case).to_string(),
            seed: overrides.seed,
            seed_applied: SEED_APPLIED_ANYWHERE,
            usage: None,
            substituted: None,
        }
    }
}

/// Resolve the replay/record transcript path for a case:
/// `[case] transcript` (validated relative at load time) or
/// `<case-file-stem>.transcript.jsonl` beside the case file.
///
/// `repeat_index` (1-based) seeds the run index into the filename so
/// `--repeats N` produces N distinguishable transcripts instead of the last
/// repeat silently clobbering the others — `.r<index>` is inserted before
/// the final extension (`<stem>.transcript.jsonl` → `<stem>.transcript.r2.jsonl`).
/// `None` reproduces the pre-existing single-run naming exactly, so the
/// default (`--repeats` unset / `1`) is byte-identical to before this option
/// existed and every already-recorded baseline transcript still resolves.
pub fn transcript_path(
    case_path: &Path,
    case: &EvalCaseFile,
    repeat_index: Option<u32>,
) -> PathBuf {
    let dir = case_path.parent().unwrap_or_else(|| Path::new("."));
    let base = match &case.case.transcript {
        Some(rel) => dir.join(rel),
        None => {
            let stem = case_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("case");
            dir.join(format!("{stem}.transcript.jsonl"))
        }
    };
    seed_repeat_index(base, repeat_index)
}

/// Insert `.r<index>` before the final extension of `path`. No-op when
/// `repeat_index` is `None`.
fn seed_repeat_index(path: PathBuf, repeat_index: Option<u32>) -> PathBuf {
    let Some(idx) = repeat_index else {
        return path;
    };
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("transcript");
    let seeded_name = match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{stem}.r{idx}.{ext}"),
        None => format!("{stem}.r{idx}"),
    };
    path.with_file_name(seeded_name)
}

/// Obtain a parsed transcript per `mode`. Any failure (missing agent,
/// missing binary, timeout, in-band stream error) is an `Err` — the caller
/// records it as a failed case with the message as diagnostics.
///
/// `repeat_index` — see [`transcript_path`] — is `None` for a normal
/// single-run case and `Some(1-based index)` under `--repeats N > 1`.
///
/// `overrides` selects the `(runtime, model)` and carries the paired seed; its
/// `Default` is the pre-P2 Claude CLI path with the case's own model.
pub async fn obtain_transcript(
    case_path: &Path,
    case: &EvalCaseFile,
    home: &Path,
    mode: RunMode,
    repeat_index: Option<u32>,
    overrides: &RunOverrides,
) -> Result<(EvalTranscript, RunFacts), String> {
    let mut facts = RunFacts::requested(overrides, case);
    match mode {
        RunMode::Replay => {
            let path = transcript_path(case_path, case, repeat_index);
            let raw = std::fs::read_to_string(&path).map_err(|e| {
                format!(
                    "replay transcript missing: {} ({e}) — run once with --record to create it",
                    path.display()
                )
            })?;
            Ok((parse_stream_json(&raw)?, facts))
        }
        RunMode::Live { record } => {
            let raw = if overrides.is_claude_cli_path(case) {
                run_live(
                    case,
                    home,
                    overrides.effective_model(case),
                    overrides.effective_agent(case),
                )
                .await?
            } else {
                run_live_via_runtime(case, home, overrides, &mut facts).await?
            };
            if record {
                let path = transcript_path(case_path, case, repeat_index);
                std::fs::write(&path, &raw)
                    .map_err(|e| format!("cannot record transcript {}: {e}", path.display()))?;
            }
            Ok((parse_stream_json(&raw)?, facts))
        }
    }
}

/// Temp file that best-effort deletes itself (system-prompt hand-off; the
/// prompt is not a secret but shouldn't accumulate in tmp).
struct TempPromptFile(PathBuf);

impl TempPromptFile {
    fn create(content: &str) -> Result<Self, String> {
        let path =
            std::env::temp_dir().join(format!("duduclaw-eval-sys-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&path, content)
            .map_err(|e| format!("cannot write system prompt temp file: {e}"))?;
        Ok(TempPromptFile(path))
    }
}

impl Drop for TempPromptFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// WP2.1 side-effect fix: an agent directory copied into a sandbox home (or
/// provisioned by a template) may carry a `.mcp.json` whose duduclaw server
/// entry still points `DUDUCLAW_HOME` at the ORIGINAL (often production)
/// home — a live eval run would then write tasks/memory/audit into
/// production data even though the operator asked for an isolated run.
///
/// This rewrites the duduclaw entries (identified by an `mcp-server` arg) to
/// `DUDUCLAW_HOME = <eval home>` and returns the rewritten JSON. Non-duduclaw
/// servers (playwright, …) are untouched. `None` ⇒ nothing needed rewriting
/// (already correct, or no duduclaw entry) — caller passes the original file.
/// A parse failure also returns `None`: degrading to today's behavior beats
/// refusing to run over a hand-edited config.
pub fn rewrite_mcp_home(raw: &str, home: &Path) -> Option<String> {
    let mut v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let servers = v.get_mut("mcpServers")?.as_object_mut()?;
    let home_str = home.to_string_lossy().into_owned();
    let mut changed = false;
    for (_name, server) in servers.iter_mut() {
        let is_duduclaw = server
            .get("args")
            .and_then(|a| a.as_array())
            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some("mcp-server")));
        if !is_duduclaw {
            continue;
        }
        let obj = server.as_object_mut()?;
        let env = obj
            .entry("env")
            .or_insert_with(|| serde_json::Value::Object(Default::default()))
            .as_object_mut()?;
        if env.get("DUDUCLAW_HOME").and_then(|h| h.as_str()) != Some(home_str.as_str()) {
            env.insert(
                "DUDUCLAW_HOME".to_string(),
                serde_json::Value::String(home_str.clone()),
            );
            changed = true;
        }
        // The CLI's init event echoes the MCP server env into the recorded
        // stream — a production `DUDUCLAW_MCP_API_KEY` would land verbatim in
        // every shipped transcript (found the hard way: 24 recorded baselines
        // carried the real key before this existed). The stdio transport never
        // authenticates with it, so a placeholder is functionally identical.
        if env
            .get("DUDUCLAW_MCP_API_KEY")
            .and_then(|k| k.as_str())
            .is_some_and(|k| k != "eval-local")
        {
            env.insert(
                "DUDUCLAW_MCP_API_KEY".to_string(),
                serde_json::Value::String("eval-local".to_string()),
            );
            changed = true;
        }
    }
    if changed {
        serde_json::to_string_pretty(&v).ok()
    } else {
        None
    }
}

/// Temp `.mcp.json` guard (same lifetime discipline as [`TempPromptFile`]).
struct TempMcpFile(PathBuf);

impl TempMcpFile {
    fn create(content: &str) -> Result<Self, String> {
        let path =
            std::env::temp_dir().join(format!("duduclaw-eval-mcp-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, content)
            .map_err(|e| format!("cannot write rewritten mcp config: {e}"))?;
        Ok(TempMcpFile(path))
    }
}

impl Drop for TempMcpFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Spawn `claude` for the case's agent and return raw stream-json stdout.
///
/// `model` is the effective model (an `--model` override, else `[case] model`);
/// `agent` the effective agent (an `--agent` override, else `[case] agent`).
async fn run_live(
    case: &EvalCaseFile,
    home: &Path,
    model: &str,
    agent: &str,
) -> Result<String, String> {
    let agent_dir = home.join("agents").join(agent);
    if !agent_dir.join("agent.toml").exists() {
        return Err(missing_agent_error(agent, &agent_dir, case));
    }

    let claude = duduclaw_core::which_claude()
        .or_else(|| duduclaw_core::which_claude_in_home(home))
        .ok_or_else(|| "claude CLI not found (PATH + known install locations)".to_string())?;

    // Live eval runs the employee's CLI on the host, never in the task
    // sandbox: report it once per agent per eval run when the sandbox is on.
    duduclaw_gateway::task_sandbox::note_not_applied(
        home,
        agent,
        duduclaw_core::agent_toml::load(&agent_dir)
            .container
            .sandbox_enabled
            .unwrap_or(false),
        duduclaw_gateway::task_sandbox::HostPath::Eval,
        duduclaw_gateway::task_sandbox::HostAction::RanOnHost,
    );

    // Keep the guards alive for the whole child lifetime.
    let sys_file = match case.case.system_prompt.as_deref() {
        Some(sp) if !sp.trim().is_empty() => Some(TempPromptFile::create(sp)?),
        _ => None,
    };
    let mcp_original = agent_dir.join(".mcp.json");
    let mut mcp_temp: Option<TempMcpFile> = None;
    let mcp_path: Option<PathBuf> = if mcp_original.exists() {
        match std::fs::read_to_string(&mcp_original) {
            Ok(raw) => match rewrite_mcp_home(&raw, home) {
                Some(rewritten) => {
                    let guard = TempMcpFile::create(&rewritten)?;
                    let p = guard.0.clone();
                    mcp_temp = Some(guard);
                    Some(p)
                }
                None => Some(mcp_original.clone()),
            },
            Err(_) => Some(mcp_original.clone()),
        }
    } else {
        None
    };

    let capabilities = duduclaw_gateway::runtime::load_agent_capabilities(&agent_dir);
    let args = build_eval_cli_args(
        case,
        model,
        capabilities.as_ref(),
        mcp_path.as_deref(),
        sys_file.as_ref().map(|f| f.0.as_path()),
        duduclaw_core::agent_toml::resolve_minimal_context(Some(&agent_dir)),
    );
    let _keep_alive = &mcp_temp;

    let mut cmd = tokio::process::Command::new(&claude);
    cmd.args(&args)
        .current_dir(&agent_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn {claude}: {e}"))?;

    let timeout = std::time::Duration::from_secs(case.case.timeout_secs);
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| {
            format!(
                "live run timed out after {}s (case timeout_secs)",
                case.case.timeout_secs
            )
        })?
        .map_err(|e| format!("claude CLI wait failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() && stdout.trim().is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "claude CLI exited with {} and no stream output; stderr_tail={:?}",
            output.status,
            duduclaw_core::truncate_bytes(stderr.trim(), 400)
        ));
    }
    Ok(stdout)
}

/// The "no such provisioned agent" error, shared by both live paths.
///
/// When an `--agent` override is in play the message names BOTH ids, so the
/// operator can tell "my override does not exist" from "the case's own agent
/// does not exist" — the smoke run hit the latter 12/12 and the old message
/// could not distinguish them.
fn missing_agent_error(agent: &str, agent_dir: &Path, case: &EvalCaseFile) -> String {
    let overridden = agent != case.case.agent;
    format!(
        "agent '{agent}' not found under {} (live mode needs a provisioned agent; \
         use --replay for offline regression){}",
        agent_dir.display(),
        if overridden {
            format!(
                " — this is the `--agent` override; the case itself declares '{}'",
                case.case.agent
            )
        } else {
            String::new()
        }
    )
}

/// Argument layout mirrors the gateway's `build_claude_cli_args` (harness
/// parity: same permission mode, tool allow/deny wiring, strict per-agent
/// MCP config, system prompt via file). No `--resume`: eval cases are
/// intentionally single-shot and session-free for reproducibility.
fn build_eval_cli_args(
    case: &EvalCaseFile,
    // P2: the effective model — an `--model` override, else `[case] model`.
    model: &str,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
    mcp_config: Option<&Path>,
    system_prompt_file: Option<&Path>,
    // WP-7A: mirror the harness's minimal-context flags so eval stays
    // representative of production spawns. Resolved from the eval agent dir.
    minimal_context: bool,
) -> Vec<String> {
    // WP-7A (bug1): `--exclude-dynamic-system-prompt-sections` removed — a
    // documented no-op when combined with `--system-prompt-file` (which eval
    // always sets for a case with a system prompt).
    let mut args: Vec<String> = vec![
        "-p".into(),
        case.case.prompt.clone(),
        "--model".into(),
        model.to_string(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--dangerously-skip-permissions".into(),
        "--max-turns".into(),
        case.case.max_turns.to_string(),
    ];

    let caps = capabilities.cloned().unwrap_or_default();
    let allowed = caps.allowed_tools();
    if !allowed.is_empty() {
        args.push("--allowedTools".into());
        args.push(allowed.join(","));
    }
    let denied = caps.disallowed_tools();
    if !denied.is_empty() {
        args.push("--disallowedTools".into());
        args.push(denied.join(","));
    }

    // WP-7A minimal-context (harness parity): keep `project,local` so the
    // agent's own `.claude/settings.json` hook survives.
    if minimal_context {
        args.push("--setting-sources".into());
        args.push("project,local".into());
        args.push("--tools".into());
        args.push(
            caps.minimal_builtin_tools(&duduclaw_core::types::CURATED_BUILTIN_TOOLS)
                .join(","),
        );
    }

    if let Some(mcp_json) = mcp_config {
        args.push("--mcp-config".into());
        args.push(mcp_json.to_string_lossy().into_owned());
        args.push("--strict-mcp-config".into());
    }

    if let Some(f) = system_prompt_file {
        args.push("--system-prompt-file".into());
        args.push(f.to_string_lossy().into_owned());
    }

    args
}

// ─────────────────────────────────────────────────────────────────────────
// P2: the runtime-abstraction path (codex / gemini / antigravity / grok /
// openai_compat / every generic print-mode CLI)
// ─────────────────────────────────────────────────────────────────────────

/// Run one case through the gateway's multi-runtime choke-point and return a
/// **synthesized** stream-json transcript.
///
/// Why this entry and not a per-runtime spawn of our own: every vendor-specific
/// argv discipline already lives behind `AgentRuntime::execute` — codex's
/// `--skip-git-repo-check` / `-c approval_policy=never` / capability-derived
/// `--sandbox` / MCP `-c` overrides / null stdin are all inside
/// `runtime/codex.rs`, and duplicating them here would guarantee they drift.
/// `run_agent_prompt` also resolves capabilities, effort and the account pool
/// from the agent directory exactly the way production dispatch does, which is
/// the whole point of measuring a model in *this* harness.
///
/// Two scoped task-locals make the run auditable:
/// * `NATIVE_TOOL_COLLECTOR` — the runtime's own tool events, which become the
///   transcript's `tool_use`/`tool_result` pairs so tool assertions keep working.
/// * `RUNTIME_OUTCOME` — which `(runtime, model)` really answered, so a failover
///   substitution is recorded instead of being credited to the requested model.
///
/// `allow_cross_family_failover: false`: the entire question this run answers is
/// *which family can do this job*, so a silent cross-family substitution would
/// corrupt the cell (the same hole P0/WP-B closed for the acceptance judge).
async fn run_live_via_runtime(
    case: &EvalCaseFile,
    home: &Path,
    overrides: &RunOverrides,
    facts: &mut RunFacts,
) -> Result<String, String> {
    let runtime = overrides.effective_runtime(case);
    let model = overrides.effective_model(case).to_string();
    let agent = overrides.effective_agent(case).to_string();
    let agent_dir = home.join("agents").join(&agent);
    if !agent_dir.join("agent.toml").exists() {
        return Err(missing_agent_error(&agent, &agent_dir, case));
    }

    let native: Arc<Mutex<Vec<NativeToolEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let outcome: Arc<Mutex<Option<duduclaw_gateway::runtime::RuntimeOutcome>>> =
        Arc::new(Mutex::new(None));

    let system_prompt = case.case.system_prompt.clone().unwrap_or_default();
    let call = duduclaw_gateway::runtime_dispatch::run_agent_prompt(
        duduclaw_gateway::runtime_dispatch::AgentPrompt {
            agent_dir: Some(agent_dir.as_path()),
            home_dir: home,
            agent_id: &agent,
            prompt: &case.case.prompt,
            system_prompt: &system_prompt,
            model: &model,
            max_tokens: RUNTIME_PATH_MAX_TOKENS,
            provider_override: Some(runtime),
            conversation_history: &[],
            // Eval is the evolution engine's external yardstick, and this is the
            // classification its usage is already recorded under elsewhere.
            request_type: duduclaw_gateway::cost_telemetry::RequestType::Evolution,
            runtime_settings: None,
            effort: None,
            allow_cross_family_failover: false,
        },
    );
    let response = duduclaw_gateway::runtime::RUNTIME_OUTCOME
        .scope(
            Arc::clone(&outcome),
            duduclaw_gateway::runtime::NATIVE_TOOL_COLLECTOR.scope(Arc::clone(&native), call),
        )
        .await?;

    facts.usage = ReportedUsage::from_response(&response);
    if let Some(observed) = outcome.lock().ok().and_then(|g| g.clone()) {
        let observed_runtime = observed.runtime.as_str().to_string();
        if observed_runtime != facts.runtime || observed.model != facts.model {
            facts.substituted = Some((observed_runtime, observed.model));
        }
    }

    let events = native.lock().map(|g| g.clone()).unwrap_or_default();
    // A runtime that handed back its raw event stream instead of the message
    // would otherwise become the synthesized transcript's `final_text`, and
    // every `output_contains` / `[[expect.grounded]]` assertion would be checked
    // against JSONL. Fail-open: unchanged when the content is already a message.
    let content = normalize_runtime_message_text(&response.content);
    Ok(synthesize_stream_json(
        runtime.as_str(),
        &model,
        &content,
        &events,
    ))
}

// ─────────────────────────────────────────────────────────────────────────
// Recovering the agent's MESSAGE out of whatever a runtime handed back
// ─────────────────────────────────────────────────────────────────────────

/// Event `type` values that mark a line as a CLI stream event rather than the
/// agent's own answer.
///
/// The list is a *guard*, not a parser: a reply is only treated as a stream when
/// at least one of its lines is a JSON object carrying one of these types. That
/// is what keeps a legitimate structured answer — notably the verifier's
/// `{"verdict":"PASS","reasons":[...]}`, which has no `type` key at all — from
/// being mistaken for a transcript and rewritten.
const STREAM_EVENT_TYPES: &[&str] = &[
    // codex
    "thread.started",
    "turn.started",
    "turn.completed",
    "turn.failed",
    "item.started",
    "item.updated",
    "item.completed",
    // claude stream-json
    "system",
    "assistant",
    "user",
    "result",
];

/// Recover the agent's message text when a runtime hands back a raw event
/// stream instead of the message.
///
/// **Why this exists (smoke 3, 2026-09-25).** Every codex verifier reply scored
/// `unparseable`, and the recorded first line was
/// `{"type":"turn.completed","usage":{…}}` — the stream's last event, not the
/// agent's answer. Root cause is upstream in the gateway, which this crate may
/// not edit: `runtime/codex.rs::parse_codex_stdout` recognises only the
/// `item.completed` + `item.type == "message"` + `content[].type ==
/// "output_text"` shape, so a CLI that emits the `agent_message` shape yields
/// empty content, and `CodexRuntime::execute` then falls back to
/// `stdout.lines().last()`. Every caller of that runtime inherits the defect;
/// this function makes the eval path immune to it and to the mirror-image
/// problem on the Claude side (a stream-json blob arriving where text was
/// expected).
///
/// Extraction is **last-wins** (the final answer) over:
/// * codex `item.completed` where `item.type` is `agent_message` / `message`,
///   reading `item.text` or `item.content[]` blocks of type `output_text` /
///   `text` — the same dual-name tolerance `codex.rs` applies elsewhere;
/// * claude `assistant` events' `message.content[]` text blocks, and a `result`
///   event's non-empty `result` string (the CLI's own precedence).
///
/// **Fail-open by construction**: anything that does not look like a stream, and
/// any stream from which no message can be recovered, is returned unchanged. It
/// never fabricates text and never mangles a plain prose or JSON answer.
pub fn normalize_runtime_message_text(raw: &str) -> String {
    let parsed: Vec<serde_json::Value> = raw
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.is_object())
        .collect();
    let looks_like_stream = parsed.iter().any(|v| {
        v.get("type")
            .and_then(|t| t.as_str())
            .is_some_and(|t| STREAM_EVENT_TYPES.contains(&t))
    });
    if !looks_like_stream {
        return raw.to_string();
    }

    let mut recovered: Option<String> = None;
    for event in &parsed {
        match event.get("type").and_then(|t| t.as_str()) {
            Some("item.completed") => {
                let Some(item) = event.get("item") else {
                    continue;
                };
                if !matches!(
                    item.get("type").and_then(|t| t.as_str()),
                    Some("agent_message") | Some("message")
                ) {
                    continue;
                }
                if let Some(text) = item
                    .get("text")
                    .and_then(|t| t.as_str())
                    .filter(|t| !t.trim().is_empty())
                {
                    recovered = Some(text.to_string());
                    continue;
                }
                if let Some(text) = content_block_text(item.get("content")) {
                    recovered = Some(text);
                }
            }
            Some("assistant") => {
                if let Some(text) = content_block_text(event.pointer("/message/content")) {
                    recovered = Some(text);
                }
            }
            Some("result") => {
                if let Some(text) = event
                    .get("result")
                    .and_then(|r| r.as_str())
                    .filter(|t| !t.trim().is_empty())
                {
                    recovered = Some(text.to_string());
                }
            }
            _ => {}
        }
    }
    recovered.unwrap_or_else(|| raw.to_string())
}

/// Concatenate the text of a content-block array (`output_text` / `text`).
/// `None` when the value is not such an array or carries no non-blank text.
fn content_block_text(content: Option<&serde_json::Value>) -> Option<String> {
    let arr = content?.as_array()?;
    let parts: Vec<&str> = arr
        .iter()
        .filter(|b| {
            matches!(
                b.get("type").and_then(|t| t.as_str()),
                Some("output_text") | Some("text")
            )
        })
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect();
    let joined = parts.join("\n");
    (!joined.trim().is_empty()).then_some(joined)
}

/// Build a stream-json transcript from `(final text, native tool events)`.
///
/// Emitted in the Claude CLI's own event shape so the ONE existing parser
/// ([`parse_stream_json`]) handles both paths — no second transcript builder, no
/// second set of assertion semantics. Line order:
///
/// 1. a `system` marker naming the runtime/model and the synthesis itself,
/// 2. one `assistant`/`tool_use` + `user`/`tool_result` pair per event, ids
///    assigned positionally (`eval-native-<i>`) so the parser's id-based pairing
///    is exact rather than falling back to its tolerant stack,
/// 3. one `assistant`/`text` block with the final answer,
/// 4. a `result` event repeating it (matching the CLI's own precedence).
///
/// A tool event whose `input_text`/`result_text` the collector never captured
/// contributes `input: null` / no `content` — never a fabricated payload.
pub fn synthesize_stream_json(
    runtime_id: &str,
    model: &str,
    content: &str,
    events: &[NativeToolEvent],
) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(events.len() * 2 + 3);
    lines.push(
        serde_json::json!({
            "type": "system",
            "subtype": SYNTHETIC_MARKER_SUBTYPE,
            "runtime": runtime_id,
            "model": model,
            "note": "synthesized by `duduclaw eval` from a RuntimeResponse + NativeToolEvents; \
                     not a recording of a CLI stream",
            "tool_events": events.len(),
        })
        .to_string(),
    );
    for (i, e) in events.iter().enumerate() {
        let id = format!("eval-native-{i}");
        let input = match e.input_text.as_deref() {
            Some(t) => serde_json::json!({ "text": t }),
            None => serde_json::Value::Null,
        };
        lines.push(
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "id": id,
                        "name": e.tool_name,
                        "input": input,
                    }]
                }
            })
            .to_string(),
        );
        let mut result = serde_json::json!({
            "type": "tool_result",
            "tool_use_id": id,
            "is_error": !e.success,
        });
        if let Some(t) = e.result_text.as_deref() {
            result["content"] = serde_json::Value::String(t.to_string());
        }
        lines.push(
            serde_json::json!({
                "type": "user",
                "message": { "content": [result] }
            })
            .to_string(),
        );
    }
    lines.push(
        serde_json::json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": content }] }
        })
        .to_string(),
    );
    lines.push(
        serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": content,
        })
        .to_string(),
    );
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(extra: &str) -> EvalCaseFile {
        let toml = format!(
            "[case]\nname = \"t\"\nagent = \"support-bot\"\nprompt = \"hi there\"\n{extra}[judge]\nrubric = \"r\"\n"
        );
        toml::from_str(&toml).unwrap()
    }

    #[test]
    fn transcript_path_defaults_to_case_stem() {
        let c = case("");
        let p = transcript_path(Path::new("/suite/refund-flow.toml"), &c, None);
        assert_eq!(p, PathBuf::from("/suite/refund-flow.transcript.jsonl"));
    }

    #[test]
    fn transcript_path_honours_explicit_relative() {
        let c = case("transcript = \"recorded/run1.jsonl\"\n");
        let p = transcript_path(Path::new("/suite/refund-flow.toml"), &c, None);
        assert_eq!(p, PathBuf::from("/suite/recorded/run1.jsonl"));
    }

    #[test]
    fn transcript_path_seeds_repeat_index_before_final_extension() {
        let c = case("");
        let p = transcript_path(Path::new("/suite/refund-flow.toml"), &c, Some(2));
        assert_eq!(p, PathBuf::from("/suite/refund-flow.transcript.r2.jsonl"));

        let c2 = case("transcript = \"recorded/run1.jsonl\"\n");
        let p2 = transcript_path(Path::new("/suite/refund-flow.toml"), &c2, Some(3));
        assert_eq!(p2, PathBuf::from("/suite/recorded/run1.r3.jsonl"));
    }

    #[test]
    fn cli_args_mirror_harness_invocation() {
        let c = case("model = \"claude-haiku-4-5\"\nmax_turns = 7\n");
        // minimal_context = false → no minimal flags, no dead exclude-dynamic.
        let args = build_eval_cli_args(&c, c.model(), None, None, None, false);
        let joined = args.join(" ");
        assert!(joined.contains("-p hi there"));
        assert!(joined.contains("--model claude-haiku-4-5"));
        assert!(joined.contains("--output-format stream-json"));
        assert!(joined.contains("--max-turns 7"));
        assert!(joined.contains("--dangerously-skip-permissions"));
        assert!(!joined.contains("--mcp-config"));
        assert!(!joined.contains("--system-prompt-file"));
        // WP-7A: the dead flag is gone; minimal flags only appear when enabled.
        assert!(!joined.contains("--exclude-dynamic-system-prompt-sections"));
        assert!(!joined.contains("--setting-sources"));
        assert!(!joined.contains("--tools"));
    }

    #[test]
    fn cli_args_minimal_context_adds_setting_sources_and_tools() {
        let c = case("");
        let args = build_eval_cli_args(&c, c.model(), None, None, None, true);
        let joined = args.join(" ");
        assert!(joined.contains("--setting-sources project,local"));
        // Curated built-in set, no operator globals dropped via "".
        assert!(joined.contains("--tools "));
        assert!(joined.contains("TodoWrite"));
        assert!(!joined.contains("--exclude-dynamic-system-prompt-sections"));
    }

    #[test]
    fn cli_args_wire_capabilities_and_mcp_and_sysprompt() {
        let c = case("");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".mcp.json"), "{}").unwrap();
        let caps = duduclaw_core::types::CapabilitiesConfig {
            allowed_tools: vec!["Read".into(), "mcp__duduclaw__tasks_create".into()],
            denied_tools: vec!["Bash".into()],
            ..Default::default()
        };
        let sys = dir.path().join("sys.txt");
        let mcp = dir.path().join(".mcp.json");
        let args = build_eval_cli_args(&c, c.model(), Some(&caps), Some(&mcp), Some(&sys), false);
        let joined = args.join(" ");
        assert!(joined.contains("--allowedTools"));
        assert!(joined.contains("--disallowedTools Bash"));
        assert!(joined.contains("--mcp-config"));
        assert!(joined.contains("--strict-mcp-config"));
        assert!(joined.contains("--system-prompt-file"));
    }

    // ── WP2.1 side-effect fix: DUDUCLAW_HOME rewrite ─────────────────

    #[test]
    fn rewrite_points_duduclaw_home_at_the_eval_home() {
        let raw = r#"{"mcpServers":{"duduclaw":{"command":"/usr/local/bin/duduclaw","args":["mcp-server"],"env":{"DUDUCLAW_HOME":"/Users/prod/.duduclaw","DUDUCLAW_AGENT_ID":"law-intake","DUDUCLAW_MCP_API_KEY":"ddc_prod_deadbeef"}},"playwright":{"command":"npx","args":["-y","@playwright/mcp"]}}}"#;
        let out = rewrite_mcp_home(raw, Path::new("/tmp/sandbox-home")).expect("must rewrite");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_HOME"], "/tmp/sandbox-home",
            "duduclaw server must point at the eval home"
        );
        // The production MCP key never reaches the recorded stream.
        assert_eq!(
            v["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_MCP_API_KEY"],
            "eval-local"
        );
        // Sibling env vars and non-duduclaw servers are untouched.
        assert_eq!(
            v["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_AGENT_ID"],
            "law-intake"
        );
        assert!(v["mcpServers"]["playwright"].get("env").is_none());
    }

    #[test]
    fn rewrite_inserts_home_when_env_was_absent() {
        let raw = r#"{"mcpServers":{"duduclaw":{"command":"duduclaw","args":["mcp-server"]}}}"#;
        let out = rewrite_mcp_home(raw, Path::new("/tmp/h")).expect("must rewrite");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_HOME"],
            "/tmp/h"
        );
    }

    #[test]
    fn rewrite_is_a_noop_when_already_correct_or_unparseable() {
        let correct = r#"{"mcpServers":{"duduclaw":{"command":"duduclaw","args":["mcp-server"],"env":{"DUDUCLAW_HOME":"/tmp/h"}}}}"#;
        assert!(rewrite_mcp_home(correct, Path::new("/tmp/h")).is_none());
        assert!(rewrite_mcp_home("not json", Path::new("/tmp/h")).is_none());
        // No duduclaw entry ⇒ nothing to do.
        let other = r#"{"mcpServers":{"playwright":{"command":"npx","args":["x"]}}}"#;
        assert!(rewrite_mcp_home(other, Path::new("/tmp/h")).is_none());
    }

    #[tokio::test]
    async fn replay_reads_and_parses_recorded_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let case_path = dir.path().join("t.toml");
        let c = case("");
        std::fs::write(
            dir.path().join("t.transcript.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n",
        )
        .unwrap();
        let (t, facts) = obtain_transcript(
            &case_path,
            &c,
            dir.path(),
            RunMode::Replay,
            None,
            &RunOverrides::default(),
        )
        .await
        .unwrap();
        assert_eq!(t.final_text, "hi");
        // Default overrides ⇒ the pre-P2 identity: claude + the case's model.
        assert_eq!(facts.runtime, "claude");
        assert_eq!(facts.model, c.model());
        assert_eq!(facts.agent, "support-bot", "the case's own agent");
        assert_eq!(facts.seed, None);
        assert_eq!(facts.usage, None);
        assert_eq!(facts.substituted, None);
    }

    #[tokio::test]
    async fn replay_missing_transcript_is_actionable_error() {
        let dir = tempfile::tempdir().unwrap();
        let case_path = dir.path().join("t.toml");
        let c = case("");
        let err = obtain_transcript(
            &case_path,
            &c,
            dir.path(),
            RunMode::Replay,
            None,
            &RunOverrides::default(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("--record"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn live_without_agent_dir_is_actionable_error() {
        let dir = tempfile::tempdir().unwrap(); // empty home: no agents/
        let case_path = dir.path().join("t.toml");
        let c = case("");
        let err = obtain_transcript(
            &case_path,
            &c,
            dir.path(),
            RunMode::Live { record: false },
            None,
            &RunOverrides::default(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("not found"), "unexpected: {err}");
        assert!(err.contains("--replay"), "unexpected: {err}");
    }

    // ── P2: runtime/model overrides + synthesized transcripts ────────────

    #[test]
    fn overrides_default_to_the_claude_cli_path_and_the_case_model() {
        let c = case("model = \"claude-haiku-4-5\"\n");
        let o = RunOverrides::default();
        assert!(o.is_claude_cli_path(&c));
        assert_eq!(o.effective_runtime(&c), RuntimeType::Claude);
        assert_eq!(o.effective_model(&c), "claude-haiku-4-5");
    }

    #[test]
    fn overrides_select_runtime_and_model_and_ignore_blank_models() {
        let c = case("model = \"claude-haiku-4-5\"\n");
        let o = RunOverrides {
            runtime: Some(RuntimeType::Codex),
            model: Some("gpt-5.6-sol".to_string()),
            agent: None,
            seed: Some(42),
        };
        assert!(!o.is_claude_cli_path(&c));
        assert_eq!(o.effective_model(&c), "gpt-5.6-sol");
        // A blank/whitespace override must not blank out the case's model.
        let blank = RunOverrides {
            model: Some("   ".to_string()),
            ..Default::default()
        };
        assert_eq!(blank.effective_model(&c), "claude-haiku-4-5");
        // An explicit `--runtime claude` still takes the direct CLI path.
        let explicit = RunOverrides {
            runtime: Some(RuntimeType::Claude),
            ..Default::default()
        };
        assert!(explicit.is_claude_cli_path(&c));
    }

    /// Review finding 10 regression: a case that pins `[case] runtime` with no
    /// CLI override must take THAT runtime's path — the `--record` flow is
    /// refused together with `--runtime`/`--model`, so the pinned field is the
    /// *only* way to record a non-Claude baseline, and it used to be read by
    /// nobody (`effective_runtime` took no `case`), which sent the case's codex
    /// model to the `claude` CLI as a `--model`.
    #[test]
    fn a_case_pinned_runtime_is_honoured_when_no_cli_override_is_given() {
        let c = case("runtime = \"codex\"\nmodel = \"gpt-5.6-sol\"\n");
        let o = RunOverrides::default();
        assert_eq!(o.effective_runtime(&c), RuntimeType::Codex);
        assert!(
            !o.is_claude_cli_path(&c),
            "a codex-pinned case must not take the direct `claude` CLI path"
        );
        assert_eq!(o.effective_model(&c), "gpt-5.6-sol");

        // A CLI override still wins over the pin.
        let override_claude = RunOverrides {
            runtime: Some(RuntimeType::Claude),
            ..Default::default()
        };
        assert_eq!(override_claude.effective_runtime(&c), RuntimeType::Claude);

        // An unpinned case is unchanged (Claude).
        let plain = case("model = \"claude-haiku-4-5\"\n");
        assert_eq!(o.effective_runtime(&plain), RuntimeType::Claude);
    }

    /// The report header must name what actually ran, not the default.
    #[test]
    fn run_facts_report_the_case_pinned_runtime() {
        let c = case("runtime = \"gemini\"\nmodel = \"gemini-3.7-flash\"\n");
        let facts = RunFacts::requested(&RunOverrides::default(), &c);
        assert_eq!(facts.runtime, "gemini");
        assert_eq!(facts.model, "gemini-3.7-flash");
    }

    #[test]
    fn the_agent_override_replaces_the_case_agent_and_blank_does_not() {
        let c = case("");
        assert_eq!(RunOverrides::default().effective_agent(&c), "support-bot");
        let o = RunOverrides {
            agent: Some("agnes".to_string()),
            ..Default::default()
        };
        assert_eq!(o.effective_agent(&c), "agnes");
        let blank = RunOverrides {
            agent: Some("   ".to_string()),
            ..Default::default()
        };
        assert_eq!(blank.effective_agent(&c), "support-bot");
    }

    #[tokio::test]
    async fn a_missing_override_agent_names_both_ids() {
        let dir = tempfile::tempdir().unwrap(); // empty home: no agents/
        let case_path = dir.path().join("t.toml");
        let c = case("");
        let err = obtain_transcript(
            &case_path,
            &c,
            dir.path(),
            RunMode::Live { record: false },
            None,
            &RunOverrides {
                agent: Some("agnes".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(err.contains("agent 'agnes' not found"), "{err}");
        assert!(err.contains("--agent` override"), "{err}");
        assert!(err.contains("declares 'support-bot'"), "{err}");
        // Without an override the message stays the pre-P2 one (no extra half).
        let plain = obtain_transcript(
            &case_path,
            &c,
            dir.path(),
            RunMode::Live { record: false },
            None,
            &RunOverrides::default(),
        )
        .await
        .unwrap_err();
        assert!(plain.contains("agent 'support-bot' not found"), "{plain}");
        assert!(!plain.contains("override"), "{plain}");
    }

    #[test]
    fn synthesized_transcript_parses_back_into_the_signals_assertions_need() {
        let events = vec![
            NativeToolEvent {
                tool_name: "Bash".to_string(),
                success: true,
                result_text: Some("total 4\nnotes.md".to_string()),
                input_text: Some("ls -la".to_string()),
            },
            NativeToolEvent {
                tool_name: "Write".to_string(),
                success: false,
                result_text: None,
                input_text: None,
            },
        ];
        let raw = synthesize_stream_json("codex", "gpt-5.6-sol", "done: notes.md", &events);
        assert!(raw.contains(SYNTHETIC_MARKER_SUBTYPE), "{raw}");
        let t = parse_stream_json(&raw).expect("synthesized transcript must parse");
        assert_eq!(t.final_text, "done: notes.md");
        assert_eq!(t.tool_uses.len(), 2);
        assert_eq!(t.tool_uses[0].name, "Bash");
        assert_eq!(
            t.tool_uses[0].result_text.as_deref(),
            Some("total 4\nnotes.md")
        );
        assert!(!t.tool_uses[0].is_error);
        // A failed event pairs as an error with no fabricated result text.
        assert_eq!(t.tool_uses[1].name, "Write");
        assert!(t.tool_uses[1].is_error);
        assert_eq!(t.tool_uses[1].result_text, None);
        assert_eq!(t.tool_uses[1].input, serde_json::Value::Null);
        // Documented fidelity limit: exactly one text block, no thinking.
        assert_eq!(t.text_blocks, 1);
        assert_eq!(t.thinking_blocks, 0);
    }

    #[test]
    fn synthesized_transcript_with_no_tool_events_is_still_valid() {
        let raw = synthesize_stream_json("gemini", "gemini-2.5-flash", "hello", &[]);
        let t = parse_stream_json(&raw).expect("parses");
        assert_eq!(t.final_text, "hello");
        assert!(t.tool_uses.is_empty());
        assert_eq!(t.result_events, 1);
    }

    // ── smoke-3 bug 2: the agent's message, not a stream line ────────────

    /// The exact shape that produced 4/4 `unparseable`: a codex stream whose
    /// answer is an `agent_message` item and whose LAST line is `turn.completed`.
    const CODEX_VERDICT_STREAM: &str = concat!(
        r#"{"type":"thread.started","thread_id":"t1"}"#,
        "\n",
        r#"{"type":"turn.started"}"#,
        "\n",
        r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"{\"verdict\":\"FAIL\",\"reasons\":[\"no memory_search call\"]}"}}"#,
        "\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":50}}"#,
        "\n",
    );

    #[test]
    fn a_codex_stream_yields_its_agent_message_not_the_last_event() {
        let text = normalize_runtime_message_text(CODEX_VERDICT_STREAM);
        assert_eq!(
            text,
            r#"{"verdict":"FAIL","reasons":["no memory_search call"]}"#
        );
        assert!(
            !text.contains("turn.completed"),
            "the stream's last event must never become the answer: {text}"
        );
    }

    #[test]
    fn a_codex_stream_in_the_older_message_output_text_shape_also_works() {
        let raw = concat!(
            r#"{"type":"item.completed","item":{"type":"message","content":[{"type":"output_text","text":"PASS"}]}}"#,
            "
",
            r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}"#,
        );
        assert_eq!(normalize_runtime_message_text(raw), "PASS");
    }

    #[test]
    fn a_claude_stream_json_blob_yields_its_final_assistant_text() {
        let raw = concat!(
            r#"{"type":"system","subtype":"init"}"#,
            "
",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"first"}]}}"#,
            "
",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"PASS final"}]}}"#,
            "
",
            r#"{"type":"result","subtype":"success","result":"PASS final"}"#,
        );
        assert_eq!(
            normalize_runtime_message_text(raw),
            "PASS final",
            "last-wins, and a `result` event's text has the CLI's own precedence"
        );
    }

    #[test]
    fn a_plain_answer_is_returned_byte_identical() {
        for raw in [
            "PASS",
            "FAIL
missing memory_search",
            // The critical one: the structured verdict has no `type` key, so it
            // must never be mistaken for a stream and rewritten.
            r#"{"verdict":"PASS","reasons":[]}"#,
            "```json\n{\"verdict\": \"FAIL\"}\n```",
            "",
            "   ",
            // An object with a `type` that is not a stream event stays untouched.
            r#"{"type":"analysis","verdict":"PASS"}"#,
        ] {
            assert_eq!(
                normalize_runtime_message_text(raw),
                raw,
                "must pass through unchanged: {raw:?}"
            );
        }
    }

    #[test]
    fn a_stream_with_no_recoverable_message_is_returned_unchanged() {
        // Only usage events — nothing to recover, so fail open rather than
        // fabricate or blank the reply.
        let raw = concat!(
            r#"{"type":"turn.started"}"#,
            "
",
            r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}"#,
        );
        assert_eq!(normalize_runtime_message_text(raw), raw);
        // An `agent_message` with blank text is also "nothing recovered".
        let blank = r#"{"type":"item.completed","item":{"type":"agent_message","text":"  "}}"#;
        assert_eq!(normalize_runtime_message_text(blank), blank);
    }

    #[test]
    fn a_synthesized_transcript_built_from_a_stream_carries_the_message() {
        // End-to-end of the executor path's fix: raw stream in ⇒ the assertion
        // layer sees the message as `final_text`.
        let content = normalize_runtime_message_text(CODEX_VERDICT_STREAM);
        let raw = synthesize_stream_json("codex", "gpt-5.6-sol", &content, &[]);
        let t = parse_stream_json(&raw).expect("parses");
        assert_eq!(
            t.final_text,
            r#"{"verdict":"FAIL","reasons":["no memory_search call"]}"#
        );
        assert!(!t.final_text.contains("turn.completed"));
    }

    #[test]
    fn reported_usage_distinguishes_free_from_unmeasured() {
        let zero = duduclaw_gateway::runtime::RuntimeResponse {
            content: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            model_used: "m".to_string(),
            runtime_name: "claude".to_string(),
        };
        assert_eq!(
            ReportedUsage::from_response(&zero),
            None,
            "an all-zero usage block is `unmeasured`, never `Some(0)`"
        );
        let some = duduclaw_gateway::runtime::RuntimeResponse {
            input_tokens: 10,
            ..zero
        };
        assert_eq!(
            ReportedUsage::from_response(&some),
            Some(ReportedUsage {
                input_tokens: 10,
                output_tokens: 0,
                cache_read_tokens: 0,
            })
        );
    }
}
