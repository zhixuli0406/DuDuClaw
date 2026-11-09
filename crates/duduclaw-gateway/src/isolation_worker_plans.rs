//! Production E plans for real web, skill and media files. Policy, credentials
//! and publication stay on the host; unsupported lifecycles remain unavailable.
use std::{fs::File, io::Write, path::PathBuf, sync::Arc};
use duduclaw_isolation::{bounded::{List, Text}, ids::{EmployeeId, RequestId}, worker::{ContentFormat,
    HtmlOutputFormat, HtmlQuery, InputFd, ParseOperation, ParseOptions, ParseRequest,
    ParseResult, ParsedItem, SkillAction, WorkerLimits, MAX_RESULT_TEXT_BYTES, WORKER_PROTOCOL_VERSION}};
use duduclaw_mcp_boundary::{VerificationError, execution::PolicyFuture,
    resources::{AuthorizedFile, HostSnapshotSource},
    tool_policy::AuthorizedToolCall, worker_execution::{WorkerPlan, WorkerPlanFactory, WorkerReply}};
use serde_json::{Value, json};
use base64::Engine;
use sha2::{Digest, Sha256};

#[cfg(target_os = "linux")]
#[path = "isolation_skill_publication.rs"]
mod skill_publication;

const BODY_CAP_CHARS: usize = 60_000;

pub struct GatewayWorkerPlans {
    home: PathBuf,
    employee: Option<(String, EmployeeId)>,
    roots: Option<Arc<duduclaw_mcp_boundary::resources::ResourceRoots>>,
}
impl GatewayWorkerPlans {
    pub fn new(home: PathBuf) -> Result<Self, VerificationError> {
        if !home.is_absolute() { return Err(VerificationError::Invalid); }
        #[cfg(target_os = "linux")]
        skill_publication::recover(&home)?;
        Ok(Self { home, employee: None, roots: None })
    }

