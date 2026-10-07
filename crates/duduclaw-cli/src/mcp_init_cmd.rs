//! `duduclaw mcp init` — set up the standalone MCP server for one AI client.
//!
//! A developer arriving from an MCP directory listing (official MCP Registry,
//! Glama, awesome-mcp-servers) does not run the DuDuClaw gateway. This command
//! gives them a working `duduclaw mcp-server` in one step: it creates the data
//! directory if needed, issues a refresh token for the standalone scope set
//! through the existing [`crate::mcp_refresh::issue_refresh_token`], and either
//! prints the client configuration or (Claude Code) registers it with
//! `claude mcp add`.
//!
//! ## Why the token is external
//!
//! Measured 2026-10-07 on an isolated home with the 1.70.1 binary, both kinds
//! of key keep memory in the caller's own namespace (`internal/<client>` or
//! `external/<client>`). They differ in the wiki and in what else they reach:
//!
//! * an internal non-agent key writes its wiki into the **process's default
//!   agent** (`[general] default_agent`, `dudu` when unset): on a fresh home
//!   that creates `agents/dudu/` without an `agent.toml`, which later blocks
//!   `duduclaw agent create dudu`; on an existing platform home it writes into
//!   the main employee's wiki, whose pages are injected into its prompts;
//! * an external key writes its wiki into `agents/<client>/wiki` (its own),
//!   has `agent_id` / `namespace` arguments stripped by the dispatch gate,
//!   is audited as `external`, and can never be widened beyond
//!   [`crate::mcp_auth::EXTERNALLY_GRANTABLE_SCOPES`].
//!
//! So the standalone token is external, and `--scopes` only accepts
//! externally grantable scopes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use duduclaw_core::error::{DuDuClawError, Result};

use crate::mcp_auth::{EXTERNALLY_GRANTABLE_SCOPES, Scope, parse_scopes};

/// The default standalone scope set: memory and wiki, read and write.
///
/// `messaging:send` is grantable too but its tools need channels configured
/// in `config.toml`, so it is left out of the default (see
/// `docs/guides/mcp-standalone.md` for the measured tool table).
pub(crate) const STANDALONE_SCOPES: &str = "memory:read,memory:write,wiki:read,wiki:write";

/// The client-config server name used in every snippet.
pub(crate) const SERVER_NAME: &str = "duduclaw";

/// Environment label embedded in the issued token (`ddc_refresh_<label>_…`).
const TOKEN_ENV_LABEL: &str = "prod";

/// English first: a standalone user arriving from an MCP directory may not
/// read Chinese.
const AI_SESSION_REFUSAL: &str = "duduclaw mcp init refused: this looks like a DuDuClaw AI \
     employee's session, and an AI employee may not issue MCP keys for itself. \
     If this is your own terminal, unset the variable(s) named below and run it again.\n\
     duduclaw mcp init 只能由操作者在自己的終端機執行，AI 員工的工作階段不能為自己簽發 MCP 金鑰。\
     若這是你自己的終端機，請先 unset 下列變數再執行。";

/// The AI client to configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum InitClient {
    /// Claude Code: register with `claude mcp add` (user scope).
    ClaudeCode,
    /// OpenAI Codex CLI: print the `~/.codex/config.toml` block.
    Codex,
    /// Cursor: print the `~/.cursor/mcp.json` entry.
    Cursor,
    /// Print the configuration for all three clients. Registers nothing with
    /// any client, but still issues a new key on every run.
    Print,
}

impl InitClient {
    pub(crate) fn slug(self) -> &'static str {
        match self {
            InitClient::ClaudeCode => "claude-code",
            InitClient::Codex => "codex",
            InitClient::Cursor => "cursor",
            InitClient::Print => "print",
        }
    }

    /// The token's `client_id`, shown in audit logs and `mcp list-tokens`.
    /// Always starts with [`duduclaw_core::STANDALONE_CLIENT_PREFIX`], which
    /// no employee may be named with, so the key's wiki directory
    /// (`agents/<client_id>/wiki/`) can never be an employee's.
    pub(crate) fn client_id(self) -> String {
        format!("{}{}", duduclaw_core::STANDALONE_CLIENT_PREFIX, self.slug())
    }
}

/// Options parsed from the command line.
#[derive(Debug, Clone)]
pub(crate) struct InitOptions {
    pub client: InitClient,
    pub scopes: String,
    pub yes: bool,
    /// Replace a `duduclaw` entry in Claude Code that this command did not
    /// write (one whose key is not a `standalone-*` key of this home).
    pub replace: bool,
}

/// How the client should start the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Launch {
    pub command: String,
    pub args: Vec<String>,
}

