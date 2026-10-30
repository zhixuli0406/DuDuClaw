//! Discovery-specific runtime contracts, separate from interactive runtimes.
//! Evidence: local `claude --help`, `codex exec --help`, `grok --help`;
//! real event captures in commercial/research/release-live-validation-2026-10-01-evidence/runtime-parity;
//! https://code.claude.com/docs/en/cli-reference
//! https://developers.openai.com/codex/config-reference
//! Design: commercial/docs/specs/DESIGN-discovery-runtime-parity-2026-10.md (§2, §5, §6, §7).
use std::{collections::BTreeMap, path::Path};
use serde_json::{Value, json};
use super::contracts::AttemptInfraError;
use super::attempt_guard::{GROK_DISALLOWED_TOOLS, GROK_TOOLS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFamily {
    Claude,
    Codex,
    Grok,
    OpenAiCompat,
}
impl RuntimeFamily {
    pub fn parse(name: &str) -> Result<Self, AttemptInfraError> {
        if let Some(removed) = duduclaw_core::runtime_catalog::removed_runtime(name) {
            return Err(AttemptInfraError::RuntimeUnsupported(removed.refusal("discovery attempt runtime")));
        }
        match name {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "openai-compat" | "openai_compat" => Ok(Self::OpenAiCompat),
            "grok" => Ok(Self::Grok),
            _ => Err(AttemptInfraError::RuntimeUnsupported(name.into())),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::OpenAiCompat => "openai-compat",
        }
    }
    pub fn provider(self) -> &'static str {
        match self {
            Self::Claude => "anthropic",
            Self::Codex | Self::OpenAiCompat => "openai",
            Self::Grok => "xai",
        }
    }
    /// True when no native step ceiling exists and the host counts steps.
    pub fn host_counted_steps(self) -> bool { matches!(self, Self::Codex) }
    pub fn argv(self, model: &str, turns: u32, node: &Path) -> Vec<String> {
        match self {
            Self::Claude => {
                let mut argv=super::agent_spawn::build_claude_argv(model,turns,Path::new("/dudu-runtime/empty-mcp.json"));
                argv.retain(|arg|arg!="--restricted"); argv.push("--safe-mode".into()); argv
            }
            Self::Codex => {
                let mut argv=vec!["exec".into(),"--json".into(),"--strict-config".into(),
                    "--ignore-user-config".into(),"--ignore-rules".into(),"--ephemeral".into(),
                    "--skip-git-repo-check".into(),"--dangerously-bypass-approvals-and-sandbox".into(),
                    "--model".into(),model.into(),"--cd".into(),node.to_string_lossy().into_owned()];
                // Docker is the external hard sandbox named in exec --help.
                // No --add-dir (that flag grants writable directories).
                for setting in ["approval_policy=\"never\"","mcp_servers={}","web_search=\"disabled\"",
                    "features.multi_agent=false","features.hooks=false","features.plugins=false",
                    "features.memories=false","project_doc_max_bytes=0","features.apps=false",
                    "check_for_update_on_startup=false","analytics.enabled=false"] {
                    argv.extend(["-c".into(),setting.into()]);
                }
                argv.push("-".into()); argv
            }
            // Headless grok does not read stdin; no --trust, so workspace hooks/settings stay unloaded.
            Self::Grok => vec!["--prompt-file".into(),"/dudu-runtime/prompt.txt".into(),
                "--output-format".into(),"streaming-messages-json".into(),"--cwd".into(),node.to_string_lossy().into_owned(),
                "-m".into(),model.into(),"--max-turns".into(),turns.to_string(),
                "--permission-mode".into(),"bypassPermissions".into(),"--tools".into(),GROK_TOOLS.join(","),
                "--disallowed-tools".into(),GROK_DISALLOWED_TOOLS.join(","),
                "--disable-web-search".into(),"--no-subagents".into(),"--no-plan".into(),"--no-auto-update".into()],
            Self::OpenAiCompat => vec!["-I".into(),"-S".into(),"-B".into(),"-c".into(),
                include_str!("python/attempt_openai_compat.py").into(), model.into(),turns.to_string()],
        }
    }
}

/// Fixed in-container paths. HOME is a private tmpfs directory.
pub const PRIVATE_HOME: &str = "/tmp/dudu-private/home";