    pub fn for_employee(home: PathBuf, agent: String, employee: EmployeeId) -> Result<Self, VerificationError> {
        if !duduclaw_core::is_valid_agent_id(&agent) || duduclaw_core::is_reserved_agent_id(&agent) {
            return Err(VerificationError::Invalid);
        }
        let mut factory = Self::new(home)?;
        factory.employee = Some((agent, employee));
        Ok(factory)
    }
    pub fn with_roots(mut self, roots: Arc<duduclaw_mcp_boundary::resources::ResourceRoots>) -> Self {
        self.roots = Some(roots); self
    }
    /// H12a: the subject employee is the boot-bound `(agent, employee)` of this
    /// producer, checked against the verified capability record — never an
    /// argument. The legacy `agent_id` self-reference must equal it; any other
    /// selector (`target_agent`, scope, path, identity keys) refuses.
    fn skill_subject<'a>(&'a self, call: &AuthorizedToolCall) -> Result<(&'a str, String), VerificationError> {
        let (agent, employee) = self.employee.as_ref().ok_or(VerificationError::Unavailable)?;
        if call.context().record().employee != *employee { return Err(VerificationError::BindingMismatch); }
        duduclaw_mcp_boundary::tool_policy::check_own_skill_arguments(call.name(), call.arguments())?;
        duduclaw_mcp_boundary::tool_policy::own_skill_selector(call.context(), call.name())?;
        let args = call.arguments().as_object().ok_or(VerificationError::Invalid)?;
        if args.get("agent_id").is_some_and(|v| v.as_str() != Some(agent.as_str())) {
            return Err(VerificationError::BindingMismatch);
        }
        let name = args.get("skill_name").and_then(Value::as_str).filter(|name| !name.is_empty() && name.len() <= 64
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
            .ok_or(VerificationError::Invalid)?;
        Ok((agent, name.to_owned()))
    }
    async fn prepare_skill_scan(&self, call: &AuthorizedToolCall) -> Result<(WorkerPlan, Value), VerificationError> {
        let (agent, name) = self.skill_subject(call)?;
        let (home, agent, tool, call) = (self.home.clone(), agent.to_owned(), call.name(), call.clone());
        tokio::task::spawn_blocking(move || {
            call.context().verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
            let local = ["agents", agent.as_str(), "SKILLS"];
            let filename = format!("{name}.md");
            let (source, bytes) = if tool == "skill_extract" {
                let nested = ["agents", agent.as_str(), "SKILLS", name.as_str()];
                let bytes = match read_host_file(&home, &nested, "SKILL.md")? {
                    Some(bytes) => bytes,
                    None => match read_host_file(&home, &local, &filename)? {
                        Some(bytes) => bytes,
                        None => read_host_file(&home, &local, &name)?.ok_or(VerificationError::Unavailable)?,
                    },
                };
                ("local", bytes)
            } else if tool == "shared_skill_adopt" {
                ("shared", read_host_file(&home, &["shared", "skills"], &filename)?.ok_or(VerificationError::Unavailable)?)
            } else {
                match read_host_file(&home, &local, &filename)? {
                    Some(bytes) => ("local", bytes),
                    None if tool == "skill_security_scan" => ("global", read_host_file(&home, &["skills"], &filename)?.ok_or(VerificationError::Unavailable)?),
                    None => return Err(VerificationError::Unavailable),
                }
            };
            let patterns = match read_host_file(&home, &["agents", agent.as_str()], "CONTRACT.toml")? {
                None => vec![],
                Some(bytes) => contract_patterns(&bytes)?,
            };
            let must_not = List::new(patterns).map_err(|_| VerificationError::Invalid)?;
            let (operation, options) = if tool == "skill_extract" {
                (ParseOperation::ExtractSkill, ParseOptions::SkillKnowledge {
                    name: Text::new(name.clone()).map_err(|_| VerificationError::Invalid)?,
                    agent: Text::new(agent.clone()).map_err(|_| VerificationError::Invalid)?,
                    at: Text::new(chrono::Utc::now().to_rfc3339()).map_err(|_| VerificationError::Invalid)? })
            } else if tool == "skill_security_scan" {
                (ParseOperation::ScanSkill, ParseOptions::SkillScan { must_not })
            } else {
                let action = if tool == "shared_skill_adopt" { SkillAction::Adopt } else { SkillAction::Graduate };
                (ParseOperation::PrepareSkill, ParseOptions::SkillPrepare { action,
                    agent: Text::new(agent.clone()).map_err(|_| VerificationError::Invalid)?,
                    at: Text::new(chrono::Utc::now().to_rfc3339()).map_err(|_| VerificationError::Invalid)?, must_not })
            };
            let plan = file_plan(&call, HostSnapshotSource::Skill { agent: &agent, name: &name }, &bytes, operation, ContentFormat::Text, Some(options))?;
            Ok((plan, json!({"agent":agent,"skill":name,"source":source,"sha256":hex::encode(Sha256::digest(&bytes))})))
        }).await.map_err(|_| VerificationError::Unavailable)?
    }

    async fn prepare_product(&self, call: &AuthorizedToolCall) -> Result<(WorkerPlan, Value), VerificationError> {
        match call.name() {
            "web_extract" => self.prepare_web(call).await,
            "skill_security_scan" | "shared_skill_adopt" | "skill_graduate" | "skill_extract" => self.prepare_skill_scan(call).await,
            "transcribe_audio" => {
                media_arguments(call.name(), call.arguments())?;
                let bytes = audio_bytes(call.arguments()["audio_base64"].as_str().ok_or(VerificationError::Invalid)?)?;
                let state = json!({"audio_base64":base64::engine::general_purpose::STANDARD.encode(&bytes),
                    "sha256":hex::encode(Sha256::digest(&bytes))});
                Ok((audio_plan(call, &bytes)?, state))
            }
            "synthesize_speech" => {
                media_arguments(call.name(), call.arguments())?;
                use crate::tts::TtsProvider;
                // Cloud credentials and provider selection stay on the host.
                // Local/command providers require their own A producer first.
                let router = crate::tts::TtsRouter::for_protected_home(&self.home);
                if !router.is_available() { return Err(VerificationError::Unavailable); }
                call.context().verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
                let bytes = router.synthesize(call.arguments()["text"].as_str().ok_or(VerificationError::Invalid)?,
                    call.arguments().get("voice").and_then(Value::as_str).unwrap_or("")).await
                    .map_err(|_| VerificationError::Unavailable)?;
                // The MCP response is a bounded frame, not a bulk-media channel.
                if bytes.len() > 128 * 1024 { return Err(VerificationError::Unavailable); }
                let state = json!({"audio_base64":base64::engine::general_purpose::STANDARD.encode(&bytes),
                    "sha256":hex::encode(Sha256::digest(&bytes))});
                Ok((audio_plan(call, &bytes)?, state))
            }
            _ => Err(VerificationError::Unavailable),
        }
    }

    async fn finish_product(&self, call: &AuthorizedToolCall, reply: WorkerReply, state: Value)
        -> Result<Value, VerificationError> {
        if call.name() == "web_extract" { return web_result(call.arguments(), reply, Some(state)); }
        if matches!(call.name(), "skill_security_scan" | "shared_skill_adopt" | "skill_graduate" | "skill_extract") {
            let (agent, name) = self.skill_subject(call)?;
            if state["agent"].as_str() != Some(agent) || state["skill"].as_str() != Some(name.as_str()) {
                return Err(VerificationError::BindingMismatch);
            }
            let ParseResult::Complete { items } = reply.response.result else { return Err(VerificationError::Unavailable); };
            if !reply.outputs.is_empty() { return Err(VerificationError::Invalid); }
            if call.name() == "skill_extract" {
                let [ParsedItem::SkillKnowledge { input_slot: 0, concepts, entities, pages }] = items.as_slice()
                    else { return Err(VerificationError::Invalid); };
                let (home, agent, name, context, roots, concepts, entities, pages) = (self.home.clone(), agent.to_owned(), name,
                    call.context().clone(), self.roots.clone().ok_or(VerificationError::Unavailable)?, *concepts, *entities, pages.as_slice().to_vec());
                let authority = duduclaw_mcp_boundary::io_authority::capture();
                return tokio::task::spawn_blocking(move || authority.run(|| publish_extraction(&home, &agent, &name, &context, &roots,
                    concepts, entities, &pages))).await.map_err(|_| VerificationError::Unavailable)?;
            }
            let Some(ParsedItem::SkillScan { input_slot: 0, passed, risk, findings }) = items.as_slice().first()
                else { return Err(VerificationError::Invalid); };
            if call.name() != "skill_security_scan" {
                if !*passed { return Err(VerificationError::Invalid); }
                let [_, ParsedItem::PreparedSkill { input_slot: 0, content }] = items.as_slice()
                    else { return Err(VerificationError::Invalid); };
                if call.name() == "skill_graduate" {
                    // H12a: global scope is loaded by every employee, so this is
                    // not an own-namespace publication. Like synthesis, it needs
                    // the dashboard admin card, bound to the scanned source.
                    call.context().verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
                    let binding = json!({"agent":agent,"skill":name,"source":state["source"],"source_sha256":state["sha256"]});
                    if let Err(message) = crate::skill_lifecycle::isolated::approve(&self.home, agent, "graduate",
                        &format!("{agent}:{name}"), &binding,
                        json!({"skill":name,"from":agent,"scope":"global","markdown":content.as_str()})).await {
                        return Ok(json!({"isError":true,"content":[{"type":"text","text":message}]}));
                    }
                }
                let (home, agent, name, tool, content) = (self.home.clone(), agent.to_owned(), name,
                    call.name(), content.as_str().to_owned());
                let authority = duduclaw_mcp_boundary::io_authority::capture();
                return tokio::task::spawn_blocking(move || authority.run(|| publish_skill(&home, &agent, &name, tool, &state,
                    &content))).await.map_err(|_| VerificationError::Unavailable)?;
            }
            if items.as_slice().len() != 1 { return Err(VerificationError::Invalid); }
            duduclaw_mcp_boundary::io_authority::verify_before_io()?;
            crate::evolution_events::emitter::EvolutionEventEmitter::global().emit_security_scan(agent, &name, *passed,
                json!({"risk_level":risk.as_str(),"findings_count":findings.as_slice().len()}));
            return Ok(scan_report(&name, *passed, risk.as_str(), findings.as_slice()));
        }
        media_arguments(call.name(), call.arguments())?;
        audio_result(&reply)?;
        let bytes = audio_bytes(state["audio_base64"].as_str().ok_or(VerificationError::Invalid)?)?;
        if state["sha256"].as_str() != Some(hex::encode(Sha256::digest(&bytes)).as_str()) {
            return Err(VerificationError::BindingMismatch);
        }
        if call.name() == "synthesize_speech" {
            return Ok(json!({"content":[{"type":"text","text":format!("Audio synthesized ({} bytes). Base64 data:\n{}",
                bytes.len(), state["audio_base64"].as_str().ok_or(VerificationError::Invalid)?)}]}));
        }
        // Never feed a worker-selected pathname, command, provider or credential
        // to the host. The inspected bytes are this invocation's own snapshot.
        if bytes != audio_bytes(call.arguments()["audio_base64"].as_str().ok_or(VerificationError::Invalid)?)? {
            return Err(VerificationError::BindingMismatch);
        }
        duduclaw_mcp_boundary::io_authority::verify_before_io()?;
        let text = duduclaw_inference::whisper::transcribe(&bytes,
            Some(call.arguments().get("language").and_then(Value::as_str).unwrap_or("zh")),
            &duduclaw_inference::whisper::WhisperMode::Api).await.map_err(|_| VerificationError::Unavailable)?;
        if text.len() > MAX_RESULT_TEXT_BYTES { return Err(VerificationError::Unavailable); }
        Ok(json!({"content":[{"type":"text","text":text}]}))
    }

    async fn prepare_web(&self, call: &AuthorizedToolCall) -> Result<(WorkerPlan, Value), VerificationError> {
        let query = web_query(call.arguments())?;
        let url = call.arguments()["url"].as_str().ok_or(VerificationError::Invalid)?;
        if !crate::web_fetch::admit_protected_tool_fetch() { return Err(VerificationError::Unavailable); }
        let ttl = match call.arguments().get("ttl_seconds") {
            None => 0,
            Some(value) => value.as_u64().ok_or(VerificationError::Invalid)?,
        };
        // No redirects/proxies: an origin capability cannot authorize a second
        // origin. This cache cannot contain legacy redirect-following results.
        // A D-H2 PublicOrigins call re-resolves its URL through the signed grant.
        if call.resources().iter().any(|grant| matches!(grant, duduclaw_isolation::capability::ResourceGrant::PublicOrigins { .. })) {
            duduclaw_mcp_boundary::resources::authorize_public_url(call)?;
        }
        call.context().verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
        let fetched = crate::web_fetch::web_fetch_protected_cached(url, ttl, &self.home.join("web_cache"))
            .await.map_err(|_| VerificationError::Unavailable)?;
        let state = json!({"url":fetched.url,"selector":query.selector.as_str(),"cached":fetched.cached});
        let body = fetched.body.into_bytes();
        let call = call.clone();
        let plan = tokio::task::spawn_blocking(move || html_plan(&call, &body, query)).await
            .map_err(|_| VerificationError::Unavailable)??;
        Ok((plan, state))
    }
}
impl WorkerPlanFactory for GatewayWorkerPlans {
    fn supports(&self, tool: &str) -> bool { matches!(tool, "web_extract" | "transcribe_audio" | "synthesize_speech")
        || (matches!(tool, "skill_security_scan" | "shared_skill_adopt" | "skill_graduate") && self.employee.is_some())
        || (tool == "skill_extract" && self.employee.is_some() && self.roots.is_some()) }
    fn authorize_handler<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
        Box::pin(async move {
            if !self.supports(call.name()) { return Err(VerificationError::Unavailable); }
            // Original scope, approval, redaction and egress gates run before D
            // dispatch. Independently refuse extra selectors and URL credentials.
            if matches!(call.name(), "skill_security_scan" | "shared_skill_adopt" | "skill_graduate" | "skill_extract") { self.skill_subject(call)?; return Ok(()); }
            if call.name() != "web_extract" { return media_arguments(call.name(), call.arguments()); }
            web_query(call.arguments())?;
            crate::web_fetch::validate_workflow_public_url(call.arguments()["url"].as_str()
                .ok_or(VerificationError::Invalid)?).map_err(|_| VerificationError::Invalid)?;
            Ok(())
        })
    }
    fn prepare<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, WorkerPlan> {
        Box::pin(async move { self.prepare_product(call).await.map(|(plan, _)| plan) })
    }
    fn prepare_with_state<'a>(&'a self, call: &'a AuthorizedToolCall)
        -> PolicyFuture<'a, (WorkerPlan, Value)> {
        Box::pin(async move { self.prepare_product(call).await })
    }
    fn finish<'a>(&'a self, call: &'a AuthorizedToolCall, reply: WorkerReply) -> PolicyFuture<'a, Value> {
        Box::pin(async move {
            if call.name() != "web_extract" { return Err(VerificationError::Unavailable); }
            web_result(call.arguments(), reply, None)
        })
    }
    fn finish_with_state<'a>(&'a self, call: &'a AuthorizedToolCall, reply: WorkerReply,
        state: Value) -> PolicyFuture<'a, Value> {
        Box::pin(async move { self.finish_product(call, reply, state).await })
    }
}