/// Parse and check `--scopes`: non-empty, known, externally grantable.
pub(crate) fn validate_scopes(raw: &str) -> std::result::Result<HashSet<Scope>, String> {
    let scopes = parse_scopes(raw).map_err(|e| format!("invalid --scopes: {e}"))?;
    if scopes.is_empty() {
        return Err(format!(
            "--scopes is empty; the standalone default is {STANDALONE_SCOPES}"
        ));
    }
    let mut refused: Vec<String> = scopes
        .iter()
        .filter(|s| !EXTERNALLY_GRANTABLE_SCOPES.contains(s))
        .map(|s| s.to_string())
        .collect();
    if !refused.is_empty() {
        refused.sort();
        let allowed: Vec<String> = EXTERNALLY_GRANTABLE_SCOPES
            .iter()
            .map(|s| s.to_string())
            .collect();
        return Err(format!(
            "--scopes {} cannot be granted to a standalone client (allowed: {}). \
             Keys for other scopes are issued with `duduclaw mcp issue-refresh-token`.",
            refused.join(","),
            allowed.join(",")
        ));
    }
    Ok(scopes)
}

/// The npm package spec a client launches through `npx`: pinned to this
/// binary's version, so the server the client starts is the one that issued
/// the key (a bare `duduclaw` would follow whatever npm calls latest).
pub(crate) fn npx_package_spec() -> String {
    format!("duduclaw@{}", env!("CARGO_PKG_VERSION"))
}

/// The command a client should run. A binary inside the npx cache
/// (`…/_npx/…`) is not a stable path, so the client gets
/// `npx -y duduclaw@<this version>`.
pub(crate) fn launch_for_exe(exe: &Path) -> Launch {
    let in_npx_cache = exe
        .components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new("_npx"));
    if in_npx_cache {
        Launch {
            command: "npx".to_string(),
            args: vec!["-y".into(), npx_package_spec(), "mcp-server".into()],
        }
    } else {
        Launch {
            command: exe.to_string_lossy().into_owned(),
            args: vec!["mcp-server".into()],
        }
    }
}

/// Environment for the client entry: the key, plus `DUDUCLAW_HOME` when this
/// run used a non-default data directory (the server must open the same one).
pub(crate) fn client_env(token: &str, custom_home: Option<&Path>) -> Vec<(String, String)> {
    let mut env = vec![("DUDUCLAW_MCP_API_KEY".to_string(), token.to_string())];
    if let Some(home) = custom_home {
        env.push((
            "DUDUCLAW_HOME".to_string(),
            home.to_string_lossy().into_owned(),
        ));
    }
    env
}

/// `claude mcp add` arguments (after the `claude` program name).
pub(crate) fn claude_add_args(launch: &Launch, env: &[(String, String)]) -> Vec<String> {
    let mut args = vec![
        "mcp".to_string(),
        "add".to_string(),
        SERVER_NAME.to_string(),
        "-s".to_string(),
        "user".to_string(),
    ];
    for (k, v) in env {
        args.push("-e".to_string());
        args.push(format!("{k}={v}"));
    }
    args.push("--".to_string());
    args.push(launch.command.clone());
    args.extend(launch.args.iter().cloned());
    args
}

/// Which shell the printed command line is quoted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellStyle {
    /// bash / zsh / sh: single quotes.
    Posix,
    /// PowerShell and cmd.exe: double quotes (no single-quote quoting there).
    Windows,
}

impl ShellStyle {
    /// The style of the platform this binary runs on.
    pub(crate) fn native() -> Self {
        if cfg!(windows) {
            ShellStyle::Windows
        } else {
            ShellStyle::Posix
        }
    }
}

/// Quote one word for display in a shell command line.
fn shell_word(s: &str, style: ShellStyle) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@,+".contains(c));
    if plain {
        return s.to_string();
    }
    match style {
        ShellStyle::Posix => format!("'{}'", s.replace('\'', r"'\''")),
        ShellStyle::Windows => format!("\"{}\"", s.replace('"', "\\\"")),
    }
}

/// The `claude mcp add …` command line as the user would type it.
pub(crate) fn claude_command_line(
    launch: &Launch,
    env: &[(String, String)],
    style: ShellStyle,
) -> String {
    std::iter::once("claude".to_string())
        .chain(
            claude_add_args(launch, env)
                .iter()
                .map(|a| shell_word(a, style)),
        )
        .collect::<Vec<_>>()
        .join(" ")
}