/// Host-written files for the read-only `/dudu-runtime` directory, beyond
/// `empty-mcp.json`. Paths are relative to that directory.
pub fn runtime_files(family: RuntimeFamily, prompt: &str) -> Vec<(String, Vec<u8>)> {
    match family {
        RuntimeFamily::Grok => vec![("prompt.txt".into(), prompt.as_bytes().to_vec())],
        _ => vec![],
    }
}


/// Bytes written to the CLI's stdin. Grok reads `--prompt-file` instead.
pub fn stdin_payload(family: RuntimeFamily, prompt: &str) -> Vec<u8> {
    match family {
        RuntimeFamily::Grok => Vec::new(),
        _ => prompt.as_bytes().to_vec(),
    }
}

fn credential_keys(family: RuntimeFamily) -> &'static [&'static str] {
    match family {
        RuntimeFamily::Claude => &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"],
        RuntimeFamily::Codex | RuntimeFamily::OpenAiCompat => &["OPENAI_API_KEY"],
        RuntimeFamily::Grok => &["XAI_API_KEY"],
    }
}

/// Only selected credentials enter the image; HOME/config paths are fresh.
pub fn environment(
    family: RuntimeFamily,
    selected: &std::collections::HashMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::from([
        ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
        ("HOME".into(), PRIVATE_HOME.into()),
        ("TMPDIR".into(), "/tmp/dudu-private/tmp".into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("TZ".into(), "UTC".into()),
        ("DISABLE_AUTOUPDATER".into(), "1".into()),
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        (
            "CLAUDE_CONFIG_DIR".into(),
            "/tmp/dudu-private/home/.claude".into(),
        ),
        ("CLAUDE_CODE_TMPDIR".into(), "/tmp/dudu-private/tmp".into()),
        ("CODEX_HOME".into(), "/tmp/dudu-private/home/.codex".into()),
    ]);
    if family==RuntimeFamily::Grok {
        for (key,value) in [("GROK_HOME","/tmp/dudu-private/home/.grok"),("GROK_DISABLE_AUTOUPDATER","1"),
            ("GROK_SUBAGENTS","0"),("GROK_MEMORY","0")] { out.insert(key.into(),value.into()); }
    }
    for key in credential_keys(family) {
        if let Some(value)=selected.get(*key).filter(|v|!v.is_empty()) { out.insert((*key).into(),value.clone()); }
    }
    // codex exec reads CODEX_API_KEY first; both carry the one selected key.
    if family==RuntimeFamily::Codex {
        if let Some(value)=out.get("OPENAI_API_KEY").cloned() { out.insert("CODEX_API_KEY".into(),value); }
    }
    out
}

/// Environment variables whose value is a secret: every family's credential
/// keys, the derived `CODEX_API_KEY` and the credential document. They reach
/// the container by name only (never as argv text).
pub fn is_secret_env(key: &str) -> bool {
    key == "CODEX_API_KEY"
        || key == CREDENTIAL_DOC_ENV
        || [
            RuntimeFamily::Claude,
            RuntimeFamily::Codex,
            RuntimeFamily::Grok,
            RuntimeFamily::OpenAiCompat,
        ]
        .into_iter()
        .any(|family| credential_keys(family).contains(&key))
}

/// Environment names of the credential document handed to the trusted PID 1.
pub const CREDENTIAL_DOC_ENV: &str = "DUDU_CREDENTIAL_DOC";
pub const CREDENTIAL_DEST_ENV: &str = "DUDU_CREDENTIAL_DEST";
pub const CREDENTIAL_DOC_MAX_BYTES: usize = 64 * 1024;

/// HOME-relative destination of a subscription login document. Host-fixed.
pub fn credential_destination(family: RuntimeFamily) -> Option<&'static str> {
    match family { RuntimeFamily::Codex=>Some(".codex/auth.json"), RuntimeFamily::Grok=>Some(".grok/auth.json"), _=>None }
}

/// The selected OAuth account's seat credential is the CLI's `auth.json`.
/// It must be a JSON object of at most 64 KiB; it is re-serialised to one line.
/// Families without a document path ignore the seat. Invalid ⇒ `NoAccount`.
/// The value is never logged and the error never carries it.
pub fn credential_document(family: RuntimeFamily, seat_token: Option<&str>) -> Result<Option<String>, AttemptInfraError> {
    let (Some(_), Some(seat)) = (credential_destination(family), seat_token.filter(|s|!s.trim().is_empty())) else { return Ok(None) };
    if seat.len() > CREDENTIAL_DOC_MAX_BYTES { return Err(AttemptInfraError::NoAccount); }
    match serde_json::from_str::<Value>(seat) {
        Ok(Value::Object(fields)) if !fields.is_empty() => {
            let line = Value::Object(fields).to_string();
            if line.len() > CREDENTIAL_DOC_MAX_BYTES { return Err(AttemptInfraError::NoAccount); }
            Ok(Some(line))
        }
        _ => Err(AttemptInfraError::NoAccount),
    }
}

