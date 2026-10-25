//! W3 preflight/original choice. Constructors belong to actual E1 hosts only.
use super::*;
use crate::google_identity::{
    home_source::{
        self,
        w3_observation::{SourceSnapshot, LocalDependency},
        sa_material, bridge_material,
    },
    delegation_key::w3_material as delegate_material,
};
use serde_json::{Value, json};
use std::time::Duration;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectKind {
    ServiceAccount,
    OAuth,
}
struct HistoricalDelegateFailure {
    reason: &'static str,
    attempt: String,
}
struct DirectAttemptFailure {
    kind: DirectKind,
    reason: &'static str,
    attempt: String,
}
impl DirectAttemptFailure {
    fn new(kind: DirectKind, reason: &'static str) -> Self {
        Self {
            kind,
            reason,
            attempt: uuid::Uuid::new_v4().to_string(),
        }
    }
    #[cfg(test)]
    fn reason(&self) -> &str {
        self.reason
    }
    #[cfg(test)]
    fn kind(&self) -> DirectKind {
        self.kind
    }
    #[cfg(test)]
    fn attempt_id(&self) -> &str {
        &self.attempt
    }
}
enum Choice {
    Sa { subject: String },
    OAuthNative(home_source::NativeHomeSelection),
    OAuthRemote(home_source::HomeSourceReceipt),
    Bridge { failure: DirectAttemptFailure },
    BridgeMetadata,
    Delegate { email: String },
}
enum OwnerMaterial {
    Sa(sa_material::Resolved),
    OAuth(home_source::HomeOAuthResolved),
    Bridge(bridge_material::Resolved),
    Delegate(delegate_material::Resolved),
}
pub(super) struct Material {
    owner: OwnerMaterial,
    nonce: String,
}
impl Material {
    fn new(owner: OwnerMaterial) -> Self {
        Self {
            owner,
            nonce: uuid::Uuid::new_v4().to_string(),
        }
    }
    fn validate(&self) -> Result<(), Refusal> {
        self.owner.validate()
    }
    fn live(&self) -> bool {
        self.owner.live()
    }
    fn weight(&self) -> usize {
        std::mem::size_of_val(self) + self.nonce.capacity() + self.owner.weight()
    }
    fn source_digest(&self) -> String {
        self.owner.source_digest()
    }
    pub(super) fn home_sa_is(&self, v: &sa_material::Resolved) -> bool {
        self.owner.home_sa_is(v)
    }
    pub(super) fn delegate_is(&self, v: &delegate_material::Resolved) -> bool {
        self.owner.delegate_is(v)
    }
    pub(super) fn bridge_is(&self, v: &bridge_material::Resolved) -> bool {
        self.owner.bridge_is(v)
    }
    pub(super) fn oauth_is(&self, v: &home_source::HomeOAuthResolved) -> bool {
        self.owner.oauth_is(v)
    }
    pub(super) async fn native(
        &self,
        a: &GoogleEffectAuthority,
        t: &crate::google_workspace::native_call::NativeCallTemplate,
    ) -> Result<Value, ClosedNativeFailure> {
        self.owner.native(a, t).await
    }
    pub(super) async fn mcp(
        &self,
        a: &GoogleEffectAuthority,
        b: &RemoteRequestBound,
    ) -> Result<duduclaw_llm::OwnedScopedReply, duduclaw_llm::OwnedScopedFailure> {
        self.owner.mcp(a, b).await
    }
    #[cfg(test)]
    pub(super) async fn test_foreign(
        &self,
        a: &GoogleEffectAuthority,
        e: &crate::google_workspace::native_call::NativeCallTemplate,
        o: &crate::google_workspace::native_call::NativeCallTemplate,
    ) -> bool {
        self.owner.test_foreign(a, e, o).await
    }
}
pub(super) struct Prepared {
    source: SourceSnapshot,
    choice: Choice,
    material: Option<Material>,
    material_binding: String,
    dependency: Option<LocalDependency>,
    delegate_original: Option<(String, Option<&'static str>)>,
    delegate_failure: Option<HistoricalDelegateFailure>,
    delegate_dependency: Option<LocalDependency>,
    fallback: bool,
    created: std::time::Instant,
    wall_expiry: u64,
}
fn unavailable() -> Refusal {
    Refusal::hard(
        "account_unavailable",
        "公司共用 Google 帳號暫時無法使用，這次沒有執行資料操作。",
    )
}
fn check(deadline: tokio::time::Instant) -> Result<(), Refusal> {
    if tokio::time::Instant::now() >= deadline {
        Err(Refusal::hard(
            "selection_expired",
            "Google 帳號準備逾時，請重新選擇。",
        ))
    } else {
        Ok(())
    }
}
impl Prepared {
    pub(super) fn metadata(context: &GoogleCallContext, fallback: bool) -> Result<Self, Refusal> {
        let source = SourceSnapshot::capture(&context.home, &context.agent)?;
        let effective = gi::effective_for(&context.home, Some(&context.agent));
        let mut delegate_original = None;
        let mut fallback = fallback;
        if let gi::Effective::Delegate { email } = &effective.effective {
            crate::google_identity::requester_gate::check(
                effective.requesters,
                &context.requester,
                &context.agent,
                email,
                context.people.as_ref(),
            )?;
            let precheck = crate::google_identity::resolve::precheck(
                &context.home,
                email,
                context.people.as_ref(),
            );
            if precheck.is_ok() {
                return Ok(Self {
                    source,
                    choice: Choice::Delegate {
                        email: email.clone(),
                    },
                    material: None,
                    material_binding: String::new(),
                    dependency: None,
                    delegate_original: Some((email.clone(), None)),
                    delegate_failure: None,
                    delegate_dependency: None,
                    fallback: false,
                    created: std::time::Instant::now(),
                    wall_expiry: crate::google_personal::authority::now() + 1800,
                });
            }
            let error = precheck.err().ok_or_else(changed)?;
            if !error.fallback_eligible || effective.fallback != gi::Fallback::Home {
                return Err(error);
            }
            delegate_original = Some((email.clone(), Some(error.code)));
            fallback = true;
        }
        let (_, bytes) = home_source::w3_observation::FileProof::capture(
            &context.home.join("config.toml"),
            1024 * 1024,
        )?;
        let raw = std::str::from_utf8(&bytes).map_err(|_| changed())?;
        let mut dependency = None;
        let choice = match crate::google_service_account::parse_config(raw, &context.home) {
            Ok(Some(config)) => {
                dependency = Some(LocalDependency::capture(&config.key_file, 16 * 1024));
                Choice::Sa {
                    subject: config.subject,
                }
            }
            _ => match home_source::prepare_home_native_selection(&context.home, "google_status") {
                Ok(receipt) => Choice::OAuthNative(receipt),
                Err(_) => {
                    crate::google_apps_script::parse_bridge_section(raw)
                        .map_err(|_| unavailable())?
                        .ok_or_else(unavailable)?;
                    Choice::BridgeMetadata
                }
            },
        };
        let mut source = source;
        source.refresh_after_resolution()?;
        Ok(Self {
            source,
            choice,
            material: None,
            material_binding: String::new(),
            dependency,
            delegate_original,
            delegate_failure: None,
            delegate_dependency: None,
            fallback,
            created: std::time::Instant::now(),
            wall_expiry: crate::google_personal::authority::now() + 1800,
        })
    }
    pub(super) async fn home(
        context: &GoogleCallContext,
        deadline: tokio::time::Instant,
        fallback: bool,
        original: PreflightOriginal<'_>,
    ) -> Result<Self, Refusal> {
        check(deadline)?;
        context.validate()?;
        let mut source = SourceSnapshot::capture(&context.home, &context.agent)?;
        let (_, bytes) = home_source::w3_observation::FileProof::capture(
            &context.home.join("config.toml"),
            1024 * 1024,
        )?;
        let raw = std::str::from_utf8(&bytes).map_err(|_| changed())?;
        let parsed = crate::google_service_account::parse_config(raw, &context.home);
        let mut dependency = None;
        let (direct_kind, attempt) = match parsed {
            Ok(Some(config)) => {
                dependency = Some(LocalDependency::capture(&config.key_file, 16 * 1024));
                let subject = config.subject.clone();
                let permit = GooglePreflightPermit {
                    context,
                    source: &source,
                    dependency: dependency.as_ref(),
                    original: original.borrowed(),
                    deadline,
                };
                let result = sa_material::resolve(&context.home, &config, deadline, &permit)
                    .await
                    .map(|token| (Choice::Sa { subject }, OwnerMaterial::Sa(token)));
                (DirectKind::ServiceAccount, result)
            }
            Err(_) => (
                DirectKind::ServiceAccount,
                Err(CredentialResolveFailure::Credential(unavailable())),
            ),
            Ok(None) => {
                let result = if context.purpose.scope_key() == "google_remote_mcp" {
                    match home_source::prepare_home_selection(
                        &context.home,
                        crate::google_workspace::REQUIRED_SCOPES,
                    ) {
                        Ok(receipt) => {
                            source.adopt_prepared_oauth(
                                receipt.credential_id(),
                                receipt.logical_epoch(),
                                receipt.source_generation(),
                            )?;
                            let permit = GooglePreflightPermit {
                                context,
                                source: &source,
                                dependency: None,
                                original: original.borrowed(),
                                deadline,
                            };
                            permit.validate()?;
                            let pending = home_source::resolve_home_oauth_preflight(
                                &context.home,
                                &receipt,
                                crate::google_workspace::REQUIRED_SCOPES,
                                deadline,
                                &permit,
                            )
                            .await;
                            permit.validate_oauth_resolution()?;
                            pending
                                .map(|token| {
                                    (Choice::OAuthRemote(receipt), OwnerMaterial::OAuth(token))
                                })
                                .map_err(|_| CredentialResolveFailure::Credential(unavailable()))
                        }
                        Err(_) => Err(CredentialResolveFailure::Credential(unavailable())),
                    }
                } else {
                    match home_source::prepare_home_native_selection(
                        &context.home,
                        context.purpose.scope_key(),
                    ) {
                        Ok(receipt) => {
                            let (id, epoch, generation) = receipt.logical_connector();
                            source.adopt_prepared_oauth(id, epoch, generation)?;
                            let permit = GooglePreflightPermit {
                                context,
                                source: &source,
                                dependency: None,
                                original: original.borrowed(),
                                deadline,
                            };
                            permit.validate()?;
                            let pending = home_source::resolve_home_native_preflight(
                                &context.home,
                                &receipt,
                                context.purpose.scope_key(),
                                deadline,
                                &permit,
                            )
                            .await;
                            permit.validate_oauth_resolution()?;
                            pending
                                .map(|token| {
                                    (Choice::OAuthNative(receipt), OwnerMaterial::OAuth(token))
                                })
                                .map_err(|_| CredentialResolveFailure::Credential(unavailable()))
                        }
                        Err(_) => Err(CredentialResolveFailure::Credential(unavailable())),
                    }
                };
                (DirectKind::OAuth, result)
            }
        };
        check(deadline)?;
        context.validate()?;
        // Accept only the resolver's legitimate token publication, never config,
        // client, marker or policy replacement during the single attempt.
        if direct_kind == DirectKind::OAuth && attempt.is_ok() {
            if let Ok((_, OwnerMaterial::OAuth(token))) = &attempt {
                token.validate_w3_control(&context.home)?;
            }
            source.refresh_after_resolution()?;
        } else {
            source.validate()?;
        }
        if let Some(dep) = &dependency {
            dep.validate()?;
        }
        let (choice, material) = match attempt {
            Ok(done) => done,
            Err(CredentialResolveFailure::Authority(error)) => return Err(error),
            Err(CredentialResolveFailure::Credential(error)) => {
                // Timeout/cancel never becomes a second credential attempt.
                check(deadline)?;
                if context.purpose.scope_key() == "google_remote_mcp" {
                    return Err(error);
                }
                let (url, stored) = crate::google_apps_script::parse_bridge_section(raw)
                    .map_err(|_| unavailable())?
                    .ok_or_else(unavailable)?;
                let failure = DirectAttemptFailure::new(direct_kind, error.code);
                source.validate()?;
                check(deadline)?;
                let permit = GooglePreflightPermit {
                    context,
                    source: &source,
                    dependency: dependency.as_ref(),
                    original: original.borrowed(),
                    deadline,
                };
                let token =
                    bridge_material::resolve(&context.home, url, stored, deadline, &permit).await?;
                (Choice::Bridge { failure }, OwnerMaterial::Bridge(token))
            }
        };
        drop(bytes);
        check(deadline)?;
        source.validate()?;
        let material = Material::new(material);
        let material_binding = material.nonce.clone();
        let prepared = Self {
            source,
            choice,
            material: Some(material),
            material_binding,
            dependency,
            delegate_original: None,
            delegate_failure: None,
            delegate_dependency: None,
            fallback,
            created: std::time::Instant::now(),
            wall_expiry: crate::google_personal::authority::now()
                .checked_add(1800)
                .ok_or_else(changed)?,
        };
        prepared.validate(context)?;
        Ok(prepared)
    }
    pub(super) async fn immediate(
        context: &GoogleCallContext,
        deadline: tokio::time::Instant,
    ) -> Result<Self, Refusal> {
        check(deadline)?;
        context.validate()?;
        let effective = gi::effective_for(&context.home, Some(&context.agent));
        match &effective.effective {
            gi::Effective::Home => {
                Self::home(context, deadline, false, PreflightOriginal::Home).await
            }
            gi::Effective::Delegate { email } => {
                // Requester and hard protection failures are outside fallback.
                super::super::requester_gate::check(
                    effective.requesters,
                    &context.requester,
                    &context.agent,
                    email,
                    context.people.as_ref(),
                )?;
                let source = SourceSnapshot::capture(&context.home, &context.agent)?;
                let dependency = LocalDependency::capture(
                    &super::super::delegation_key::path(&context.home),
                    64 * 1024,
                );
                let precheck =
                    super::super::resolve::precheck(&context.home, email, context.people.as_ref());
                let precheck_error = precheck.as_ref().err().map(|r| r.code);
                let permit = GooglePreflightPermit {
                    context,
                    source: &source,
                    dependency: Some(&dependency),
                    original: PreflightOriginal::Delegate {
                        email,
                        precheck: precheck_error,
                    },
                    deadline,
                };
                let attempt = match precheck {
                    Ok(()) => {
                        delegate_material::resolve(&context.home, email, deadline, &permit).await
                    }
                    Err(error) => Err(CredentialResolveFailure::Credential(error)),
                };
                check(deadline)?;
                context.validate()?;
                source.validate()?;
                dependency.validate()?;
                match attempt {
                    Ok(material) => {
                        let material = Material::new(OwnerMaterial::Delegate(material));
                        let material_binding = material.nonce.clone();
                        let prepared = Self {
                            source,
                            choice: Choice::Delegate {
                                email: email.clone(),
                            },
                            material: Some(material),
                            material_binding,
                            dependency: Some(dependency),
                            delegate_original: Some((email.clone(), precheck_error)),
                            delegate_failure: None,
                            delegate_dependency: None,
                            fallback: false,
                            created: std::time::Instant::now(),
                            wall_expiry: crate::google_personal::authority::now() + 1800,
                        };
                        prepared.validate(context)?;
                        Ok(prepared)
                    }
                    Err(CredentialResolveFailure::Credential(error))
                        if error.fallback_eligible && effective.fallback == gi::Fallback::Home =>
                    {
                        let mut prepared = Self::home(
                            context,
                            deadline,
                            true,
                            PreflightOriginal::Delegate {
                                email,
                                precheck: precheck_error,
                            },
                        )
                        .await?;
                        // Preserve the exact original Delegate attempt and key
                        // dependency even when Home has its own SA dependency.
                        prepared.delegate_original = Some((email.clone(), precheck_error));
                        prepared.delegate_failure = Some(HistoricalDelegateFailure {
                            reason: error.code,
                            attempt: uuid::Uuid::new_v4().to_string(),
                        });
                        prepared.source.validate()?;
                        source.validate()?;
                        dependency.validate()?;
                        prepared.delegate_dependency = Some(dependency);
                        prepared.validate(context)?;
                        Ok(prepared)
                    }
                    Err(error) => Err(error.into_refusal()),
                }
            }
            other => Err(super::super::resolve::refusal_for(other).unwrap_or_else(changed)),
        }
    }
    pub(super) fn validate(&self, context: &GoogleCallContext) -> Result<(), Refusal> {
        context.validate()?;
        self.source.validate()?;
        if !self.live() {
            return Err(changed());
        }
        if let Some(dep) = &self.dependency {
            dep.validate()?;
        }
        if let Some(dep) = &self.delegate_dependency {
            dep.validate()?;
        }
        if let Some((email, precheck_error)) = &self.delegate_original {
            let policy = gi::effective_for(&context.home, Some(&context.agent));
            if !matches!(&policy.effective,gi::Effective::Delegate{email:actual} if actual==email) {
                return Err(changed());
            }
            super::super::requester_gate::check(
                policy.requesters,
                &context.requester,
                &context.agent,
                email,
                context.people.as_ref(),
            )?;
            let actual =
                super::super::resolve::precheck(&context.home, email, context.people.as_ref());
            if actual.as_ref().err().map(|r| r.code) != *precheck_error {
                return Err(changed());
            }
        }
        if let Some(material) = &self.material {
            material.validate()?;
            if let OwnerMaterial::OAuth(token) = &material.owner {
                token.validate_w3_control(&context.home)?;
            }
        }
        Ok(())
    }
    pub(super) fn live(&self) -> bool {
        crate::google_personal::authority::now() < self.wall_expiry
            && self.created.elapsed() < Duration::from_secs(1800)
            && self.material.as_ref().is_none_or(Material::live)
    }
    pub(super) fn take(&mut self) -> Result<Material, Refusal> {
        self.material.take().ok_or_else(changed)
    }
    pub(super) fn validate_material(
        &self,
        context: &GoogleCallContext,
        material: &Material,
    ) -> Result<(), Refusal> {
        self.validate(context)?;
        if self.material_binding != material.nonce {
            return Err(changed());
        }
        material.validate()?;
        if let OwnerMaterial::OAuth(token) = &material.owner {
            token.validate_w3_control(&context.home)?;
        }
        if matches!(
            (&self.choice, &material.owner),
            (Choice::Sa { .. }, OwnerMaterial::Sa(_))
                | (Choice::OAuthNative(_), OwnerMaterial::OAuth(_))
                | (Choice::OAuthRemote(_), OwnerMaterial::OAuth(_))
                | (Choice::Bridge { .. }, OwnerMaterial::Bridge(_))
                | (Choice::Delegate { .. }, OwnerMaterial::Delegate(_))
        ) {
            Ok(())
        } else {
            Err(changed())
        }
    }
    pub(super) fn weight(&self) -> usize {
        std::mem::size_of_val(self)
            + self.material_binding.capacity()
            + self
                .delegate_failure
                .as_ref()
                .map_or(0, |f| f.attempt.capacity())
            + self.source.weight()
            + self.dependency.as_ref().map_or(0, LocalDependency::weight)
            + self
                .delegate_dependency
                .as_ref()
                .map_or(0, LocalDependency::weight)
            + self.material.as_ref().map_or(0, Material::weight)
            + match &self.choice {
                Choice::Sa { subject } => subject.capacity(),
                Choice::Bridge { failure } => failure.attempt.capacity(),
                Choice::Delegate { email } => email.capacity(),
                Choice::OAuthNative(v) => v.storage_weight().unwrap_or(usize::MAX / 2),
                Choice::OAuthRemote(v) => v.storage_weight(),
                Choice::BridgeMetadata => 0,
            }
            + self
                .delegate_original
                .as_ref()
                .map_or(0, |(email, _)| email.capacity())
    }
    pub(super) fn projection(&self) -> Value {
        json!({"material_binding":self.material_binding,"source":self.source.digest(),"choice":self.label(),"material":self.material.as_ref().map(Material::source_digest),"historical_delegate_attempt":self.delegate_failure.as_ref().map(|f|json!({"reason":f.reason,"attempt":f.attempt})),"historical_direct_attempt":match &self.choice{Choice::Bridge{failure}=>Some(json!({"id":failure.attempt,"kind":match failure.kind{DirectKind::ServiceAccount=>"service_account",DirectKind::OAuth=>"oauth"},"reason":failure.reason})),_=>None}})
    }
    fn label(&self) -> &'static str {
        match self.choice {
            Choice::Sa { .. } => "home_service_account",
            Choice::OAuthNative(_) | Choice::OAuthRemote(_) => "home_oauth",
            Choice::Bridge { .. } | Choice::BridgeMetadata => "home_apps_script",
            Choice::Delegate { .. } => "delegate",
        }
    }
    pub(super) fn fallback(&self) -> bool {
        self.fallback
    }
    pub(super) fn audit_account(&self) -> &str {
        match &self.choice {
            Choice::Delegate { email } => email,
            _ => "",
        }
    }
    pub(super) fn approval(
        &self,
        original: &PersonalSelection,
        digest: String,
    ) -> Result<gi::GoogleAccountApproval, Refusal> {
        let (selector, unavailable) = original.fallback_description()?;
        let mut approval = gi::GoogleAccountApproval {
            selected_identity: gi::ApprovedGoogleIdentity::HomeOAuthFallback,
            selected_google_account: String::new(),
            selected_credential_owner: String::new(),
            selected_person_login: String::new(),
            requester_kind: "unknown".into(),
            configured_home_fallback: true,
            home_fallback_authorized: true,
            account_binding_digest: digest,
            home_connection: None,
            home_source: None,
        };
        match &self.choice {
            Choice::OAuthNative(receipt) => {
                let (id, epoch, generation) = receipt.logical_connector();
                approval.home_connection = Some(gi::GoogleHomeConnectionApproval {
                    credential_id: id.into(),
                    logical_epoch: epoch,
                    source_generation: generation.into(),
                    original_selector: selector,
                    original_unavailable: unavailable,
                });
            }
            Choice::Sa { subject } => {
                approval.selected_identity = gi::ApprovedGoogleIdentity::HomeServiceAccountFallback;
                approval.home_source = Some(gi::GoogleHomeSourceApproval::ServiceAccount {
                    source_binding_digest: self.source.digest(),
                    source_generation: self.source.generation(),
                    declared_subject: Some(subject.clone()),
                    original_selector: selector,
                    original_unavailable: unavailable,
                });
            }
            Choice::Bridge { .. } | Choice::BridgeMetadata => {
                approval.selected_identity = gi::ApprovedGoogleIdentity::HomeAppsScriptFallback;
                approval.home_source = Some(gi::GoogleHomeSourceApproval::AppsScript {
                    source_binding_digest: self.source.digest(),
                    source_generation: self.source.generation(),
                    original_selector: selector,
                    original_unavailable: unavailable,
                });
            }
            _ => return Err(changed()),
        }
        Ok(approval)
    }
}
impl OwnerMaterial {
    fn validate(&self) -> Result<(), Refusal> {
        match self {
            Self::Sa(v) => v.validate(),
            Self::Delegate(v) => v.validate(),
            Self::Bridge(v) => v.validate(),
            Self::OAuth(v) => v.validate_w3_snapshot(),
        }
    }
    fn live(&self) -> bool {
        match self {
            Self::Sa(v) => v.live(),
            Self::Delegate(v) => v.live(),
            Self::Bridge(_) => true,
            Self::OAuth(v) => v.validate_w3_snapshot().is_ok(),
        }
    }
    fn weight(&self) -> usize {
        match self {
            Self::Sa(v) => v.weight(),
            Self::Delegate(v) => v.weight(),
            Self::Bridge(v) => v.weight(),
            Self::OAuth(v) => v.w3_weight(),
        }
    }
    fn source_digest(&self) -> String {
        match self {
            Self::Sa(v) => v.source_digest(),
            Self::Delegate(v) => v.source_digest(),
            Self::Bridge(v) => v.source_digest(),
            Self::OAuth(_) => "captured-oauth-row".into(),
        }
    }
    pub(super) fn home_sa_is(&self, actual: &sa_material::Resolved) -> bool {
        matches!(self,Self::Sa(expected) if std::ptr::eq(expected,actual))
    }
    pub(super) fn delegate_is(&self, actual: &delegate_material::Resolved) -> bool {
        matches!(self,Self::Delegate(expected) if std::ptr::eq(expected,actual))
    }
    pub(super) fn bridge_is(&self, actual: &bridge_material::Resolved) -> bool {
        matches!(self,Self::Bridge(expected) if std::ptr::eq(expected,actual))
    }
    pub(super) fn oauth_is(&self, actual: &home_source::HomeOAuthResolved) -> bool {
        matches!(self,Self::OAuth(expected) if std::ptr::eq(expected,actual))
    }
    pub(super) async fn native(
        &self,
        authority: &GoogleEffectAuthority,
        template: &crate::google_workspace::native_call::NativeCallTemplate,
    ) -> Result<Value, ClosedNativeFailure> {
        match self {
            Self::Sa(v) => v
                .borrow_for_effect(AssertionEffectPermit { authority })
                .map_err(|e| ClosedNativeFailure::validation(e, false))?
                .execute_native(template)
                .await
                .map(|v| v.into_value()),
            Self::Delegate(v) => v
                .borrow_for_effect(AssertionEffectPermit { authority })
                .map_err(|e| ClosedNativeFailure::validation(e, false))?
                .execute_native(template)
                .await
                .map(|v| v.into_value()),
            Self::Bridge(v) => v
                .execute_native(BridgeEffectPermit { authority }, template)
                .await
                .map(|v| v.into_value()),
            Self::OAuth(v) => v
                .borrow_for_effect(HomeOAuthEffectPermit { authority })
                .map_err(|e| ClosedNativeFailure::validation(e, false))?
                .execute_native(template)
                .await
                .map(|v| v.into_value()),
        }
    }
    pub(super) async fn mcp(
        &self,
        authority: &GoogleEffectAuthority,
        bound: &RemoteRequestBound,
    ) -> Result<duduclaw_llm::OwnedScopedReply, duduclaw_llm::OwnedScopedFailure> {
        use duduclaw_llm::OwnedScopedFailure;
        match self {
            Self::Sa(v) => v
                .borrow_for_effect(AssertionEffectPermit { authority })
                .map_err(|_| OwnedScopedFailure::Denied)?
                .execute_mcp(bound)
                .await
                .map(|v| v.into_reply()),
            Self::Delegate(v) => v
                .borrow_for_effect(AssertionEffectPermit { authority })
                .map_err(|_| OwnedScopedFailure::Denied)?
                .execute_mcp(bound)
                .await
                .map(|v| v.into_reply()),
            Self::OAuth(v) => v
                .borrow_for_effect(HomeOAuthEffectPermit { authority })
                .map_err(|_| OwnedScopedFailure::Denied)?
                .execute_mcp(bound)
                .await
                .map(|v| v.into_reply()),
            Self::Bridge(_) => Err(OwnedScopedFailure::Denied),
        }
    }
}