/// TOML basic-string escaping.
fn toml_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The `~/.codex/config.toml` block.
pub(crate) fn codex_block(launch: &Launch, env: &[(String, String)]) -> String {
    let args = launch
        .args
        .iter()
        .map(|a| toml_str(a))
        .collect::<Vec<_>>()
        .join(", ");
    let env = env
        .iter()
        .map(|(k, v)| format!("{k} = {}", toml_str(v)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[mcp_servers.{SERVER_NAME}]\ncommand = {}\nargs = [{args}]\nenv = {{ {env} }}\n",
        toml_str(&launch.command)
    )
}

/// The `~/.cursor/mcp.json` document (merge the inner entry by hand when the
/// file already has other servers).
pub(crate) fn cursor_json(launch: &Launch, env: &[(String, String)]) -> String {
    let env: serde_json::Map<String, serde_json::Value> = env
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let doc = serde_json::json!({
        "mcpServers": {
            SERVER_NAME: {
                "command": launch.command,
                "args": launch.args,
                "env": env,
            }
        }
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

/// What `run` decided to do, for the caller to print and tests to inspect.
#[derive(Debug)]
pub(crate) struct InitPlan {
    pub client_id: String,
    pub jti: String,
    pub launch: Launch,
    pub env: Vec<(String, String)>,
}

/// Steps 1 and 2: refuse inside an AI session, create the home, issue the
/// token. Pure of any client-side effect, so tests can call it directly.
pub(crate) fn prepare(
    home: &Path,
    opts: &InitOptions,
    exe: &Path,
    custom_home: Option<&Path>,
    ai_markers: &[&str],
) -> Result<InitPlan> {
    refuse_in_ai_session(ai_markers)?;
    let scopes = validate_scopes(&opts.scopes).map_err(DuDuClawError::Config)?;
    ensure_home(home)?;
    let client_id = opts.client.client_id();
    let (token, meta) =
        crate::mcp_refresh::issue_refresh_token(home, TOKEN_ENV_LABEL, &client_id, &scopes, true)
            .map_err(|e| DuDuClawError::Config(format!("issue refresh token failed: {e}")))?;
    let launch = launch_for_exe(exe);
    let env = client_env(&token, custom_home);
    Ok(InitPlan {
        client_id,
        jti: meta.jti,
        launch,
        env,
    })
}

/// Create the data directory when missing. In the CLI the global file
/// logger has usually created it (with `logs/`) before this runs, the same
/// way every other command creates it, so this only matters for a caller
/// with no logger. Everything else the server needs is created by the code
/// that uses it: `issue_refresh_token` creates `mcp_tokens.db`, and
/// `duduclaw mcp-server` opens `memory.db` at start. There is no second
/// initialisation path to keep in step with the platform's.
fn ensure_home(home: &Path) -> Result<()> {
    std::fs::create_dir_all(home).map_err(|e| {
        DuDuClawError::Config(format!(
            "cannot create the data directory {}: {e}",
            home.display()
        ))
    })
}

/// Active tokens issued to the same standalone client before this run.
fn earlier_active_jtis(home: &Path, client_id: &str, new_jti: &str) -> Vec<String> {
    let now = chrono::Utc::now();
    crate::mcp_refresh::list_tokens(home)
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.client_id == client_id && t.jti != new_jti)
        .filter(|t| !t.is_revoked() && !t.is_expired(now))
        .map(|t| t.jti)
        .collect()
}

/// Refuse inside an AI employee's session (any marker variable present).
fn refuse_in_ai_session(ai_markers: &[&str]) -> Result<()> {
    if ai_markers.is_empty() {
        return Ok(());
    }
    Err(DuDuClawError::Config(format!(
        "{AI_SESSION_REFUSAL}\nVariables found / 偵測到的變數: {}",
        ai_markers.join(", ")
    )))
}

/// The path this binary was started from, as the client should start it.
///
/// Not canonicalized: a Homebrew `bin/` link would turn into a versioned
/// `Cellar/<version>/` path and an npm shim into a node-version directory,
/// both of which disappear on upgrade. A Windows verbatim prefix (`\\?\`) is
/// dropped because several clients cannot start a program spelled that way.
pub(crate) fn client_exe_path(exe: PathBuf) -> PathBuf {
    let text = exe.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    exe
}

/// `duduclaw mcp init`.
pub(crate) fn run(home: &Path, opts: InitOptions) -> Result<()> {
    let markers = crate::ai_session_guard::markers();
    refuse_in_ai_session(&markers)?;

    let exe = std::env::current_exe()
        .map(client_exe_path)
        .map_err(|e| DuDuClawError::Config(format!("cannot resolve this binary's path: {e}")))?;
    let custom_home = std::env::var("DUDUCLAW_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|_| home.to_path_buf());

    // Claude Code: look at the existing `duduclaw` entry before any key is
    // issued, so a refusal leaves nothing behind.
    let claude = match opts.client {
        InitClient::ClaudeCode => duduclaw_core::which_claude().map(PathBuf::from),
        _ => None,
    };
    let existing = match (&claude, claude_user_config_path()) {
        (Some(_), Some(config)) => read_claude_entry(&config, home),
        _ => ClaudeEntry::Absent,
    };
    if existing.needs_replace_flag() && !opts.replace {
        return Err(DuDuClawError::Config(format!(
            "Claude Code already has a `{SERVER_NAME}` MCP server that `duduclaw mcp init` \
             did not write:\n{}\nNo key was issued. Re-run with --replace to replace it (the \
             old entry is saved first), or remove it yourself with \
             `claude mcp remove {SERVER_NAME} -s user`.",
            existing.describe()
        )));
    }

    let plan = prepare(home, &opts, &exe, custom_home.as_deref(), &markers)?;

    println!();
    println!("DuDuClaw MCP server, standalone profile");
    println!("  data directory : {}", home.display());
    println!("  client id      : {}", plan.client_id);
    println!("  token jti      : {}", plan.jti);
    println!("  scopes         : {}", opts.scopes);
    println!(
        "  expires in     : {} days",
        crate::mcp_refresh::REFRESH_TOKEN_TTL_DAYS
    );
    println!();
    println!("The key below is shown once. It is stored only as a hash.");
    println!();

    match opts.client {
        InitClient::Print => {
            print_claude(&plan);
            print_codex(&plan);
            print_cursor(&plan);
        }
        InitClient::ClaudeCode => {
            print_claude(&plan);
            match claude {
                Some(bin) => register_with_claude(&bin, home, &plan, opts.yes, &existing)?,
                None => println!(
                    "Claude Code CLI not found. Run the command above after installing it."
                ),
            }
        }
        InitClient::Codex => print_codex(&plan),
        InitClient::Cursor => print_cursor(&plan),
    }

    let earlier = earlier_active_jtis(home, &plan.client_id, &plan.jti);
    if !earlier.is_empty() {
        println!();
        println!(
            "Earlier keys for {} are still active. Once the new one works:",
            plan.client_id
        );
        for jti in earlier {
            println!("  duduclaw mcp revoke-token {jti}");
        }
    }
    println!();
    println!(
        "The client keeps this key in plain text in its own configuration file \
         (~/.claude.json, ~/.codex/config.toml, ~/.cursor/mcp.json)."
    );
    println!(
        "Guide: https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/guides/mcp-standalone.md"
    );
    Ok(())
}

fn print_claude(plan: &InitPlan) {
    let style = ShellStyle::native();
    println!("Claude Code:");
    println!("  {}", claude_command_line(&plan.launch, &plan.env, style));
    match style {
        ShellStyle::Posix => {
            println!("  (quoted for bash/zsh; in PowerShell or cmd use double quotes)")
        }
        ShellStyle::Windows => println!("  (quoted for PowerShell/cmd; in bash use single quotes)"),
    }
    println!();
}

fn print_codex(plan: &InitPlan) {
    println!("Codex (add to ~/.codex/config.toml):");
    for line in codex_block(&plan.launch, &plan.env).lines() {
        println!("  {line}");
    }
    println!();
}

fn print_cursor(plan: &InitPlan) {
    println!("Cursor (~/.cursor/mcp.json; merge into an existing \"mcpServers\" by hand):");
    for line in cursor_json(&plan.launch, &plan.env).lines() {
        println!("  {line}");
    }
    println!();
}

// ── The existing Claude Code entry ──────────────────────────────────────────

/// The `duduclaw` entry in Claude Code's user configuration.
#[derive(Debug, Clone)]
pub(crate) enum ClaudeEntry {
    /// No such entry (or no configuration file yet).
    Absent,
    /// Written by `duduclaw mcp init`: its key is a `standalone-*` key of the
    /// data directory the entry names (any state, expired included).
    Standalone {
        client_id: String,
        entry: serde_json::Value,
    },
    /// Anything else: another key, another server under the same name, or a
    /// key this home does not know.
    Other { entry: serde_json::Value },
    /// The file exists but could not be read or parsed.
    Unreadable { path: PathBuf, reason: String },
}

impl ClaudeEntry {
    /// Replacing it needs `--replace`.
    pub(crate) fn needs_replace_flag(&self) -> bool {
        matches!(
            self,
            ClaudeEntry::Other { .. } | ClaudeEntry::Unreadable { .. }
        )
    }

    fn entry(&self) -> Option<&serde_json::Value> {
        match self {
            ClaudeEntry::Standalone { entry, .. } | ClaudeEntry::Other { entry } => Some(entry),
            _ => None,
        }
    }

    /// One indented description with every secret masked.
    pub(crate) fn describe(&self) -> String {
        match self {
            ClaudeEntry::Absent => "  (none)".to_string(),
            ClaudeEntry::Unreadable { path, reason } => {
                format!("  {} could not be read: {reason}", path.display())
            }
            ClaudeEntry::Standalone { entry, client_id } => format!(
                "{}\n  (written by an earlier `duduclaw mcp init`, client id {client_id})",
                describe_entry_masked(entry)
            ),
            ClaudeEntry::Other { entry } => describe_entry_masked(entry),
        }
    }
}

/// Claude Code's user-scope configuration file: `$CLAUDE_CONFIG_DIR/.claude.json`
/// when that variable is set, `~/.claude.json` otherwise (checked against
/// Claude Code 2.1.285, which reports the file it modified).
pub(crate) fn claude_user_config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir).join(".claude.json"));
    }
    dirs::home_dir().map(|h| h.join(".claude.json"))
}