/// Model ids accepted from a provider stream: `[A-Za-z0-9._:-]{1,64}`.
pub fn observed_model_id(value: &str) -> Option<&str> {
    let ok = !value.is_empty() && value.len() <= 64 && value != "unknown"
        && value.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.'|b'_'|b':'|b'-'));
    ok.then_some(value)
}

/// Native protocol translated into the runner's common result/usage envelope.
/// No model id is invented when the provider stream omits it.
#[derive(Default)]
pub struct StreamAdapter {
    text: String,
    model: Option<String>,
}
impl StreamAdapter {
    /// The model the provider stream reported (Grok), filtered.
    pub fn observed_model(&self) -> Option<&str> { self.model.as_deref() }
    /// Codex reports usage only per turn, so a host step limit has unknown usage.
    pub fn synthetic_result(&self) -> Value {
        json!({"type":"result","subtype":"step_limit","result":self.text,"usage":null,"is_error":false})
    }
    pub fn normalize(&mut self, family:RuntimeFamily, event:Value) -> Value {
        match family {
            RuntimeFamily::Claude|RuntimeFamily::OpenAiCompat=>event,
            RuntimeFamily::Grok=>{
                // Same envelope as Claude stream-json; only the model is filtered.
                let mut event=event;
                if event["type"].as_str()==Some("assistant") {
                    if let Some(message)=event.get_mut("message").and_then(Value::as_object_mut) {
                        match message.get("model").and_then(Value::as_str).and_then(observed_model_id).map(str::to_owned) {
                            Some(model)=>{self.model=Some(model.clone());message.insert("model".into(),Value::String(model));}
                            None=>{message.remove("model");}
                        }
                    }
                }
                event
            }
            RuntimeFamily::Codex=>match event["type"].as_str() {
                Some("item.completed") if event["item"]["type"].as_str()==Some("agent_message") => {
                    self.text=event["item"]["text"].as_str().unwrap_or("").into();
                    json!({"type":"assistant","message":{"content":[{"type":"text","text":self.text}]}})
                }
                Some("item.started")|Some("item.completed") if matches!(event["item"]["type"].as_str(),Some("command_execution"|"file_change")) => json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":event["item"]["type"]}]}}),
                Some("turn.completed") => {
                    let usage=&event["usage"];
                    let cached=usage["cached_input_tokens"].as_u64().unwrap_or(0);
                    let usage=usage["input_tokens"].as_u64().zip(usage["output_tokens"].as_u64())
                        .filter(|(input,_)|cached<=*input)
                        .map(|(input,output)|json!({"input_tokens":input-cached,"output_tokens":output,"cache_read_input_tokens":cached}));
                    json!({"type":"result","result":self.text,"usage":usage,"is_error":false})
                }
                Some("turn.failed")|Some("error")=>json!({"type":"result","is_error":true,"error":event["error"],"errors":event["message"]}),
                _=>json!({"type":"system"}),
            },
        }
    }
}

/// An attempt has credentials when its family's key reached the environment,
/// or (Codex, Grok) a validated login document is delivered.
pub fn has_credentials(family:RuntimeFamily,env:&BTreeMap<String,String>,doc:Option<&str>)->bool {
    credential_keys(family).iter().any(|key|env.get(*key).is_some_and(|v|!v.is_empty()))
        || (credential_destination(family).is_some() && doc.is_some_and(|d|!d.is_empty()))
}

/// No userinfo/redirect credentials; unencrypted transport is local probes only.
pub fn compatible_endpoint(value:&str)->Result<String,AttemptInfraError> {
    let url=reqwest::Url::parse(value).map_err(|_|AttemptInfraError::IsolationUnavailable)?;
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some()
        || !(url.scheme()=="https" || (url.scheme()=="http" && matches!(url.host_str(),Some("localhost"|"127.0.0.1"|"[::1]")))) {
        return Err(AttemptInfraError::IsolationUnavailable);
    }
    Ok(value.trim_end_matches('/').into())
}
#[cfg(test)]
#[path = "tests_attempt_adapter.rs"]
mod tests;