/// H12a: render a worker scan verdict as inert data. Findings are worker text
/// derived from untrusted skill bytes: each is one bounded line (controls,
/// including newlines, become spaces) so a finding cannot forge report lines
/// such as `Passed: true`. The verdict itself is the contract-validated bool.
fn scan_report(name: &str, passed: bool, risk: &str, findings: &[Text<2048>]) -> Value {
    let lines = findings.iter().map(|finding| {
        let line: String = finding.as_str().chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        format!("- {}", line.trim())
    }).collect::<Vec<_>>();
    json!({"content":[{"type":"text","text":format!("**Security scan: {name}**\nRisk level: {risk}\nPassed: {passed}\nFindings ({}):\n{}",
        lines.len(), lines.join("\n"))}]})
}

fn web_query(args: &Value) -> Result<HtmlQuery, VerificationError> {
    let object = args.as_object().ok_or(VerificationError::Invalid)?;
    if object.keys().any(|key| !matches!(key.as_str(), "url" | "ttl_seconds" | "selector" | "format")) {
        return Err(VerificationError::Invalid);
    }
    let url = args.get("url").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
    if url.is_empty() || url.len() > 8192 || url.chars().any(char::is_control) { return Err(VerificationError::Invalid); }
    if args.get("ttl_seconds").is_some_and(|value| value.as_u64().is_none()) { return Err(VerificationError::Invalid); }
    let selector = args.get("selector").and_then(Value::as_str).filter(|s| !s.is_empty())
        .ok_or(VerificationError::Invalid)?;
    let format = match args.get("format") {
        None => HtmlOutputFormat::Text,
        Some(Value::String(format)) => match format.as_str() {
            "text" => HtmlOutputFormat::Text, "html" => HtmlOutputFormat::Html,
            "json" => HtmlOutputFormat::Json, _ => return Err(VerificationError::Invalid),
        },
        Some(_) => return Err(VerificationError::Invalid),
    };
    Ok(HtmlQuery { name: Text::new("result".into()).map_err(|_| VerificationError::Invalid)?,
        selector: Text::new(selector.to_owned()).map_err(|_| VerificationError::Invalid)?,
        format, max_matches: 0 })
}

