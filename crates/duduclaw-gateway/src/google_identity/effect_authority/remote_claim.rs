//! True operator entry and per-operation requester, frame and original account.
use super::*;
use crate::mcp_external::ExternalGoogleMountEntry;
use duduclaw_llm::{ClientOperation, ClosedClientTarget, OwnedScopedOperation};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub(crate) struct GoogleClosedMount {
    entry: Arc<ExternalGoogleMountEntry>,
}
impl std::fmt::Debug for GoogleClosedMount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GoogleClosedMount([host configuration])")
    }
}
impl GoogleClosedMount {
    pub(crate) fn from_host_entry(entry: ExternalGoogleMountEntry) -> Result<Self, Refusal> {
        if !entry.validate() || entry.server().trim().is_empty() {
            return Err(changed());
        }
        Ok(Self {
            entry: Arc::new(entry),
        })
    }
    pub(crate) fn registry_slot(self: Arc<Self>) -> duduclaw_llm::RegistryClient {
        duduclaw_llm::RegistryClient::scoped_http_closed(
            self.entry.recipe().clone(),
            self.entry.binding(),
            self,
        )
    }
    async fn prepare(
        &self,
        target: ClosedClientTarget<'_>,
        operation: ClientOperation<'_>,
        args: Value,
    ) -> Result<Box<dyn OwnedScopedOperation + Send + 'static>, Refusal> {
        // Task-local identity must be captured here, before any await/offload.
        let who = crate::google_identity::mcp_mount::requester(self.entry.agent());
        if !self.entry.validate()
            || target.server() != self.entry.server()
            || !target.matches_recipe(self.entry.recipe())
            || !target.matches_host_binding(&self.entry.binding())
            || !target.matches_filter(self.entry.filter())
        {
            return Err(changed());
        }
        if let ClientOperation::Tool { name } = operation {
            if !self.entry.filter().permits(name) {
                return Err(changed());
            }
        }
        let bound = RemoteRequestBound::new(
            self.entry.clone(),
            who.clone(),
            target.deadline(),
            operation,
            args,
        )?;
        #[cfg(test)]
        if !testing::required(self.entry.home(), self.entry.url()) {
            return Err(changed());
        }
        let entry = self.entry.clone();
        // Only finite metadata preparation can outlive a cancelled receiver.
        let pending = tokio::task::spawn_blocking(move || {
            let (home, agent_dir, agent) = GoogleCallContext::canonical(entry.home(), entry.dir())?;
            let context = GoogleCallContext {
                people: crate::google_identity::requester_gate::people_for(&home, None),
                home,
                agent_dir,
                agent,
                requester: who,
                purpose: GooglePurpose::ClosedRemote(bound),
            };
            context.validate()?;
            let selected = match gi::effective_for(&context.home, Some(&context.agent)).effective {
                gi::Effective::Personal { .. } => Some(PersonalSelection::prepare_dedicated(
                    &context.home,
                    &context.agent,
                    &context.requester,
                    context.people.as_ref(),
                    "google_remote_mcp",
                    || context.validate(),
                )?),
                gi::Effective::Home | gi::Effective::Delegate { .. } => None,
                other => {
                    return Err(crate::google_identity::resolve::refusal_for(&other)
                        .unwrap_or_else(changed));
                }
            };
            Ok::<_, Refusal>((context, selected))
        })
        .await
        .map_err(|_| changed())??;
        let (context, personal) = pending;
        #[cfg(test)]
        let context = testing::maybe_corrupt_frame(context);
        context.validate()?;
        let GooglePurpose::ClosedRemote(bound) = &context.purpose else {
            return Err(changed());
        };
        let deadline = bound.deadline();
        let original = match personal {
            Some(original) if original.ensure_healthy_personal().is_ok() => {
                RemoteOriginal::Personal(original)
            }
            Some(original) => {
                original.validate_original_unavailable(
                    &context.home,
                    &context.agent,
                    &context.requester,
                    context.people.as_ref(),
                    "google_remote_mcp",
                )?;
                let prepared = super::variants::Prepared::home(
                    &context,
                    deadline,
                    true,
                    PreflightOriginal::Personal(&original),
                )
                .await?;
                original.validate_original_unavailable(
                    &context.home,
                    &context.agent,
                    &context.requester,
                    context.people.as_ref(),
                    "google_remote_mcp",
                )?;
                RemoteOriginal::PreparedHome { original, prepared }
            }
            None => RemoteOriginal::Immediate(
                super::variants::Prepared::immediate(&context, deadline).await?,
            ),
        };
        let mut claim = ClaimedRemoteCall {
            original,
            nonce: bound.nonce.clone(),
            frame_digest: bound.frame_digest.clone(),
        };
        claim.validate(&context)?;
        let selected = match &mut claim.original {
            RemoteOriginal::Personal(original) => Material::Personal(
                access::resolve_pinned_mcp(
                    &context.home,
                    &context.agent,
                    &context.requester,
                    context.people.as_ref(),
                    original,
                    deadline,
                )
                .await
                .map_err(|mut error| {
                    error.fallback_eligible = false;
                    error
                })?,
            ),
            RemoteOriginal::PreparedHome { prepared, .. } | RemoteOriginal::Immediate(prepared) => {
                Material::Variant(prepared.take()?)
            }
        };
        claim.validate_material(&context, &selected)?;
        // Both resolver temporary leases/mutexes have left their functions.
        let authority = ResolvedGoogleMaterial {
            context,
            claim: MaterialClaim::Remote(claim),
            selected,
        }
        .acquire_effect()
        .await?;
        Ok(Box::new(super::mcp_operation::GoogleMcpOperation::new(
            authority,
        )))
    }
}
#[async_trait::async_trait]
impl duduclaw_llm::ToolInterceptor for GoogleClosedMount {
    fn before_call(&self, _: &str, _: &str, _: Value) -> duduclaw_llm::InterceptDecision {
        duduclaw_llm::InterceptDecision::Deny(
            "Closed Google operation requires its actual registry slot".into(),
        )
    }
    fn after_call(&self, _: &str, _: &str, _: &Value, _: &mut Value) {}
    async fn prepare_closed_client_call(
        &self,
        target: ClosedClientTarget<'_>,
        operation: ClientOperation<'_>,
        args: Value,
    ) -> Result<Box<dyn OwnedScopedOperation + Send + 'static>, String> {
        self.prepare(target, operation, args)
            .await
            .map_err(|_| "Closed Google authority unavailable".into())
    }
}

