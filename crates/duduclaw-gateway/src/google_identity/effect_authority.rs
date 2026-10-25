//! One call's pinned Google material and effect lease. Host metadata status
//! consumes real claims; native effects stay within their credential owner.
//! No bearer can be extracted from these sealed types.
use std::path::{Path, PathBuf};

use duduclaw_core::google_identity::{self as gi, CredentialRef};
use duduclaw_knowledge::requester::RequesterIdentity;

use super::{Refusal, requester_gate::People};
use crate::google_personal::{
    access::{self, PinnedPersonalToken},
    authority::PersonalSelection,
    grants,
};

mod host_claim;
mod mcp_operation;
mod native_operation;
mod remote_claim;
pub(crate) mod variants;
pub(crate) use remote_claim::{GoogleClosedMount, RemoteRequestBound};
pub(crate) use mcp_operation::PersonalMcpEffectPermit;
#[cfg(test)]
pub(in crate::google_identity) use remote_claim::testing as remote_testing;
pub(crate) use native_operation::{ClosedNativeFailure, PersonalNativeEffectPermit};
use crate::google_identity::home_source::{HomeOAuthBearer, HomeOAuthResolved};
#[cfg(test)]
pub(in crate::google_identity) use host_claim::live_key as host_live_key;
pub(in crate::google_identity) use host_claim::{Clock, SelectionStore};
#[cfg(test)]
pub(in crate::google_identity) use host_claim::{MAX_SELECTIONS_PER_AGENT, MAX_TOTAL_BYTES};

#[cfg(test)]
#[path = "effect_authority_tests.rs"]
mod tests;

fn changed() -> Refusal {
    Refusal::hard(
        "identity_changed",
        "Google 帳號或本次操作的授權已變更，請重新選擇帳號並核准。",
    )
}
#[cfg(test)]
fn binding(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}

// Purpose is deliberately not a wire type, and carries no secret.
enum GooglePurpose {
    Dedicated {
        native_tool: String,
        final_arguments_digest: String,
        credential_ref: CredentialRef,
    },
    ClosedRemote(RemoteRequestBound),
    #[cfg(test)]
    RemoteMcp {
        mount_binding: String,
        operation_binding: String,
    },
}
impl GooglePurpose {
    fn scope_key(&self) -> &str {
        match self {
            Self::Dedicated { native_tool, .. } => native_tool,
            Self::ClosedRemote(_) => "google_remote_mcp",
            #[cfg(test)]
            Self::RemoteMcp { .. } => "google_remote_mcp",
        }
    }
}

/// Only live host code supplies requester and directory. No environment,
/// serialized input, or model-selected account can construct this context.
pub(crate) struct GoogleCallContext {
    home: PathBuf,
    agent_dir: PathBuf,
    agent: String,
    requester: RequesterIdentity,
    people: Box<dyn People>,
    purpose: GooglePurpose,
}
impl GoogleCallContext {
    fn canonical(home: &Path, agent_dir: &Path) -> Result<(PathBuf, PathBuf, String), Refusal> {
        let home = home.canonicalize().map_err(|_| changed())?;
        let agents = home.join("agents").canonicalize().map_err(|_| changed())?;
        let dir = agent_dir.canonicalize().map_err(|_| changed())?;
        let agent = dir
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|id| duduclaw_core::is_valid_agent_id(id))
            .ok_or_else(changed)?
            .to_owned();
        if dir.parent() != Some(agents.as_path()) || !dir.join("agent.toml").is_file() {
            return Err(changed());
        }
        Ok((home, dir, agent))
    }

    /// Constructible only by this owner and its private real-claim child.
    /// External effect senders remain separately gated.
    fn dedicated(
        home: &Path,
        agent: &str,
        people: Box<dyn People>,
        native_tool: &str,
        final_arguments_digest: String,
        credential_ref: CredentialRef,
    ) -> Result<Self, Refusal> {
        if gi::native_google_scope(native_tool).is_none()
            || !gi::google_digest_well_formed(&final_arguments_digest)
            || !credential_ref.is_well_formed()
            || !duduclaw_core::is_valid_agent_id(agent)
        {
            return Err(changed());
        }
        let (home, agent_dir, canonical_agent) =
            Self::canonical(home, &home.join("agents").join(agent))?;
        if canonical_agent != agent {
            return Err(changed());
        }
        // This lane's authenticated caller is Unknown, never an inferred owner.
        Ok(Self {
            home,
            agent_dir,
            agent: canonical_agent,
            requester: RequesterIdentity::Unknown,
            people,
            purpose: GooglePurpose::Dedicated {
                native_tool: native_tool.into(),
                final_arguments_digest,
                credential_ref,
            },
        })
    }

    /// M1 must capture the actual task-local requester before offloading and
    /// bind each operation anew; a registry's previous caller is not authority.
    #[cfg(test)]
    fn remote_mcp(
        home: &Path,
        agent_dir: &Path,
        requester: RequesterIdentity,
        people: Box<dyn People>,
        mount_binding: String,
        operation_binding: String,
    ) -> Result<Self, Refusal> {
        if !binding(&mount_binding) || !binding(&operation_binding) {
            return Err(changed());
        }
        let (home, agent_dir, agent) = Self::canonical(home, agent_dir)?;
        Ok(Self {
            home,
            agent_dir,
            agent,
            requester,
            people,
            purpose: GooglePurpose::RemoteMcp {
                mount_binding,
                operation_binding,
            },
        })
    }

    fn validate(&self) -> Result<(), Refusal> {
        let (home, dir, agent) = Self::canonical(&self.home, &self.agent_dir)?;
        if home != self.home || dir != self.agent_dir || agent != self.agent {
            return Err(changed());
        }
        if !crate::google_workspace::integration_enabled(&self.home) {
            return Err(Refusal::hard("disabled", "管理員尚未開啟 Google 整合。"));
        }
        grants::check_metadata_barrier(&self.home)
            .map_err(|m| Refusal::hard("grant_uncertain", m))?;
        if let GooglePurpose::Dedicated {
            native_tool,
            credential_ref,
            ..
        } = &self.purpose
        {
            super::credential_registry::authorize(
                &self.home,
                credential_ref,
                &self.agent,
                native_tool,
            )
            .map_err(|_| changed())?;
        }
        if let GooglePurpose::ClosedRemote(bound) = &self.purpose {
            bound.validate_context(self)?;
        }
        Ok(())
    }
}