fn html_plan(call: &AuthorizedToolCall, body: &[u8], query: HtmlQuery) -> Result<WorkerPlan, VerificationError> {
    if body.len() > 5 * 1024 * 1024 { return Err(VerificationError::Invalid); }
    file_plan(call, HostSnapshotSource::WebFetch { url: call.arguments()["url"].as_str().ok_or(VerificationError::Invalid)? }, body, ParseOperation::ExtractHtml, ContentFormat::Text,
        Some(ParseOptions::Html { queries: List::new(vec![query]).map_err(|_| VerificationError::Invalid)? }))
}

fn file_plan(call: &AuthorizedToolCall, source: HostSnapshotSource<'_>, body: &[u8], operation: ParseOperation, format: ContentFormat, options: Option<ParseOptions>)
    -> Result<WorkerPlan, VerificationError> {
    if body.len() > 64 * 1024 * 1024 { return Err(VerificationError::Invalid); }
    let input = AuthorizedFile::from_host_snapshot(call, source, body)?;
    let request = ParseRequest { version: WORKER_PROTOCOL_VERSION, request_id: RequestId(rand::random()),
        operation, options,
        inputs: List::new(vec![InputFd { slot: 0, format, size_bytes: body.len() as u64 }])
            .map_err(|_| VerificationError::Invalid)?,
        outputs: List::new(vec![]).map_err(|_| VerificationError::Invalid)?,
        limits: WorkerLimits { memory_bytes: (512 * 1024 * 1024).try_into().map_err(|_| VerificationError::Invalid)?,
            wall_time_ms: 30_000.try_into().map_err(|_| VerificationError::Invalid)?,
            max_result_text_bytes: (MAX_RESULT_TEXT_BYTES as u32).try_into().map_err(|_| VerificationError::Invalid)? } };
    request.validate().map_err(|_| VerificationError::Invalid)?;
    WorkerPlan::new(request, input)
}

fn web_result(args: &Value, reply: WorkerReply, state: Option<Value>) -> Result<Value, VerificationError> {
    let query = web_query(args)?;
    if !reply.outputs.is_empty() { return Err(VerificationError::Invalid); }
    let ParseResult::Complete { items } = reply.response.result else { return Err(VerificationError::Unavailable); };
    let mut values = Vec::new();
    for item in items.as_slice() {
        let ParsedItem::Html { input_slot: 0, query: name, value } = item else { return Err(VerificationError::Invalid); };
        if name.as_str() != "result" { return Err(VerificationError::Invalid); }
        let value: Value = serde_json::from_str(value.as_str()).map_err(|_| VerificationError::Invalid)?;
        if matches!(query.format, HtmlOutputFormat::Text | HtmlOutputFormat::Html) && !value.is_string() {
            return Err(VerificationError::Invalid);
        }
        values.push(value);
    }
    let mut report = json!({"url":args["url"],"selector":query.selector.as_str(),"matches":values.len(),"results":values});
    if let Some(state) = state {
        if state["url"] != args["url"] || state["selector"] != args["selector"] {
            return Err(VerificationError::BindingMismatch);
        }
        report["cached"] = json!(state["cached"].as_bool().ok_or(VerificationError::Invalid)?);
    }
    let text = serde_json::to_string_pretty(&report).map_err(|_| VerificationError::Invalid)?;
    let text = if text.chars().count() > BODY_CAP_CHARS {
        text.chars().take(BODY_CAP_CHARS).collect::<String>() + "\n…[truncated]"
    } else { text };
    Ok(json!({"content":[{"type":"text","text":text}]}))
}

#[cfg(test)]
#[path = "isolation_worker_plans_tests.rs"]
mod tests;

fn media_arguments(tool: &str, args: &Value) -> Result<(), VerificationError> {
    let object = args.as_object().ok_or(VerificationError::Invalid)?;
    let (required, optional) = match tool {
        "transcribe_audio" => ("audio_base64", "language"),
        "synthesize_speech" => ("text", "voice"),
        _ => return Err(VerificationError::Invalid),
    };
    if object.keys().any(|key| key != required && key != optional) { return Err(VerificationError::Invalid); }
    let value = args[required].as_str().filter(|s| !s.is_empty()).ok_or(VerificationError::Invalid)?;
    if tool == "transcribe_audio" {
        if value.len() > 34 * 1024 * 1024 { return Err(VerificationError::Invalid); }
    } else if value.chars().count() > crate::tts::MAX_TTS_TEXT_CHARS { return Err(VerificationError::Invalid); }
    if let Some(value) = args.get(optional) {
        let value = value.as_str().ok_or(VerificationError::Invalid)?;
        if value.len() > 128 || value.chars().any(char::is_control)
            || (optional == "language" && (value.is_empty() || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))) {
            return Err(VerificationError::Invalid);
        }
    }
    Ok(())
}
fn audio_bytes(encoded: &str) -> Result<Vec<u8>, VerificationError> {
    if encoded.is_empty() || encoded.len() > 34 * 1024 * 1024 { return Err(VerificationError::Invalid); }
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|_| VerificationError::Invalid)?;
    if bytes.is_empty() || bytes.len() > 25 * 1024 * 1024 { return Err(VerificationError::Invalid); }
    Ok(bytes)
}
fn audio_plan(call: &AuthorizedToolCall, bytes: &[u8]) -> Result<WorkerPlan, VerificationError> {
    if bytes.is_empty() || bytes.len() > 25 * 1024 * 1024 { return Err(VerificationError::Invalid); }
    let source = match call.name() {
        "transcribe_audio" => HostSnapshotSource::InlineAudio,
        "synthesize_speech" => HostSnapshotSource::SynthesizedSpeech,
        _ => return Err(VerificationError::BindingMismatch),
    };
    file_plan(call, source, bytes, ParseOperation::InspectAudio, ContentFormat::Audio, None)
}