#[cfg(test)]
impl OwnerMaterial {
    pub(super) async fn test_foreign(
        &self,
        authority: &GoogleEffectAuthority,
        expected: &crate::google_workspace::native_call::NativeCallTemplate,
        other: &crate::google_workspace::native_call::NativeCallTemplate,
    ) -> bool {
        match self {
            Self::Sa(v) => {
                v.borrow_for_effect(AssertionEffectPermit { authority })
                    .unwrap()
                    .test_foreign_native_step(expected, other)
                    .await
            }
            Self::Delegate(v) => {
                v.borrow_for_effect(AssertionEffectPermit { authority })
                    .unwrap()
                    .test_foreign_native_step(expected, other)
                    .await
            }
            Self::OAuth(v) => {
                v.borrow_for_effect(HomeOAuthEffectPermit { authority })
                    .unwrap()
                    .test_foreign_native_step(expected, other)
                    .await
            }
            Self::Bridge(_) => false,
        }
    }
}

#[cfg(test)]
pub(in crate::google_identity) mod testing {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    fn fixtures() -> &'static Mutex<std::collections::HashMap<PathBuf, (String, String, String)>> {
        static F: OnceLock<Mutex<std::collections::HashMap<PathBuf, (String, String, String)>>> =
            OnceLock::new();
        F.get_or_init(Default::default)
    }
    pub(in crate::google_identity) struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            fixtures().lock().unwrap().remove(&self.0);
            let _ = std::fs::remove_file(self.0.join(".w3-issuer-owned-fixture"));
        }
    }
    pub(in crate::google_identity) fn install_issuer(
        home: &Path,
        original: &str,
        endpoint: &str,
    ) -> Guard {
        let url = reqwest::Url::parse(endpoint).unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert!(url.port().is_some());
        let home = home.canonicalize().unwrap();
        let nonce = uuid::Uuid::new_v4().to_string();
        std::fs::write(home.join(".w3-issuer-owned-fixture"), &nonce).unwrap();
        assert!(
            fixtures()
                .lock()
                .unwrap()
                .insert(home.clone(), (original.into(), endpoint.into(), nonce))
                .is_none()
        );
        Guard(home)
    }
    pub(crate) fn issuer(home: &Path, original: &str) -> Option<reqwest::Url> {
        let home = home.canonicalize().ok()?;
        let map = fixtures().lock().ok()?;
        let (expected, endpoint, nonce) = map.get(&home)?;
        if expected != original
            || std::fs::read_to_string(home.join(".w3-issuer-owned-fixture"))
                .ok()
                .as_deref()
                != Some(nonce.as_str())
        {
            return None;
        }
        reqwest::Url::parse(endpoint).ok()
    }
}

#[cfg(test)]
mod w3_source_oracles {
    use super::*;
    #[test]
    fn w3_historical_failure_does_not_remint_for_external_recovery() {
        let first = DirectAttemptFailure::new(DirectKind::ServiceAccount, "google_unreachable");
        assert_eq!(first.reason(), "google_unreachable");
        assert_eq!(first.kind(), DirectKind::ServiceAccount);
        assert_ne!(
            first.attempt_id(),
            DirectAttemptFailure::new(DirectKind::ServiceAccount, "google_unreachable")
                .attempt_id()
        );
    }
}

#[cfg(test)]
impl Prepared {
    pub(super) fn expire_for_test(&mut self) {
        self.wall_expiry = 0;
    }
}

#[cfg(test)]
impl Prepared {
    pub(super) fn expire_oauth_mono_for_test(&mut self) {
        match &mut self
            .material
            .as_mut()
            .expect("expected retained material")
            .owner
        {
            OwnerMaterial::OAuth(token) => token.expire_w3_mono_for_test(),
            _ => panic!("expected W3 OAuth owner"),
        }
    }
}