/// Neither this value nor its token is Clone, Debug, Serialize or a string view.
/// The only transition consumes it and acquires a new effect lease.
enum Material {
    Personal(PinnedPersonalToken),
    HomeOAuth(HomeOAuthResolved),
    Variant(variants::Material),
}
enum MaterialClaim {
    Dedicated(host_claim::ClaimedSelection),
    Remote(remote_claim::ClaimedRemoteCall),
    #[cfg(test)]
    Fixture,
}
/// Only host_claim constructs the production pending call after consuming
/// the original private entry. No context/selection DTO is accepted here.
pub(in crate::google_identity) struct PendingGoogleCall {
    context: GoogleCallContext,
    claim: host_claim::ClaimedSelection,
}
impl PendingGoogleCall {
    pub(in crate::google_identity) async fn resolve(
        self,
    ) -> Result<ResolvedGoogleMaterial, Refusal> {
        host_claim::resolve_claimed(self).await
    }
    pub(in crate::google_identity) fn is_metadata_status(&self) -> bool {
        self.context.purpose.scope_key() == "google_status"
    }
    pub(in crate::google_identity) fn metadata_status(self) -> Result<serde_json::Value, Refusal> {
        host_claim::metadata_status(self)
    }
    #[cfg(test)]
    pub(in crate::google_identity) fn check_resolved(
        &self,
        resolved: &crate::google_identity::resolve::Resolved,
    ) -> Result<(), Refusal> {
        self.claim.check_legacy_resolved(resolved)
    }
}
pub(crate) struct ResolvedGoogleMaterial {
    context: GoogleCallContext,
    claim: MaterialClaim,
    selected: Material,
}