fn audio_result(reply: &WorkerReply) -> Result<(), VerificationError> {
    if !reply.outputs.is_empty() { return Err(VerificationError::Invalid); }
    let ParseResult::Complete { items } = &reply.response.result else { return Err(VerificationError::Unavailable); };
    let [ParsedItem::Audio { input_slot: 0, sample_rate, channels, frames, .. }] = items.as_slice()
        else { return Err(VerificationError::Invalid); };
    if !(8000..=192000).contains(sample_rate) || !(1..=8).contains(channels) || *frames == 0 {
        return Err(VerificationError::Invalid);
    }
    Ok(())
}

// Policy files are host-owned. Parse a bounded, strict TOML table rather than
// heuristically dropping malformed CONTRACT restrictions.
fn contract_patterns(bytes: &[u8]) -> Result<Vec<Text<1024>>, VerificationError> {
    let content = std::str::from_utf8(bytes).map_err(|_| VerificationError::Invalid)?;
    let table: toml::Table = content.parse().map_err(|_| VerificationError::Invalid)?;
    let nested = match table.get("boundaries") {
        None => None,
        Some(value) => value.as_table().ok_or(VerificationError::Invalid)?.get("must_not"),
    };
    let value = nested.or_else(|| table.get("must_not"));
    let Some(value) = value else { return Ok(vec![]); };
    let values = value.as_array().ok_or(VerificationError::Invalid)?;
    if values.len() > 128 { return Err(VerificationError::Invalid); }
    values.iter().map(|v| Text::new(v.as_str().ok_or(VerificationError::Invalid)?.to_owned())
        .map_err(|_| VerificationError::Invalid)).collect()
}