/// Read the user-scope `duduclaw` entry and tell whose it is. `home` is the
/// data directory used when the entry carries no `DUDUCLAW_HOME`.
pub(crate) fn read_claude_entry(config: &Path, home: &Path) -> ClaudeEntry {
    let text = match std::fs::read_to_string(config) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ClaudeEntry::Absent,
        Err(e) => {
            return ClaudeEntry::Unreadable {
                path: config.to_path_buf(),
                reason: e.to_string(),
            };
        }
    };
    let doc: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            return ClaudeEntry::Unreadable {
                path: config.to_path_buf(),
                reason: format!("not valid JSON ({e})"),
            };
        }
    };
    let Some(entry) = doc.get("mcpServers").and_then(|m| m.get(SERVER_NAME)) else {
        return ClaudeEntry::Absent;
    };
    let env = entry.get("env");
    let key = env
        .and_then(|e| e.get("DUDUCLAW_MCP_API_KEY"))
        .and_then(|v| v.as_str());
    let lookup_home = env
        .and_then(|e| e.get("DUDUCLAW_HOME"))
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.to_path_buf());
    let standalone = key
        .and_then(|k| crate::mcp_refresh::find_token_meta(&lookup_home, k))
        .map(|meta| meta.client_id)
        .filter(|id| id.starts_with(duduclaw_core::STANDALONE_CLIENT_PREFIX));
    match standalone {
        Some(client_id) => ClaudeEntry::Standalone {
            client_id,
            entry: entry.clone(),
        },
        None => ClaudeEntry::Other {
            entry: entry.clone(),
        },
    }
}