// Compatibility only for the original owned-fixture tests. Production has
// no arbitrary selected/context factory: it must consume the real host claim.
#[cfg(test)]
async fn resolve_pinned(
    context: GoogleCallContext,
    selected: PersonalSelection,
) -> Result<ResolvedGoogleMaterial, Refusal> {
    context.validate()?;
    let personal = access::resolve_pinned(
        &context.home,
        &context.agent,
        &context.requester,
        context.people.as_ref(),
        &selected,
        context.purpose.scope_key(),
    )
    .await?;
    // Resolve's refresh mutex and temporary grant have already been dropped.
    context.validate()?;
    personal.validate(
        &context.home,
        &context.agent,
        &context.requester,
        context.people.as_ref(),
        context.purpose.scope_key(),
    )?;
    Ok(ResolvedGoogleMaterial {
        context,
        claim: MaterialClaim::Fixture,
        selected: Material::Personal(personal),
    })
}
impl ResolvedGoogleMaterial {
    fn validate_material(&self) -> Result<(), Refusal> {
        match &self.claim {
            MaterialClaim::Dedicated(claim) => {
                claim.validate_material(&self.context, &self.selected)
            }
            MaterialClaim::Remote(claim) => claim.validate_material(&self.context, &self.selected),
            #[cfg(test)]
            MaterialClaim::Fixture => match &self.selected {
                Material::Personal(token) => token.validate(
                    &self.context.home,
                    &self.context.agent,
                    &self.context.requester,
                    self.context.people.as_ref(),
                    self.context.purpose.scope_key(),
                ),
                _ => Err(changed()),
            },
        }
    }
    #[cfg(test)]
    fn fixture_token_matches(&self, expected: &str) -> bool {
        matches!(&self.selected, Material::Personal(p) if p.fixture_token_matches(expected))
    }
    pub(crate) async fn acquire_effect(self) -> Result<GoogleEffectAuthority, Refusal> {
        self.context.validate()?;
        let lease = grants::acquire(&self.context.home)
            .await
            .map_err(|m| Refusal::hard("grant_uncertain", m))?;
        let authority = GoogleEffectAuthority {
            material: self,
            _lease: lease,
        };
        authority.validate()?;
        Ok(authority)
    }
}

/// An owned lease lasts through the effect adapter's last handle. Dropping or
/// cancelling this value cannot leave a reusable unleased bearer behind.
pub(crate) struct GoogleEffectAuthority {
    material: ResolvedGoogleMaterial,
    _lease: grants::Lease,
}
impl GoogleEffectAuthority {
    /// Metadata-only: no key access, unseal, refresh, status normalization,
    /// refresh mutex or new grant acquisition occurs in the effect phase.
    pub(crate) fn validate(&self) -> Result<(), Refusal> {
        let m = &self.material;
        m.context.validate()?;
        m.validate_material()
    }
    fn home_permit_for_native<'a>(
        &'a self,
        tool: &str,
        final_digest: &str,
        reference: &CredentialRef,
    ) -> Result<HomeOAuthEffectPermit<'a>, Refusal> {
        if tool == "google_status" {
            return Err(changed());
        }
        self.validate_dedicated(tool, final_digest, reference)?;
        if !matches!(&self.material.selected, Material::HomeOAuth(_)) {
            return Err(changed());
        }
        Ok(HomeOAuthEffectPermit { authority: self })
    }
    fn borrow_home_native<'a>(
        &'a self,
        tool: &str,
        final_digest: &str,
        reference: &CredentialRef,
    ) -> Result<HomeOAuthBearer<'a>, Refusal> {
        let permit = self.home_permit_for_native(tool, final_digest, reference)?;
        match &self.material.selected {
            Material::HomeOAuth(material) => material.borrow_for_effect(permit),
            _ => Err(changed()),
        }
    }
    #[cfg(test)]
    pub(crate) fn validate_remote(&self, mount: &str, operation: &str) -> Result<(), Refusal> {
        match &self.material.context.purpose {
            #[cfg(test)]
            GooglePurpose::RemoteMcp {
                mount_binding,
                operation_binding,
            } if mount_binding == mount && operation_binding == operation => self.validate(),
            _ => Err(changed()),
        }
    }
    pub(crate) fn validate_dedicated(
        &self,
        tool: &str,
        final_digest: &str,
        reference: &CredentialRef,
    ) -> Result<(), Refusal> {
        match &self.material.context.purpose {
            GooglePurpose::Dedicated {
                native_tool,
                final_arguments_digest,
                credential_ref,
            } if native_tool == tool
                && final_arguments_digest == final_digest
                && credential_ref == reference =>
            {
                self.validate()
            }
            _ => Err(changed()),
        }
    }
}

/// E1-only constructor, borrowed authority lifetime, no capability DTO/clone.
/// H1 recognizes this value but cannot independently construct it.
pub(crate) struct HomeOAuthEffectPermit<'a> {
    authority: &'a GoogleEffectAuthority,
}
impl HomeOAuthEffectPermit<'_> {
    pub(crate) fn validate_home_oauth(&self, actual: &HomeOAuthResolved) -> Result<(), Refusal> {
        match &self.authority.material.selected {
            Material::HomeOAuth(expected) if std::ptr::eq(expected, actual) => {
                self.authority.validate()
            }
            Material::Variant(expected) if expected.oauth_is(actual) => self.authority.validate(),
            _ => Err(changed()),
        }
    }
}