pub(crate) struct RemoteRequestBound {
    entry: Arc<ExternalGoogleMountEntry>,
    requester: RequesterIdentity,
    deadline: tokio::time::Instant,
    tool: Option<String>,
    args: Value,
    initialize: Vec<u8>,
    notification: Vec<u8>,
    final_frame: Vec<u8>,
    frame_digest: String,
    nonce: String,
}
struct FrameWriter(Vec<u8>);
impl std::io::Write for FrameWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > 16 * 1024 * 1024)
        {
            return Err(std::io::Error::other("Closed frame exceeds limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn frame_bytes(frame: &Value) -> Result<Vec<u8>, Refusal> {
    let mut out = FrameWriter(Vec::new());
    serde_json::to_writer(&mut out, frame).map_err(|_| changed())?;
    Ok(out.0)
}
impl RemoteRequestBound {
    fn new(
        entry: Arc<ExternalGoogleMountEntry>,
        requester: RequesterIdentity,
        deadline: tokio::time::Instant,
        operation: ClientOperation<'_>,
        args: Value,
    ) -> Result<Self, Refusal> {
        if tokio::time::Instant::now() >= deadline {
            return Err(changed());
        }
        let (tool, frame) = match operation {
            ClientOperation::Discover if args.is_null() => {
                (None, duduclaw_llm::build_tools_list_request(2))
            }
            ClientOperation::Tool { name } if !name.is_empty() && args.is_object() => (
                Some(name.to_owned()),
                duduclaw_llm::build_tools_call_request(2, name, args.clone()),
            ),
            _ => return Err(changed()),
        };
        let final_frame = frame_bytes(&frame)?;
        let nonce = uuid::Uuid::new_v4().to_string();
        let frame_digest = full_digest(&entry, &nonce, tool.as_deref(), &args, &final_frame);
        Ok(Self {
            entry,
            requester,
            deadline,
            tool,
            args,
            initialize: frame_bytes(&duduclaw_llm::build_initialize_request(1))?,
            notification: frame_bytes(&duduclaw_llm::build_initialized_notification())?,
            final_frame,
            frame_digest,
            nonce,
        })
    }
    pub(super) fn validate_context(&self, context: &GoogleCallContext) -> Result<(), Refusal> {
        #[cfg(test)]
        if !testing::required(&context.home, self.entry.url()) {
            return Err(changed());
        }
        if !self.entry.validate()
            || self.entry.home() != context.home.as_path()
            || self.entry.dir() != context.agent_dir.as_path()
            || self.entry.agent() != context.agent
            || self.requester != context.requester
            || tokio::time::Instant::now() >= self.deadline
            || self.frame_digest
                != full_digest(
                    &self.entry,
                    &self.nonce,
                    self.tool.as_deref(),
                    &self.args,
                    &self.final_frame,
                )
        {
            return Err(changed());
        }
        Ok(())
    }
    pub(crate) fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }
    pub(crate) fn endpoint(&self) -> &str {
        self.entry.url()
    }
    pub(crate) fn headers(&self) -> &[(String, String)] {
        self.entry.headers()
    }
    pub(crate) fn initialize(&self) -> &[u8] {
        &self.initialize
    }
    pub(crate) fn notification(&self) -> &[u8] {
        &self.notification
    }
    pub(crate) fn final_frame(&self) -> &[u8] {
        &self.final_frame
    }
    pub(crate) fn discovery(&self) -> bool {
        self.tool.is_none()
    }
    pub(crate) fn arguments(&self) -> &Value {
        &self.args
    }
}
enum RemoteOriginal {
    Personal(PersonalSelection),
    PreparedHome {
        original: PersonalSelection,
        prepared: super::variants::Prepared,
    },
    Immediate(super::variants::Prepared),
}
pub(super) struct ClaimedRemoteCall {
    original: RemoteOriginal,
    nonce: String,
    frame_digest: String,
}
impl ClaimedRemoteCall {
    fn validate(&self, context: &GoogleCallContext) -> Result<(), Refusal> {
        context.validate()?;
        let GooglePurpose::ClosedRemote(bound) = &context.purpose else {
            return Err(changed());
        };
        if self.nonce != bound.nonce || self.frame_digest != bound.frame_digest {
            return Err(changed());
        }
        match &self.original {
            RemoteOriginal::Personal(original) => {
                original.ensure_healthy_personal()?;
                original.validate(
                    &context.home,
                    &context.agent,
                    &context.requester,
                    context.people.as_ref(),
                    "google_remote_mcp",
                )
            }
            RemoteOriginal::PreparedHome { original, prepared } => {
                original.validate_original_unavailable(
                    &context.home,
                    &context.agent,
                    &context.requester,
                    context.people.as_ref(),
                    "google_remote_mcp",
                )?;
                prepared.validate(context)
            }
            RemoteOriginal::Immediate(prepared) => prepared.validate(context),
        }
    }
    pub(super) fn validate_material(
        &self,
        context: &GoogleCallContext,
        selected: &Material,
    ) -> Result<(), Refusal> {
        self.validate(context)?;
        match (&self.original, selected) {
            (RemoteOriginal::Personal(original), Material::Personal(token))
                if token.matches_selection(original) =>
            {
                token.validate(
                    &context.home,
                    &context.agent,
                    &context.requester,
                    context.people.as_ref(),
                    "google_remote_mcp",
                )
            }
            (
                RemoteOriginal::PreparedHome { prepared, .. } | RemoteOriginal::Immediate(prepared),
                Material::Variant(material),
            ) => prepared.validate_material(context, material),
            _ => Err(changed()),
        }
    }
}

fn full_digest(
    entry: &ExternalGoogleMountEntry,
    nonce: &str,
    tool: Option<&str>,
    args: &Value,
    frame: &[u8],
) -> String {
    let mut digest = Sha256::new();
    let binding = entry.binding();
    let arguments = gi::google_arguments_digest(args);
    let operation: &[u8] = if tool.is_some() { b"tool" } else { b"discover" };
    for bytes in [
        entry.server().as_bytes(),
        binding.as_bytes(),
        nonce.as_bytes(),
        operation,
        tool.unwrap_or("").as_bytes(),
        arguments.as_bytes(),
        frame,
    ] {
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
pub(in crate::google_identity) mod testing {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    struct Fixture {
        endpoint: String,
        nonce: String,
        observations: Vec<Value>,
        hooks: HashMap<String, Box<dyn FnOnce() + Send>>,
        corrupt_frame: bool,
    }
    fn fixtures() -> &'static Mutex<HashMap<PathBuf, Fixture>> {
        static F: OnceLock<Mutex<HashMap<PathBuf, Fixture>>> = OnceLock::new();
        F.get_or_init(Default::default)
    }
    pub(in crate::google_identity) struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            fixtures().lock().unwrap().remove(&self.0);
            let _ = std::fs::remove_file(self.0.join(".closed-mcp-owned-fixture"));
        }
    }
    pub(in crate::google_identity) fn install(home: &Path, endpoint: &str) -> Guard {
        let url = reqwest::Url::parse(endpoint).unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert!(url.port().is_some());
        let home = home.canonicalize().unwrap();
        let nonce = uuid::Uuid::new_v4().to_string();
        std::fs::write(home.join(".closed-mcp-owned-fixture"), &nonce).unwrap();
        assert!(
            fixtures()
                .lock()
                .unwrap()
                .insert(
                    home.clone(),
                    Fixture {
                        endpoint: endpoint.into(),
                        nonce,
                        observations: Vec::new(),
                        hooks: HashMap::new(),
                        corrupt_frame: false
                    }
                )
                .is_none()
        );
        Guard(home)
    }
    pub(super) fn required(home: &Path, endpoint: &str) -> bool {
        let Ok(home) = home.canonicalize() else {
            return false;
        };
        fixtures().lock().unwrap().get(&home).is_some_and(|f| {
            f.endpoint == endpoint
                && std::fs::read_to_string(home.join(".closed-mcp-owned-fixture"))
                    .ok()
                    .as_deref()
                    == Some(f.nonce.as_str())
        })
    }
    pub(in crate::google_identity::effect_authority) fn boundary(home: &Path, stage: &str) {
        let callback = fixtures()
            .lock()
            .unwrap()
            .get_mut(home)
            .and_then(|f| f.hooks.remove(stage));
        if let Some(callback) = callback {
            callback();
        }
    }
    pub(in crate::google_identity) fn hook(
        home: &Path,
        stage: &str,
        callback: Box<dyn FnOnce() + Send>,
    ) {
        fixtures()
            .lock()
            .unwrap()
            .get_mut(&home.canonicalize().unwrap())
            .unwrap()
            .hooks
            .insert(stage.into(), callback);
    }
    pub(in crate::google_identity) fn corrupt_next_final_frame(home: &Path) {
        fixtures()
            .lock()
            .unwrap()
            .get_mut(&home.canonicalize().unwrap())
            .unwrap()
            .corrupt_frame = true;
    }
    pub(super) fn maybe_corrupt_frame(mut context: GoogleCallContext) -> GoogleCallContext {
        let corrupt = fixtures()
            .lock()
            .unwrap()
            .get_mut(&context.home)
            .is_some_and(|f| std::mem::take(&mut f.corrupt_frame));
        if corrupt {
            if let GooglePurpose::ClosedRemote(bound) = &mut context.purpose {
                bound.final_frame[0] ^= 1;
            }
        }
        context
    }
    pub(in crate::google_identity) fn observations(home: &Path) -> Vec<Value> {
        fixtures()
            .lock()
            .unwrap()
            .get(&home.canonicalize().unwrap())
            .unwrap()
            .observations
            .clone()
    }
    pub(in crate::google_identity::effect_authority) fn completed(
        home: &Path,
        reply: &duduclaw_llm::OwnedScopedReply,
    ) {
        let value = match reply {
            duduclaw_llm::OwnedScopedReply::Discover(defs) => {
                serde_json::json!({"tools":defs.iter().map(|d|serde_json::json!({"name":d.name,"description":d.description,"inputSchema":d.input_schema})).collect::<Vec<_>>()})
            }
            duduclaw_llm::OwnedScopedReply::Tool { result, notice, .. } => {
                serde_json::json!({"content":result.content,"notice":notice})
            }
        };
        fixtures()
            .lock()
            .unwrap()
            .get_mut(home)
            .unwrap()
            .observations
            .push(value);
    }
}