/// A secret shown as its first four characters and its length.
pub(crate) fn mask_secret(value: &str) -> String {
    let len = value.chars().count();
    if len <= 8 {
        return format!("… ({len} chars)");
    }
    format!("{}… ({len} chars)", duduclaw_core::truncate_chars(value, 4))
}

/// Command, arguments and environment of an entry; every environment value
/// except `DUDUCLAW_HOME` (a path) is masked.
pub(crate) fn describe_entry_masked(entry: &serde_json::Value) -> String {
    let command = entry.get("command").and_then(|v| v.as_str()).unwrap_or("?");
    let args = entry
        .get("args")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|x| {
                    x.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| x.to_string())
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let mut out = format!("  command: {command} {args}")
        .trim_end()
        .to_string();
    if let Some(url) = entry.get("url").and_then(|v| v.as_str()) {
        out.push_str(&format!("\n  url: {url}"));
    }
    if let Some(env) = entry.get("env").and_then(|v| v.as_object()) {
        for (k, v) in env {
            let shown = match v.as_str() {
                Some(path) if k == "DUDUCLAW_HOME" => path.to_string(),
                Some(s) => mask_secret(s),
                None => "(not a string)".to_string(),
            };
            out.push_str(&format!("\n  env {k}={shown}"));
        }
    }
    out
}

/// Save the entry about to be replaced, readable by the owner only, and
/// return the file. `claude mcp add-json duduclaw "$(cat <file>)" -s user`
/// puts it back.
fn backup_claude_entry(home: &Path, entry: &serde_json::Value) -> Result<PathBuf> {
    let dir = home.join("mcp_init");
    std::fs::create_dir_all(&dir)
        .map_err(|e| DuDuClawError::Config(format!("cannot create {}: {e}", dir.display())))?;
    let path = dir.join(format!(
        "claude-{SERVER_NAME}-{}.json",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
    ));
    let body = serde_json::to_string_pretty(entry).unwrap_or_default();
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    let mut file = open
        .open(&path)
        .map_err(|e| DuDuClawError::Config(format!("cannot write {}: {e}", path.display())))?;
    use std::io::Write;
    file.write_all(body.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| DuDuClawError::Config(format!("cannot write {}: {e}", path.display())))?;
    Ok(path)
}