// Only a trusted boot-bound employee and validated skill slug select these
// host-owned files. Every path component is opened without following symlinks;
// a missing local skill permits global fallback, unreadable/unsafe files do not.
#[cfg(target_os = "linux")]
pub(crate) fn read_host_file(home: &std::path::Path, directories: &[&str], filename: &str)
    -> Result<Option<Vec<u8>>, VerificationError> {
    read_host_file_limit(home, directories, filename, 64 * 1024)
}
#[cfg(target_os = "linux")]
fn read_host_file_limit(home: &std::path::Path, directories: &[&str], filename: &str, cap: u64)
    -> Result<Option<Vec<u8>>, VerificationError> {
    use std::{io::Read, os::fd::{AsRawFd, FromRawFd}, os::unix::fs::MetadataExt};
    use nix::{fcntl::{openat, OFlag}, sys::stat::Mode};
    let mut root = File::open(home).map_err(|_| VerificationError::Unavailable)?;
    let mut components = directories.to_vec(); components.push(filename);
    for (index, component) in components.iter().enumerate() {
        let directory = index + 1 != components.len();
        let flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK
            | OFlag::O_NOCTTY | if directory { OFlag::O_DIRECTORY } else { OFlag::empty() };
        let fd = match openat(Some(root.as_raw_fd()), *component, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(nix::errno::Errno::ENOENT) => return Ok(None),
            Err(_) => return Err(VerificationError::Unavailable),
        };
        // SAFETY: openat returned one owned descriptor; File closes it on every path.
        root = unsafe { File::from_raw_fd(fd) };
    }
    let meta = root.metadata().map_err(|_| VerificationError::Unavailable)?;
    if !meta.is_file() || meta.nlink() != 1 || meta.len() > cap { return Err(VerificationError::Invalid); }
    let mut bytes = Vec::new();
    root.take(cap + 1).read_to_end(&mut bytes).map_err(|_| VerificationError::Unavailable)?;
    if bytes.len() as u64 != meta.len() { return Err(VerificationError::Invalid); }
    Ok(Some(bytes))
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn read_host_file(_home: &std::path::Path, _directories: &[&str], _filename: &str)
    -> Result<Option<Vec<u8>>, VerificationError> { Err(VerificationError::Unavailable) }

fn publish_skill(home: &std::path::Path, agent: &str, name: &str, tool: &str, state: &Value,
    content: &str) -> Result<Value, VerificationError> {
    if content.is_empty() || content.len() > 64 * 1024 { return Err(VerificationError::Invalid); }
    let filename = format!("{name}.md");
    let source_dirs = match (tool, state["source"].as_str()) {
        ("shared_skill_adopt", Some("shared")) => vec!["shared", "skills"],
        ("skill_graduate", Some("local")) => vec!["agents", agent, "SKILLS"],
        _ => return Err(VerificationError::BindingMismatch),
    };
    let lock = home.join("skills-publication.lock");
    duduclaw_core::with_file_lock(&lock, || {
        let result = (|| {
            duduclaw_mcp_boundary::io_authority::verify_before_io()?;
            let source = read_host_file(home, &source_dirs, &filename)?.ok_or(VerificationError::Unavailable)?;
            if state["sha256"].as_str() != Some(hex::encode(Sha256::digest(&source)).as_str()) {
                return Err(VerificationError::BindingMismatch);
            }
            if tool == "shared_skill_adopt" {
                // Install from the scanned snapshot; never reparse or copy from
                // the source pathname after the worker has returned.
                write_host_file(home, &["agents", agent, "SKILLS"], &filename, content.as_bytes())?;
                Ok(json!({"content":[{"type":"text","text":json!({"success":true,"skill":name,"adopted_to":agent,"shared_metadata_updated":false}).to_string()}]}))
            } else {
                #[cfg(target_os = "linux")]
                skill_publication::publish(home, agent, name,
                    state["sha256"].as_str().ok_or(VerificationError::Invalid)?, content.as_bytes())?;
                #[cfg(not(target_os = "linux"))]
                return Err(VerificationError::Unavailable);
                Ok(json!({"content":[{"type":"text","text":format!("Skill '{name}' graduated from agent '{agent}' to global scope.")}]}))
            }
        })();
        result.map_err(|_| std::io::Error::other("skill publication unconfirmed"))
    }).map_err(|_| VerificationError::Unavailable)
}

#[cfg(target_os = "linux")]
pub(crate) fn write_host_file(home: &std::path::Path, directories: &[&str], filename: &str, bytes: &[u8])
    -> Result<(), VerificationError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use nix::{fcntl::{openat, OFlag}, sys::stat::{mkdirat, Mode}};
    let safe = |part: &str| !part.is_empty() && part.len() <= 255 && !matches!(part, "." | "..")
        && !part.chars().any(|c| c == '/' || c == '\\' || c.is_control());
    if bytes.len() > 64 * 1024 || !safe(filename) || directories.iter().any(|part| !safe(part)) {
        return Err(VerificationError::Invalid);
    }
    let mut root = File::open(home).map_err(|_| VerificationError::Unavailable)?;
    for component in directories {
        duduclaw_mcp_boundary::io_authority::verify_before_io()?;
        match mkdirat(Some(root.as_raw_fd()), *component, Mode::from_bits_truncate(0o700)) {
            Ok(()) | Err(nix::errno::Errno::EEXIST) => {},
            Err(_) => return Err(VerificationError::Unavailable),
        }
        let fd = openat(Some(root.as_raw_fd()), *component,
            OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW, Mode::empty())
            .map_err(|_| VerificationError::Unavailable)?;
        // SAFETY: exactly one owned descriptor returned by openat.
        root = unsafe { File::from_raw_fd(fd) };
    }
    // /proc names this retained host-selected directory fd, never an employee
    // path. Rename/readback stay anchored even if an ancestor is renamed.
    let directory = PathBuf::from(format!("/proc/self/fd/{}", root.as_raw_fd()));
    let target = directory.join(filename);
    duduclaw_mcp_boundary::io_authority::verify_before_io()?;
    let mut temporary = tempfile::NamedTempFile::new_in(&directory).map_err(|_| VerificationError::Unavailable)?;
    duduclaw_mcp_boundary::io_authority::verify_before_io()?;
    temporary.write_all(bytes).map_err(|_| VerificationError::Unavailable)?;
    temporary.as_file().sync_all().map_err(|_| VerificationError::Unavailable)?;
    duduclaw_mcp_boundary::io_authority::verify_before_io()?;
    temporary.persist_noclobber(&target)
        .map_err(|_| VerificationError::Unavailable)?;
    root.sync_all().map_err(|_| VerificationError::Unavailable)?;
    let fd = openat(Some(root.as_raw_fd()), filename,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK, Mode::empty())
        .map_err(|_| VerificationError::Unavailable)?;
    // SAFETY: openat returned an owned descriptor for the retained directory.
    let readback = unsafe { File::from_raw_fd(fd) };
    use std::io::Read;
    if !readback.metadata().map_err(|_| VerificationError::Unavailable)?.is_file() { return Err(VerificationError::Invalid); }
    let mut actual = Vec::new();
    readback.take(64 * 1024 + 1).read_to_end(&mut actual).map_err(|_| VerificationError::Unavailable)?;
    if actual != bytes { return Err(VerificationError::Unavailable); }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn write_host_file(_home: &std::path::Path, _directories: &[&str], _filename: &str, _bytes: &[u8])
    -> Result<(), VerificationError> { Err(VerificationError::Unavailable) }

#[cfg(target_os = "linux")]
fn publish_extraction(home: &std::path::Path, agent: &str, name: &str,
    context: &duduclaw_mcp_boundary::RequestContext, roots: &duduclaw_mcp_boundary::resources::ResourceRoots,
    concepts: u16, entities: u16, pages: &[duduclaw_isolation::worker::SkillPage]) -> Result<Value, VerificationError> {
    use duduclaw_memory::wiki::{WikiAction, WikiProposal, WikiStore, WikiTarget};
    use duduclaw_isolation::capability::ResourceGrant;
    use std::os::unix::fs::OpenOptionsExt;
    let namespace = format!("agent:{agent}");
    if !context.record().io_resources().any(|r| matches!(r, ResourceGrant::Wiki { namespace: n } if n.as_str() == namespace)) {
        return Err(VerificationError::BindingMismatch);
    }
    validate_skill_pages(name, concepts, entities, pages)?;
    let wiki = home.join("agents").join(agent).join("wiki");
    let root = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW).open(&wiki)
        .map_err(|_| VerificationError::Unavailable)?;
    roots.verify_wiki_root(&namespace, &root)?;
    let summary_name = name.to_ascii_lowercase().split(['-', '_']).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    let summary_path = format!("sources/skill-{summary_name}.md");
    let mut proposals = Vec::new();
    for page in pages {
        proposals.push(WikiProposal { page_path: page.path.as_str().to_owned(), action: WikiAction::Create,
            content: Some(page.content.as_str().to_owned()), rationale: page.rationale.as_str().to_owned(),
            related_pages: page.related.as_slice().iter().map(|r| r.as_str().to_owned()).collect(), target: WikiTarget::default() });
    }
    crate::gvu::verifier::verify_wiki_proposals(&proposals).map_err(|_| VerificationError::Invalid)?;
    let store = WikiStore::new(wiki);
    context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
    duduclaw_mcp_boundary::io_authority::verify_before_io()?;
    store.ensure_scaffold().map_err(|_| VerificationError::Unavailable)?;
    // Match the existing completion marker; a worker does not pick it.
    if store.wiki_dir().join(&summary_path).is_file() {
        return Ok(json!({"content":[{"type":"text","text":format!("Skill '{name}' has already been extracted to wiki.")}]}));
    }
    let live = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW).open(store.wiki_dir())
        .map_err(|_| VerificationError::Unavailable)?;
    roots.verify_wiki_root(&namespace, &live)?;
    let (summary, body) = proposals.split_last().ok_or(VerificationError::Invalid)?;
    context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
    duduclaw_mcp_boundary::io_authority::verify_before_io()?;
    let written = store.apply_proposals(body).map_err(|_| VerificationError::Unavailable)?;
    if written != body.len() { return Err(VerificationError::Unavailable); }
    // Never publish the completion marker until every body page has landed
    // and read back. A partial body write remains explicitly unconfirmed.
    for page in pages.iter().take(body.len()) {
        let (directory, filename) = page.path.as_str().split_once('/').ok_or(VerificationError::Invalid)?;
        let actual = read_host_file(home, &["agents", agent, "wiki", directory], filename)?.ok_or(VerificationError::Unavailable)?;
        if actual != page.content.as_str().as_bytes() { return Err(VerificationError::Unavailable); }
    }
    context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
    duduclaw_mcp_boundary::io_authority::verify_before_io()?;
    if store.apply_proposals(std::slice::from_ref(summary)).map_err(|_| VerificationError::Unavailable)? != 1 {
        return Err(VerificationError::Unavailable);
    }
    let (_, filename) = summary_path.split_once('/').ok_or(VerificationError::Invalid)?;
    let actual = read_host_file(home, &["agents", agent, "wiki", "sources"], filename)?.ok_or(VerificationError::Unavailable)?;
    if Some(actual.as_slice()) != summary.content.as_ref().map(|s| s.as_bytes()) { return Err(VerificationError::Unavailable); }
    let count = written + 1;
    Ok(json!({"content":[{"type":"text","text":format!("Extracted knowledge from skill '{name}':\n- {concepts} concept pages\n- {entities} entity pages\n- 1 source summary\n- {count} total pages written")}]}))
}
#[cfg(not(target_os = "linux"))]
fn publish_extraction(_home: &std::path::Path, _agent: &str, _name: &str,
    _context: &duduclaw_mcp_boundary::RequestContext, _roots: &duduclaw_mcp_boundary::resources::ResourceRoots,
    _concepts: u16, _entities: u16, _pages: &[duduclaw_isolation::worker::SkillPage]) -> Result<Value, VerificationError> {
    Err(VerificationError::Unavailable)
}