/// Concrete owner permits: only E1 and its private operation children construct.
pub(crate) struct AssertionEffectPermit<'a> {
    authority: &'a GoogleEffectAuthority,
}
impl AssertionEffectPermit<'_> {
    pub(crate) fn validate_home_sa(
        &self,
        actual: &crate::google_identity::home_source::sa_material::Resolved,
    ) -> Result<(), Refusal> {
        match &self.authority.material.selected {
            Material::Variant(material) if material.home_sa_is(actual) => self.authority.validate(),
            _ => Err(changed()),
        }
    }
    pub(crate) fn validate_delegate(
        &self,
        actual: &crate::google_identity::delegation_key::w3_material::Resolved,
    ) -> Result<(), Refusal> {
        match &self.authority.material.selected {
            Material::Variant(material) if material.delegate_is(actual) => {
                self.authority.validate()
            }
            _ => Err(changed()),
        }
    }
    pub(crate) fn validate_native(
        &self,
        template: &crate::google_workspace::native_call::NativeCallTemplate,
    ) -> Result<(), Refusal> {
        self.authority.validate_native_template(template)
    }
    pub(crate) fn validate_mcp(&self, bound: &RemoteRequestBound) -> Result<(), Refusal> {
        mcp_operation::validate_bound(self.authority, bound)
    }
    #[cfg(test)]
    pub(crate) fn test_endpoint(&self, original: &str) -> Option<reqwest::Url> {
        crate::google_workspace::native_call::testing::endpoint(
            &self.authority.material.context.home,
            original,
        )
    }
    #[cfg(test)]
    pub(crate) fn test_native_boundary(&self, stage: &str) {
        crate::google_workspace::native_call::testing::boundary(
            &self.authority.material.context.home,
            stage,
        )
    }
    #[cfg(test)]
    pub(crate) fn test_completed(&self, value: &serde_json::Value) {
        crate::google_workspace::native_call::testing::completed(
            &self.authority.material.context.home,
            value,
        )
    }
    #[cfg(test)]
    pub(crate) fn test_mcp_boundary(&self, stage: &str) {
        remote_claim::testing::boundary(&self.authority.material.context.home, stage)
    }
    #[cfg(test)]
    pub(crate) fn test_mcp_completed(&self, reply: &duduclaw_llm::OwnedScopedReply) {
        remote_claim::testing::completed(&self.authority.material.context.home, reply)
    }
}
pub(crate) struct BridgeEffectPermit<'a> {
    authority: &'a GoogleEffectAuthority,
}
impl BridgeEffectPermit<'_> {
    pub(crate) fn validate(
        &self,
        actual: &crate::google_identity::home_source::bridge_material::Resolved,
        template: &crate::google_workspace::native_call::NativeCallTemplate,
    ) -> Result<(), Refusal> {
        match &self.authority.material.selected {
            Material::Variant(material) if material.bridge_is(actual) => {
                self.authority.validate_native_template(template)
            }
            _ => Err(changed()),
        }
    }
    #[cfg(test)]
    pub(crate) fn test_endpoint(&self, original: &str) -> Option<reqwest::Url> {
        crate::google_workspace::native_call::testing::endpoint(
            &self.authority.material.context.home,
            original,
        )
    }
    #[cfg(test)]
    pub(crate) fn test_boundary(&self, stage: &str) {
        crate::google_workspace::native_call::testing::boundary(
            &self.authority.material.context.home,
            stage,
        )
    }
    #[cfg(test)]
    pub(crate) fn test_completed(&self, value: &serde_json::Value) {
        crate::google_workspace::native_call::testing::completed(
            &self.authority.material.context.home,
            value,
        )
    }
}
pub(in crate::google_identity) async fn execute_immediate_native(
    home: &Path,
    users: Option<&std::sync::Arc<duduclaw_auth::UserDb>>,
    agent: &str,
    reference: &CredentialRef,
    tool: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, Refusal> {
    // Initial admission keeps the public credential-refusal taxonomy. Once
    // captured, every later wait still rechecks the original source context.
    host_claim::live_key(home, agent, reference, tool)?;
    let context = GoogleCallContext::dedicated(
        home,
        agent,
        crate::google_identity::requester_gate::people_for(home, users),
        tool,
        gi::google_arguments_digest(&args),
        reference.clone(),
    )?;
    context.validate()?;
    if tool != "google_status" {
        let template =
            match crate::google_workspace::native_call::NativeCallTemplate::prepare(tool, &args)
                .and_then(|t| {
                    t.validate_closed()?;
                    Ok(t)
                }) {
                Ok(template) => template,
                Err(message) => {
                    crate::google_identity::execute::audit(
                        home,
                        agent,
                        tool,
                        "",
                        false,
                        false,
                        "native_arguments_invalid",
                    );
                    return Ok(crate::google_workspace::native_call::tool_error(&message));
                }
            };
        if !crate::google_workspace::native_legacy::first_recipe_fits(&template).await {
            context.validate()?;
            crate::google_identity::execute::audit(
                home,
                agent,
                tool,
                "",
                false,
                false,
                "native_recipe_too_large",
            );
            return Ok(crate::google_workspace::native_call::tool_error(
                "Google 請求的網址與本文超過 1 MiB 上限。",
            ));
        }
    }
    #[cfg(test)]
    if tool != "google_status" && !crate::google_workspace::native_call::testing::required(home) {
        return Err(Refusal::hard(
            "personal_external_unsupported",
            "此 Google 操作缺少受控測試端點，未執行資料操作。",
        ));
    }
    let pending = host_claim::prepare_immediate(context).await?;
    if pending.is_metadata_status() {
        pending.metadata_status()
    } else {
        pending.execute_native(args).await
    }
}

// New credential resolution borrows the original host proof, never a raw-key
// callback or an ambient requester. This permit is not an effect lease.
enum PreflightOriginal<'a> {
    Home,
    Personal(&'a PersonalSelection),
    Delegate {
        email: &'a str,
        precheck: Option<&'static str>,
    },
}
pub(crate) struct GooglePreflightPermit<'a> {
    context: &'a GoogleCallContext,
    source: &'a crate::google_identity::home_source::w3_observation::SourceSnapshot,
    dependency: Option<&'a crate::google_identity::home_source::w3_observation::LocalDependency>,
    original: PreflightOriginal<'a>,
    deadline: tokio::time::Instant,
}
impl GooglePreflightPermit<'_> {
    pub(crate) fn validate(&self) -> Result<(), Refusal> {
        self.source.validate()?;
        self.validate_original()
    }
}