/// Run `claude mcp add` when the operator agreed. An existing `duduclaw`
/// entry is saved to a file first, then removed; if the add fails the saved
/// entry is put back.
fn register_with_claude(
    claude: &Path,
    home: &Path,
    plan: &InitPlan,
    yes: bool,
    existing: &ClaudeEntry,
) -> Result<()> {
    if !yes {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            println!("Not a terminal: re-run with --yes to register it, or run the command above.");
            return Ok(());
        }
        let prompt = match existing {
            ClaudeEntry::Absent => {
                format!("Register `{SERVER_NAME}` in Claude Code (user scope) now?")
            }
            _ => format!(
                "Register `{SERVER_NAME}` in Claude Code (user scope) now? It replaces:\n{}\n",
                existing.describe()
            ),
        };
        let go = dialoguer::Confirm::new()
            .with_prompt(prompt)
            .default(true)
            .interact()
            .unwrap_or(false);
        if !go {
            println!("Not registered. Run the command above when ready.");
            return Ok(());
        }
    }

    let backup = match existing.entry() {
        Some(entry) => Some((backup_claude_entry(home, entry)?, entry.clone())),
        None => None,
    };
    if !matches!(existing, ClaudeEntry::Absent) {
        let removed = std::process::Command::new(claude)
            .args(["mcp", "remove", SERVER_NAME, "-s", "user"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        // An unreadable file has nothing to remove for sure; any other
        // failure leaves the old entry in place, so stop before adding.
        if !matches!(existing, ClaudeEntry::Unreadable { .. })
            && !removed.as_ref().is_ok_and(|s| s.success())
        {
            return Err(DuDuClawError::Config(format!(
                "`claude mcp remove {SERVER_NAME} -s user` failed; the old entry was not changed"
            )));
        }
    }

    let added = std::process::Command::new(claude)
        .args(claude_add_args(&plan.launch, &plan.env))
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(|e| DuDuClawError::Config(format!("cannot run {}: {e}", claude.display())));
    let failure = match added {
        Ok(status) if status.success() => None,
        Ok(status) => Some(format!("`claude mcp add` failed ({status})")),
        Err(e) => Some(e.to_string()),
    };
    let Some(failure) = failure else {
        if let Some((path, _)) = &backup {
            println!("Replaced the earlier entry (saved to {}).", path.display());
        }
        println!(
            "Registered. Start a new Claude Code session and run /mcp to see `{SERVER_NAME}`."
        );
        return Ok(());
    };

    let Some((path, entry)) = backup else {
        return Err(DuDuClawError::Config(format!(
            "{failure}; run the command above by hand"
        )));
    };
    let restored = std::process::Command::new(claude)
        .args(["mcp", "add-json", SERVER_NAME])
        .arg(entry.to_string())
        .args(["-s", "user"])
        .stdout(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let what = if restored {
        "The earlier entry was put back".to_string()
    } else {
        format!(
            "Putting the earlier entry back failed too; restore it with:\n  \
             claude mcp add-json {SERVER_NAME} \"$(cat '{}')\" -s user",
            path.display()
        )
    };
    Err(DuDuClawError::Config(format!(
        "{failure}. {what} (saved copy: {}). Run the command above by hand.",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(client: InitClient) -> InitOptions {
        InitOptions {
            client,
            scopes: STANDALONE_SCOPES.into(),
            yes: true,
            replace: false,
        }
    }

    #[test]
    fn client_ids_are_prefixed() {
        assert_eq!(InitClient::ClaudeCode.client_id(), "standalone-claude-code");
        assert_eq!(InitClient::Codex.client_id(), "standalone-codex");
        assert_eq!(InitClient::Cursor.client_id(), "standalone-cursor");
        assert_eq!(InitClient::Print.client_id(), "standalone-print");
        // No employee may be created under any of them.
        for c in [
            InitClient::ClaudeCode,
            InitClient::Codex,
            InitClient::Cursor,
            InitClient::Print,
        ] {
            assert!(duduclaw_core::is_reserved_agent_id(&c.client_id()), "{c:?}");
        }
    }

    #[test]
    fn clap_parses_the_client_values_and_defaults() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[arg(long, value_enum, default_value = "print")]
            client: InitClient,
            #[arg(long, default_value = STANDALONE_SCOPES)]
            scopes: String,
            #[arg(long)]
            yes: bool,
        }
        let c = Cli::try_parse_from(["x"]).unwrap();
        assert_eq!(c.client, InitClient::Print);
        assert_eq!(c.scopes, STANDALONE_SCOPES);
        assert!(!c.yes);
        for (v, want) in [
            ("claude-code", InitClient::ClaudeCode),
            ("codex", InitClient::Codex),
            ("cursor", InitClient::Cursor),
            ("print", InitClient::Print),
        ] {
            let c = Cli::try_parse_from(["x", "--client", v, "--yes"]).unwrap();
            assert_eq!(c.client, want);
            assert!(c.yes);
        }
        assert!(Cli::try_parse_from(["x", "--client", "vscode"]).is_err());
    }

    #[test]
    fn scopes_must_be_known_non_empty_and_grantable() {
        assert_eq!(validate_scopes(STANDALONE_SCOPES).unwrap().len(), 4);
        assert!(validate_scopes("").unwrap_err().contains("empty"));
        assert!(
            validate_scopes("memory:read,bogus")
                .unwrap_err()
                .contains("invalid")
        );
        let err = validate_scopes("memory:read,admin").unwrap_err();
        assert!(err.contains("admin"), "{err}");
        let err = validate_scopes("files:read,identity:read").unwrap_err();
        assert!(err.contains("files:read,identity:read"), "{err}");
        assert!(validate_scopes("messaging:send").is_ok());
    }

    #[test]
    fn refuses_inside_an_ai_session_and_issues_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".duduclaw");
        let err = prepare(
            &home,
            &opts(InitClient::Print),
            Path::new("/usr/local/bin/duduclaw"),
            None,
            &["DUDUCLAW_AGENT_ID"],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("DUDUCLAW_AGENT_ID"), "{err}");
        // English first, and it says what to do in one's own terminal.
        let en = err.find("duduclaw mcp init refused").expect("English text");
        assert!(
            en < err.find("只能由操作者").expect("Chinese text"),
            "{err}"
        );
        assert!(err.contains("unset"), "{err}");
        assert!(!home.exists(), "nothing may be created on refusal");
    }

    #[test]
    fn prepare_creates_home_and_issues_an_external_token() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("nested").join(".duduclaw");
        let plan = prepare(
            &home,
            &opts(InitClient::ClaudeCode),
            Path::new("/opt/bin/duduclaw"),
            None,
            &[],
        )
        .unwrap();
        assert!(home.is_dir());
        assert!(home.join("mcp_tokens.db").is_file());
        assert_eq!(plan.client_id, "standalone-claude-code");
        let token = plan.env[0].1.clone();
        let p = crate::mcp_refresh::authenticate_with_refresh_token(&token, &home).unwrap();
        assert!(p.is_external);
        assert_eq!(p.client_id, "standalone-claude-code");
        assert_eq!(p.scopes, validate_scopes(STANDALONE_SCOPES).unwrap());
        assert_eq!(plan.env, vec![("DUDUCLAW_MCP_API_KEY".into(), token)]);
        assert_eq!(plan.launch.args, vec!["mcp-server".to_string()]);
    }

    #[test]
    fn npx_cache_binary_launches_through_npx() {
        let l = launch_for_exe(Path::new(
            "/Users/u/.npm/_npx/abc123/node_modules/@duduclaw/darwin-arm64/bin/duduclaw",
        ));
        assert_eq!(l.command, "npx");
        let pinned = format!("duduclaw@{}", env!("CARGO_PKG_VERSION"));
        assert_eq!(l.args, vec!["-y", pinned.as_str(), "mcp-server"]);
        let l = launch_for_exe(Path::new("/usr/local/lib/node_modules/x_npx/bin/duduclaw"));
        assert_eq!(l.command, "/usr/local/lib/node_modules/x_npx/bin/duduclaw");
    }

    #[test]
    fn client_snippets_carry_command_args_and_env() {
        let launch = Launch {
            command: "/opt/my tools/duduclaw".into(),
            args: vec!["mcp-server".into()],
        };
        let env = client_env("ddc_refresh_prod_ab", Some(Path::new("/data/dd")));
        assert_eq!(
            claude_add_args(&launch, &env),
            vec![
                "mcp",
                "add",
                "duduclaw",
                "-s",
                "user",
                "-e",
                "DUDUCLAW_MCP_API_KEY=ddc_refresh_prod_ab",
                "-e",
                "DUDUCLAW_HOME=/data/dd",
                "--",
                "/opt/my tools/duduclaw",
                "mcp-server",
            ]
        );
        assert_eq!(
            claude_command_line(&launch, &env[..1], ShellStyle::Posix),
            "claude mcp add duduclaw -s user -e DUDUCLAW_MCP_API_KEY=ddc_refresh_prod_ab -- \
             '/opt/my tools/duduclaw' mcp-server"
        );

        let codex = codex_block(&launch, &env);
        let parsed: toml::Value = toml::from_str(&codex).unwrap();
        let s = &parsed["mcp_servers"]["duduclaw"];
        assert_eq!(s["command"].as_str(), Some("/opt/my tools/duduclaw"));
        assert_eq!(s["args"][0].as_str(), Some("mcp-server"));
        assert_eq!(
            s["env"]["DUDUCLAW_MCP_API_KEY"].as_str(),
            Some("ddc_refresh_prod_ab")
        );
        assert_eq!(s["env"]["DUDUCLAW_HOME"].as_str(), Some("/data/dd"));

        let cursor: serde_json::Value = serde_json::from_str(&cursor_json(&launch, &env)).unwrap();
        let s = &cursor["mcpServers"]["duduclaw"];
        assert_eq!(s["command"], "/opt/my tools/duduclaw");
        assert_eq!(s["args"][0], "mcp-server");
        assert_eq!(s["env"]["DUDUCLAW_MCP_API_KEY"], "ddc_refresh_prod_ab");
    }

    #[test]
    fn windows_command_line_uses_double_quotes() {
        let launch = Launch {
            command: r"C:\Program Files\duduclaw\duduclaw.exe".into(),
            args: vec!["mcp-server".into()],
        };
        let env = client_env("ddc_refresh_prod_ab", None);
        assert_eq!(
            claude_command_line(&launch, &env, ShellStyle::Windows),
            "claude mcp add duduclaw -s user -e DUDUCLAW_MCP_API_KEY=ddc_refresh_prod_ab -- \
             \"C:\\Program Files\\duduclaw\\duduclaw.exe\" mcp-server"
        );
    }

    #[test]
    fn client_exe_path_keeps_links_and_drops_the_verbatim_prefix() {
        // Not canonicalized: a link path stays as given.
        let p = PathBuf::from("/opt/homebrew/bin/duduclaw");
        assert_eq!(client_exe_path(p.clone()), p);
        assert_eq!(
            client_exe_path(PathBuf::from(r"\\?\C:\Tools\duduclaw.exe")),
            PathBuf::from(r"C:\Tools\duduclaw.exe")
        );
        assert_eq!(
            client_exe_path(PathBuf::from(r"\\?\UNC\server\share\duduclaw.exe")),
            PathBuf::from(r"\\server\share\duduclaw.exe")
        );
    }

    fn write_claude_config(dir: &Path, entry: serde_json::Value) -> PathBuf {
        let path = dir.join(".claude.json");
        let doc = serde_json::json!({ "userID": "x", "mcpServers": { "duduclaw": entry } });
        std::fs::write(&path, doc.to_string()).unwrap();
        path
    }

    #[test]
    fn existing_claude_entry_is_classified_by_its_key() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".duduclaw");

        // No file, or no entry: nothing to replace.
        assert!(matches!(
            read_claude_entry(&tmp.path().join("missing.json"), &home),
            ClaudeEntry::Absent
        ));
        let empty = tmp.path().join("empty.json");
        std::fs::write(&empty, r#"{"mcpServers":{"other":{"command":"x"}}}"#).unwrap();
        assert!(matches!(
            read_claude_entry(&empty, &home),
            ClaudeEntry::Absent
        ));

        // A key `mcp init` issued in this home (expired or not): replaced freely.
        let plan = prepare(
            &home,
            &opts(InitClient::ClaudeCode),
            Path::new("/b/duduclaw"),
            None,
            &[],
        )
        .unwrap();
        let token = plan.env[0].1.clone();
        let cfg = write_claude_config(
            tmp.path(),
            serde_json::json!({"command": "/b/duduclaw", "args": ["mcp-server"],
                               "env": {"DUDUCLAW_MCP_API_KEY": token}}),
        );
        let e = read_claude_entry(&cfg, &home);
        assert!(
            matches!(&e, ClaudeEntry::Standalone { client_id, .. }
            if client_id == "standalone-claude-code"),
            "{e:?}"
        );
        assert!(!e.needs_replace_flag());

        // A key of another client id, an unknown key, another server: --replace.
        let (other_key, _) = crate::mcp_refresh::issue_refresh_token(
            &home,
            "prod",
            "ci-bot",
            &validate_scopes("memory:read").unwrap(),
            true,
        )
        .unwrap();
        for entry in [
            serde_json::json!({"command": "/b/duduclaw", "env": {"DUDUCLAW_MCP_API_KEY": other_key}}),
            serde_json::json!({"command": "/b/duduclaw", "env": {"DUDUCLAW_MCP_API_KEY":
                format!("ddc_refresh_prod_{}", "a".repeat(64))}}),
            serde_json::json!({"command": "/b/duduclaw", "env": {"DUDUCLAW_MCP_API_KEY": "sk-legacy"}}),
            serde_json::json!({"type": "http", "url": "https://example.com/mcp"}),
        ] {
            let cfg = write_claude_config(tmp.path(), entry.clone());
            let e = read_claude_entry(&cfg, &home);
            assert!(matches!(e, ClaudeEntry::Other { .. }), "{entry}: {e:?}");
            assert!(e.needs_replace_flag());
        }

        // An entry naming another data directory is looked up there.
        let other_home = tmp.path().join("other-home");
        let plan2 = prepare(
            &other_home,
            &opts(InitClient::Codex),
            Path::new("/b/d"),
            None,
            &[],
        )
        .unwrap();
        let cfg = write_claude_config(
            tmp.path(),
            serde_json::json!({"command": "/b/d", "env": {
                "DUDUCLAW_MCP_API_KEY": plan2.env[0].1,
                "DUDUCLAW_HOME": other_home.to_string_lossy()}}),
        );
        assert!(matches!(
            read_claude_entry(&cfg, &home),
            ClaudeEntry::Standalone { .. }
        ));

        // Unreadable JSON: --replace, and nothing is guessed.
        std::fs::write(&cfg, "{not json").unwrap();
        let e = read_claude_entry(&cfg, &home);
        assert!(matches!(e, ClaudeEntry::Unreadable { .. }), "{e:?}");
        assert!(e.needs_replace_flag());
    }

    #[test]
    fn classifying_an_entry_creates_no_token_database() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".duduclaw");
        let cfg = write_claude_config(
            tmp.path(),
            serde_json::json!({"command": "x", "env": {"DUDUCLAW_MCP_API_KEY":
                format!("ddc_refresh_prod_{}", "b".repeat(64))}}),
        );
        assert!(matches!(
            read_claude_entry(&cfg, &home),
            ClaudeEntry::Other { .. }
        ));
        assert!(!home.join("mcp_tokens.db").exists());
    }

    #[test]
    fn description_masks_every_secret() {
        let key = format!("ddc_refresh_prod_{}", "c".repeat(64));
        let entry = serde_json::json!({
            "command": "/b/duduclaw", "args": ["mcp-server"],
            "env": {"DUDUCLAW_MCP_API_KEY": key, "DUDUCLAW_HOME": "/data/dd", "TOKEN": "s3cr3t-value"}
        });
        let d = describe_entry_masked(&entry);
        assert!(d.contains("command: /b/duduclaw mcp-server"), "{d}");
        assert!(d.contains("DUDUCLAW_HOME=/data/dd"), "{d}");
        assert!(!d.contains(&key) && !d.contains("ccccc"), "{d}");
        assert!(d.contains("DUDUCLAW_MCP_API_KEY=ddc_… (81 chars)"), "{d}");
        assert!(!d.contains("s3cr3t"), "{d}");
        assert_eq!(mask_secret("short"), "… (5 chars)");
        assert_eq!(mask_secret("金鑰金鑰金鑰金鑰金鑰"), "金鑰金鑰… (10 chars)");
    }

    #[test]
    fn backup_is_owner_only_and_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = serde_json::json!({"command": "x", "env": {"K": "v"}});
        let path = backup_claude_entry(tmp.path(), &entry).unwrap();
        let back: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back, entry);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