fn validate_skill_pages(name: &str, concepts: u16, entities: u16,
    pages: &[duduclaw_isolation::worker::SkillPage]) -> Result<(), VerificationError> {
    if concepts > 30 || entities > 10 || pages.len() != usize::from(concepts + entities) + 1 { return Err(VerificationError::Invalid); }
    let summary_name = name.to_ascii_lowercase().split(['-', '_']).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    let summary_path = format!("sources/skill-{summary_name}.md");
    let mut paths = std::collections::HashSet::new();
    for (index, page) in pages.iter().enumerate() {
        duduclaw_mcp_boundary::resources::relative_path(page.path.as_str())?;
        let components: Vec<_> = page.path.as_str().split('/').collect();
        let expected = if index < usize::from(concepts) { "concepts" }
            else if index < usize::from(concepts + entities) { "entities" } else { "sources" };
        if components.len() != 2 || components[0] != expected || !components[1].ends_with(".md")
            || (expected == "sources" && page.path.as_str() != summary_path)
            || !paths.insert(page.path.as_str()) { return Err(VerificationError::Invalid); }
    }
    for page in pages {
        if page.related.as_slice().iter().any(|r| !paths.contains(r.as_str())) { return Err(VerificationError::Invalid); }
    }
    Ok(())
}

/// Host-selected recording snapshots. Metadata is gateway-owned authority;
/// raw HAR/actions/events are never deserialized in the protected host path.
#[cfg(target_os = "linux")]
pub fn recording_snapshot(home: &std::path::Path, id: &str, agent: &str)
    -> Result<(Value, Vec<u8>), VerificationError> {
    if id.is_empty() || id.len() > 128 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(VerificationError::Invalid);
    }
    let dirs = ["recordings", id];
    let meta: Value = serde_json::from_slice(&read_host_file(home, &dirs, "meta.json")?
        .ok_or(VerificationError::Unavailable)?).map_err(|_| VerificationError::Invalid)?;
    if meta["agent"].as_str() != Some(agent) || meta["id"].as_str() != Some(id)
        || !matches!(meta["kind"].as_str(), Some("browser" | "desktop")) {
        return Err(VerificationError::BindingMismatch);
    }
    read_host_file(home, &dirs, "done.json")?.ok_or(VerificationError::Unavailable)?;
    let text = |directories: &[&str], file: &str, cap| -> Result<String, VerificationError> {
        String::from_utf8(read_host_file_limit(home, directories, file, cap)?.unwrap_or_default())
            .map_err(|_| VerificationError::Invalid)
    };
    let (har, actions, events) = if meta["kind"] == "desktop" {
        (String::new(), String::new(), text(&["recordings", id, "desktop"], "events.jsonl", 8 * 1024 * 1024)?)
    } else {
        (text(&dirs, "session.har", 50 * 1024 * 1024)?, text(&dirs, "actions.json", 4 * 1024 * 1024)?, String::new())
    };
    // JSON escaping can expand sixfold. Refuse before serializing, rather than
    // constructing an oversized allocation and discovering the limit afterwards.
    let raw_size = har.len().checked_add(actions.len()).and_then(|n| n.checked_add(events.len()))
        .ok_or(VerificationError::Invalid)?;
    if raw_size > (64 * 1024 * 1024 - 1024) / 6 { return Err(VerificationError::Invalid); }
    let bytes = serde_json::to_vec(&json!({"har":har,"actions":actions,"events":events})).map_err(|_| VerificationError::Invalid)?;
    if bytes.len() > 64 * 1024 * 1024 { return Err(VerificationError::Invalid); }
    Ok((meta, bytes))
}
#[cfg(not(target_os = "linux"))]
pub fn recording_snapshot(_home: &std::path::Path, _id: &str, _agent: &str)
    -> Result<(Value, Vec<u8>), VerificationError> { Err(VerificationError::Unavailable) }

/// Read operator-selected absolute paths without following any component.
#[cfg(target_os = "linux")]
pub(crate) fn absolute_snapshot(path: &std::path::Path, cap: u64) -> Result<Option<Vec<u8>>, VerificationError> {
    if !path.is_absolute() || cap > 64 * 1024 * 1024 { return Err(VerificationError::Invalid); }
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            std::path::Component::RootDir => {},
            std::path::Component::Normal(name) => parts.push(name.to_str().ok_or(VerificationError::Invalid)?),
            _ => return Err(VerificationError::Invalid),
        }
    }
    let (name, dirs) = parts.split_last().ok_or(VerificationError::Invalid)?;
    read_host_file_limit(std::path::Path::new("/"), dirs, name, cap)
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn absolute_snapshot(_path: &std::path::Path, _cap: u64) -> Result<Option<Vec<u8>>, VerificationError> {
    Err(VerificationError::Unavailable)
}