impl<'a> PreflightOriginal<'a> {
    fn borrowed(&self) -> PreflightOriginal<'a> {
        match self {
            Self::Home => Self::Home,
            Self::Personal(original) => Self::Personal(original),
            Self::Delegate { email, precheck } => Self::Delegate {
                email,
                precheck: *precheck,
            },
        }
    }
}

pub(crate) enum CredentialResolveFailure {
    Authority(Refusal),
    Credential(Refusal),
}
impl From<Refusal> for CredentialResolveFailure {
    fn from(error: Refusal) -> Self {
        Self::Credential(error)
    }
}
impl CredentialResolveFailure {
    fn into_refusal(self) -> Refusal {
        match self {
            Self::Authority(error) | Self::Credential(error) => error,
        }
    }
}
impl GooglePreflightPermit<'_> {
    // C1 validates its own evolving OAuth row across publication. W3 checks
    // every other original dependency here, avoiding a false failure on C1's
    // own token refresh. The completed row is rebound to the exact proof.
    pub(crate) fn validate_oauth_resolution(&self) -> Result<(), Refusal> {
        self.source.validate_without_oauth_control()?;
        self.validate_original()
    }
    fn validate_original(&self) -> Result<(), Refusal> {
        if tokio::time::Instant::now() >= self.deadline {
            return Err(Refusal::hard("selection_expired", "Google 帳號準備逾時。"));
        }
        self.context.validate()?;
        if let Some(dep) = self.dependency {
            dep.validate()?;
        }
        match &self.original {
            PreflightOriginal::Home => {
                if !matches!(
                    gi::effective_for(&self.context.home, Some(&self.context.agent)).effective,
                    gi::Effective::Home
                ) {
                    return Err(changed());
                }
            }
            PreflightOriginal::Personal(original) => original.validate_original_unavailable(
                &self.context.home,
                &self.context.agent,
                &self.context.requester,
                self.context.people.as_ref(),
                self.context.purpose.scope_key(),
            )?,
            PreflightOriginal::Delegate { email, precheck } => {
                let policy = gi::effective_for(&self.context.home, Some(&self.context.agent));
                if !matches!(&policy.effective,gi::Effective::Delegate{email:actual} if actual==email)
                {
                    return Err(changed());
                }
                crate::google_identity::requester_gate::check(
                    policy.requesters,
                    &self.context.requester,
                    &self.context.agent,
                    email,
                    self.context.people.as_ref(),
                )?;
                if crate::google_identity::resolve::precheck(
                    &self.context.home,
                    email,
                    self.context.people.as_ref(),
                )
                .err()
                .map(|r| r.code)
                    != *precheck
                {
                    return Err(changed());
                }
            }
        }
        if tokio::time::Instant::now() >= self.deadline {
            return Err(Refusal::hard("selection_expired", "Google 帳號準備逾時。"));
        }
        Ok(())
    }
}
