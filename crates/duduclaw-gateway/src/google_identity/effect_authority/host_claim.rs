//! True host selection, final binding and synchronous one-shot claim. No DTO
//! can construct OriginalAccountUnion, a claim or its parent call context.
use super::*;
use crate::google_identity::home_source::{self, NativeHomeSelection};
use crate::google_identity::requester_gate::people_for;
use duduclaw_auth::UserDb;
use duduclaw_core::google_identity::{
    self as gi, GOOGLE_SELECTION_TTL_SECS, GoogleAccountApproval, GoogleSelectionStamp,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(in crate::google_identity) const MAX_SELECTIONS: usize = 1024;
pub(in crate::google_identity) const MAX_SELECTIONS_PER_AGENT: usize = 32;
pub(in crate::google_identity) const MAX_SELECTION_BYTES: usize = 64 * 1024;
pub(in crate::google_identity) const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone, Copy)]
pub(in crate::google_identity) struct Clock {
    pub(in crate::google_identity) unix: u64,
    pub(in crate::google_identity) mono: Instant,
}
impl Clock {
    pub(in crate::google_identity) fn now() -> Self {
        Self {
            unix: crate::google_personal::authority::now(),
            mono: Instant::now(),
        }
    }
}
fn refused() -> Refusal {
    changed()
}
fn capacity() -> Refusal {
    Refusal::hard(
        "selection_capacity",
        "待核准的 Google 帳號選擇已達上限，請稍後重新操作。",
    )
}
fn home_error(_: crate::mcp_oauth::home_store::Error) -> Refusal {
    refused()
}
fn add(a: usize, b: usize) -> Result<usize, Refusal> {
    a.checked_add(b).ok_or_else(capacity)
}

pub(in crate::google_identity) fn live_key(
    home: &Path,
    agent: &str,
    reference: &CredentialRef,
    tool: &str,
) -> Result<(), Refusal> {
    if gi::native_google_scope(tool).is_none() || !reference.is_well_formed() {
        return Err(refused());
    }
    if !crate::google_workspace::integration_enabled(home) {
        return Err(Refusal::hard("disabled", "管理員尚未開啟 Google 整合。"));
    }
    crate::google_identity::credential_registry::authorize(home, reference, agent, tool)
        .map_err(|m| Refusal::hard("credential_refused", m))?;
    grants::check_metadata_barrier(home).map_err(|m| Refusal::hard("grant_uncertain", m))
}

enum OriginalAccountUnion {
    HealthyPersonal(PersonalSelection),
    PreparedHome {
        original: PersonalSelection,
        prepared: super::variants::Prepared,
    },
    Immediate(super::variants::Prepared),
    UnavailablePersonalHomeOAuth {
        original: PersonalSelection,
        home: NativeHomeSelection,
    },
}
impl OriginalAccountUnion {
    fn validate(&self, context: &GoogleCallContext) -> Result<(), Refusal> {
        let tool = context.purpose.scope_key();
        match self {
            Self::PreparedHome { original, prepared } => {
                original.validate_original_unavailable(
                    &context.home,
                    &context.agent,
                    &context.requester,
                    context.people.as_ref(),
                    tool,
                )?;
                prepared.validate(context)
            }
            Self::Immediate(prepared) => prepared.validate(context),
            Self::HealthyPersonal(original) => {
                original.ensure_healthy_personal()?;
                original.validate(
                    &context.home,
                    &context.agent,
                    &context.requester,
                    context.people.as_ref(),
                    tool,
                )
            }
            Self::UnavailablePersonalHomeOAuth { original, home } => {
                original.validate_original_unavailable(
                    &context.home,
                    &context.agent,
                    &context.requester,
                    context.people.as_ref(),
                    tool,
                )?;
                home_source::validate_home_native_selection(&context.home, home, tool)
                    .map_err(home_error)
            }
        }
    }
    fn binding_projection(&self) -> Result<Value, Refusal> {
        match self {
            Self::PreparedHome { original, prepared } => Ok(
                json!({"original_unavailable_personal":original,"prepared":prepared.projection()}),
            ),
            Self::Immediate(prepared) => Ok(prepared.projection()),
            Self::HealthyPersonal(original) => Ok(json!({"personal":original})),
            Self::UnavailablePersonalHomeOAuth { original, home } => Ok(
                json!({"original_unavailable_personal":original,"native_home_digest":home.binding_digest().map_err(home_error)?}),
            ),
        }
    }
    fn storage_weight(&self) -> Result<usize, Refusal> {
        match self {
            Self::PreparedHome { original, prepared } => {
                add(original.storage_weight()?, prepared.weight())
            }
            Self::Immediate(prepared) => Ok(prepared.weight()),
            Self::HealthyPersonal(original) => original.storage_weight(),
            Self::UnavailablePersonalHomeOAuth { original, home } => add(
                original.storage_weight()?,
                home.storage_weight().map_err(home_error)?,
            ),
        }
    }
    fn approval(&self, digest: String) -> Result<GoogleAccountApproval, Refusal> {
        match self {
            Self::PreparedHome { original, prepared } => prepared.approval(original, digest),
            Self::Immediate(_) => Err(refused()),
            Self::HealthyPersonal(original) => original.dedicated_approval(digest),
            Self::UnavailablePersonalHomeOAuth { original, home } => {
                let (selector, unavailable) = original.fallback_description()?;
                let (id, epoch, generation) = home.logical_connector();
                Ok(GoogleAccountApproval {
                    selected_identity: gi::ApprovedGoogleIdentity::HomeOAuthFallback,
                    selected_google_account: String::new(),
                    selected_credential_owner: String::new(),
                    selected_person_login: String::new(),
                    requester_kind: "unknown".into(),
                    configured_home_fallback: true,
                    home_fallback_authorized: true,
                    account_binding_digest: digest,
                    home_source: None,
                    home_connection: Some(gi::GoogleHomeConnectionApproval {
                        credential_id: id.into(),
                        logical_epoch: epoch,
                        source_generation: generation.into(),
                        original_selector: selector,
                        original_unavailable: unavailable,
                    }),
                })
            }
        }
    }
}

struct DedicatedBinding {
    agent: String,
    reference: CredentialRef,
    tool: String,
    original_digest: String,
    final_digest: Option<String>,
    created_unix: u64,
    expires_at: u64,
    created_mono: Instant,
}
impl DedicatedBinding {
    fn live(&self, clock: Clock) -> bool {
        self.created_unix <= clock.unix
            && clock.unix < self.expires_at
            && self.expires_at.checked_sub(self.created_unix) == Some(GOOGLE_SELECTION_TTL_SECS)
            && clock
                .mono
                .checked_duration_since(self.created_mono)
                .is_some_and(|age| age < Duration::from_secs(GOOGLE_SELECTION_TTL_SECS))
    }
    fn matches(&self, agent: &str, reference: &CredentialRef, tool: &str, clock: Clock) -> bool {
        self.agent == agent && self.reference == *reference && self.tool == tool && self.live(clock)
    }
    fn context(
        &self,
        home: &Path,
        users: Option<&Arc<UserDb>>,
    ) -> Result<GoogleCallContext, Refusal> {
        GoogleCallContext::dedicated(
            home,
            &self.agent,
            people_for(home, users),
            &self.tool,
            self.final_digest
                .clone()
                .unwrap_or_else(|| self.original_digest.clone()),
            self.reference.clone(),
        )
    }
    fn projection(&self) -> Value {
        json!({"agent":self.agent,"credential_ref":self.reference,"tool":self.tool,"original_digest":self.original_digest,"created_at":self.created_unix,"expires_at":self.expires_at})
    }
    fn storage_weight(&self) -> Result<usize, Refusal> {
        let mut n = std::mem::size_of_val(self);
        for s in [
            &self.agent,
            &self.reference.sha256,
            &self.tool,
            &self.original_digest,
        ] {
            n = add(n, s.capacity())?;
        }
        if let Some(s) = &self.final_digest {
            n = add(n, s.capacity())?;
        }
        Ok(n)
    }
}
struct Entry {
    original: OriginalAccountUnion,
    bound: DedicatedBinding,
    stamp: GoogleSelectionStamp,
    bytes: usize,
}
fn stamp_weight(s: &GoogleSelectionStamp) -> Result<usize, Refusal> {
    let a = &s.approval;
    let mut n = std::mem::size_of_val(s);
    for v in [
        &s.opaque_id,
        &s.original_arguments_digest,
        &a.selected_google_account,
        &a.selected_credential_owner,
        &a.selected_person_login,
        &a.requester_kind,
        &a.account_binding_digest,
    ] {
        n = add(n, v.capacity())?;
    }
    if let Some(v) = &s.final_arguments_digest {
        n = add(n, v.capacity())?;
    }
    if let Some(h) = &a.home_connection {
        for v in [
            &h.credential_id,
            &h.source_generation,
            &h.original_selector,
            &h.original_unavailable,
        ] {
            n = add(n, v.capacity())?;
        }
    }
    if let Some(source) = &a.home_source {
        n = add(n, source.retained_capacity())?;
    }
    Ok(n)
}
#[derive(Default)]
struct Entries {
    values: HashMap<String, Entry>,
    bytes: usize,
    preparing: HashMap<String, usize>,
    reserved: usize,
}
struct Limits {
    total: usize,
    per_agent: usize,
    bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            total: MAX_SELECTIONS,
            per_agent: MAX_SELECTIONS_PER_AGENT,
            bytes: MAX_TOTAL_BYTES,
        }
    }
}
pub(in crate::google_identity) struct SelectionStore {
    inner: Mutex<Entries>,
    limits: Limits,
}
impl Default for SelectionStore {
    fn default() -> Self {
        Self {
            inner: Mutex::default(),
            limits: Limits::default(),
        }
    }
}
impl SelectionStore {
    fn prune(entries: &mut Entries, clock: Clock) {
        entries
            .values
            .retain(|_, e| e.bound.live(clock) && e.original.live());
        entries.bytes = entries.values.values().map(|e| e.bytes).sum();
    }
    #[cfg(test)]
    pub(in crate::google_identity) fn len_for_test(&self) -> usize {
        self.inner.lock().unwrap().values.len()
    }
    #[cfg(test)]
    pub(in crate::google_identity) fn set_limits_for_test(
        &mut self,
        total: usize,
        per_agent: usize,
        bytes: usize,
    ) {
        self.limits = Limits {
            total,
            per_agent,
            bytes,
        };
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(in crate::google_identity) fn select_dedicated(
        &self,
        home: &Path,
        users: Option<&Arc<UserDb>>,
        agent: &str,
        reference: &CredentialRef,
        tool: &str,
        args: &Value,
        clock: Clock,
    ) -> Result<Option<GoogleSelectionStamp>, Refusal> {
        live_key(home, agent, reference, tool)?;
        match gi::effective_for(home, Some(agent)).effective {
            gi::Effective::Home | gi::Effective::Delegate { .. } => return Ok(None),
            gi::Effective::Personal { .. } => {}
            other => {
                return Err(
                    crate::google_identity::resolve::refusal_for(&other).unwrap_or_else(refused)
                );
            }
        }
        let people = people_for(home, users);
        let selected = PersonalSelection::prepare_dedicated(
            home,
            agent,
            &RequesterIdentity::Unknown,
            people.as_ref(),
            tool,
            || live_key(home, agent, reference, tool),
        )?;
        let original = if selected.ensure_healthy_personal().is_ok() {
            OriginalAccountUnion::HealthyPersonal(selected)
        } else {
            selected.validate_original_unavailable(
                home,
                agent,
                &RequesterIdentity::Unknown,
                people.as_ref(),
                tool,
            )?;
            let home_selection =
                home_source::prepare_home_native_selection(home, tool).map_err(home_error)?;
            selected.validate_original_unavailable(
                home,
                agent,
                &RequesterIdentity::Unknown,
                people.as_ref(),
                tool,
            )?;
            OriginalAccountUnion::UnavailablePersonalHomeOAuth {
                original: selected,
                home: home_selection,
            }
        };
        self.admit_original(
            home, users, agent, reference, tool, args, clock, original, None, None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(in crate::google_identity) async fn select_dedicated_async(
        &self,
        home: &Path,
        users: Option<&Arc<UserDb>>,
        agent: &str,
        reference: &CredentialRef,
        tool: &str,
        args: &Value,
        clock: Clock,
    ) -> Result<Option<GoogleSelectionStamp>, Refusal> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(18);
        live_key(home, agent, reference, tool)?;
        match gi::effective_for(home, Some(agent)).effective {
            gi::Effective::Home | gi::Effective::Delegate { .. } => return Ok(None),
            gi::Effective::Personal { .. } => {}
            other => {
                return Err(
                    crate::google_identity::resolve::refusal_for(&other).unwrap_or_else(refused)
                );
            }
        }
        if tool != "google_status" {
            crate::google_workspace::native_call::NativeCallTemplate::prepare(tool, args)
                .and_then(|t| t.validate_closed())
                .map_err(|_| {
                    Refusal::hard(
                        "native_arguments_invalid",
                        "Google 工具參數不正確，這次未取得憑證。",
                    )
                })?;
        }
        let mut reservation = self.reserve(agent, clock)?;
        let people = people_for(home, users);
        let selected = PersonalSelection::prepare_dedicated(
            home,
            agent,
            &RequesterIdentity::Unknown,
            people.as_ref(),
            tool,
            || live_key(home, agent, reference, tool),
        )?;
        let original = if selected.ensure_healthy_personal().is_ok() {
            OriginalAccountUnion::HealthyPersonal(selected)
        } else {
            selected.validate_original_unavailable(
                home,
                agent,
                &RequesterIdentity::Unknown,
                people.as_ref(),
                tool,
            )?;
            let context = GoogleCallContext::dedicated(
                home,
                agent,
                people_for(home, users),
                tool,
                gi::google_arguments_digest(args),
                reference.clone(),
            )?;
            let prepared = if tool == "google_status" {
                super::variants::Prepared::metadata(&context, true)?
            } else {
                tokio::time::timeout_at(
                    deadline,
                    super::variants::Prepared::home(
                        &context,
                        deadline,
                        true,
                        PreflightOriginal::Personal(&selected),
                    ),
                )
                .await
                .map_err(|_| preflight_expired())??
            };
            selected.validate_original_unavailable(
                home,
                agent,
                &RequesterIdentity::Unknown,
                people.as_ref(),
                tool,
            )?;
            OriginalAccountUnion::PreparedHome {
                original: selected,
                prepared,
            }
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(preflight_expired());
        }
        // The final source checks and bounded admission stay in this future.
        self.admit_original(
            home,
            users,
            agent,
            reference,
            tool,
            args,
            clock,
            original,
            Some(deadline),
            Some(&mut reservation),
        )
    }
    fn reserve<'a>(
        &'a self,
        agent: &str,
        clock: Clock,
    ) -> Result<PreparationReservation<'a>, Refusal> {
        let mut inner = self.inner.lock().map_err(|_| refused())?;
        Self::prune(&mut inner, clock);
        let total = inner.preparing.values().copied().sum::<usize>();
        let per = inner.preparing.get(agent).copied().unwrap_or(0)
            + inner
                .values
                .values()
                .filter(|e| e.bound.agent == agent)
                .count();
        if inner.values.len() + total >= self.limits.total
            || per >= self.limits.per_agent
            || add(add(inner.bytes, inner.reserved)?, MAX_SELECTION_BYTES)? > self.limits.bytes
        {
            return Err(capacity());
        }
        *inner.preparing.entry(agent.into()).or_default() += 1;
        inner.reserved += MAX_SELECTION_BYTES;
        Ok(PreparationReservation {
            store: self,
            agent: agent.into(),
            active: true,
        })
    }
    #[allow(clippy::too_many_arguments)]
    fn admit_original(
        &self,
        home: &Path,
        users: Option<&Arc<UserDb>>,
        agent: &str,
        reference: &CredentialRef,
        tool: &str,
        args: &Value,
        clock: Clock,
        original: OriginalAccountUnion,
        deadline: Option<tokio::time::Instant>,
        mut reservation: Option<&mut PreparationReservation<'_>>,
    ) -> Result<Option<GoogleSelectionStamp>, Refusal> {
        live_key(home, agent, reference, tool)?;
        let id = uuid::Uuid::new_v4().to_string();
        let bound = DedicatedBinding {
            agent: agent.into(),
            reference: reference.clone(),
            tool: tool.into(),
            original_digest: gi::google_arguments_digest(args),
            final_digest: None,
            created_unix: clock.unix,
            expires_at: clock
                .unix
                .checked_add(GOOGLE_SELECTION_TTL_SECS)
                .ok_or_else(refused)?,
            created_mono: clock.mono,
        };
        original.validate(&bound.context(home, users)?)?;
        if add(original.storage_weight()?, bound.storage_weight()?)? > MAX_SELECTION_BYTES {
            return Err(capacity());
        }
        let digest=format!("{:x}",Sha256::digest(serde_json::to_vec(&json!({"domain":"google-original-union-v1","id":id,"binding":bound.projection(),"original":original.binding_projection()?})).map_err(|_|refused())?));
        let approval = original.approval(digest)?;
        let stamp = GoogleSelectionStamp {
            opaque_id: id.clone(),
            expires_at: bound.expires_at,
            original_arguments_digest: bound.original_digest.clone(),
            final_arguments_digest: None,
            approval,
        };
        if !stamp.is_well_formed() {
            return Err(refused());
        }
        // Conservative inline overcount is intentional. Actual retained heap
        // capacities of both sealed owners and every DTO/String are included.
        let weight = add(
            add(
                add(
                    add(std::mem::size_of::<Entry>(), id.capacity())?,
                    original.storage_weight()?,
                )?,
                bound.storage_weight()?,
            )?,
            stamp_weight(&stamp)?,
        )?;
        if weight > MAX_SELECTION_BYTES {
            return Err(capacity());
        }
        let mut inner = self.inner.lock().map_err(|_| refused())?;
        Self::prune(&mut inner, clock);
        let count = inner
            .values
            .values()
            .filter(|e| e.bound.agent == agent)
            .count();
        let own = usize::from(reservation.is_some());
        let pending = inner.preparing.values().copied().sum::<usize>();
        let pending_agent = inner.preparing.get(agent).copied().unwrap_or(0);
        let own_bytes = if own == 1 { MAX_SELECTION_BYTES } else { 0 };
        if inner.values.len() + pending - own >= self.limits.total
            || count + pending_agent - own >= self.limits.per_agent
            || add(
                add(inner.bytes, inner.reserved.saturating_sub(own_bytes))?,
                weight,
            )? > self.limits.bytes
            || inner.values.contains_key(&id)
        {
            return Err(capacity());
        }
        // No I/O or await after this final deadline check. A lost response
        // after this commit leaves only a bounded, expiring one-shot entry.
        if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
            return Err(preflight_expired());
        }
        if !original.live() {
            return Err(refused());
        }
        if let Some(reservation) = reservation.as_mut() {
            reservation.release_locked(&mut inner);
        }
        inner.bytes += weight;
        inner.values.insert(
            id,
            Entry {
                original,
                bound,
                stamp: stamp.clone(),
                bytes: weight,
            },
        );
        Ok(Some(stamp))
    }
    #[allow(clippy::too_many_arguments)]
    pub(in crate::google_identity) fn bind_dedicated(
        &self,
        home: &Path,
        users: Option<&Arc<UserDb>>,
        agent: &str,
        reference: &CredentialRef,
        tool: &str,
        id: &str,
        original_digest: &str,
        approval_digest: &str,
        args: &Value,
        clock: Clock,
    ) -> Result<GoogleSelectionStamp, Refusal> {
        live_key(home, agent, reference, tool)?;
        if !gi::google_digest_well_formed(original_digest)
            || !gi::google_digest_well_formed(approval_digest)
        {
            return Err(refused());
        }
        let digest = gi::google_arguments_digest(args);
        let mut inner = self.inner.lock().map_err(|_| refused())?;
        Self::prune(&mut inner, clock);
        let Entries { values, bytes, .. } = &mut *inner;
        let entry = values.get_mut(id).ok_or_else(refused)?;
        if !entry.bound.matches(agent, reference, tool, clock)
            || entry.bound.original_digest != original_digest
            || entry.stamp.approval.account_binding_digest != approval_digest
        {
            return Err(refused());
        }
        entry
            .original
            .validate(&entry.bound.context(home, users)?)?;
        match &entry.bound.final_digest {
            Some(value) if *value != digest => return Err(refused()),
            Some(_) => {}
            None => {
                let a = digest.clone();
                let b = digest.clone();
                let extra = add(a.capacity(), b.capacity())?;
                if add(entry.bytes, extra)? > MAX_SELECTION_BYTES
                    || add(*bytes, extra)? > self.limits.bytes
                {
                    return Err(capacity());
                }
                entry.bound.final_digest = Some(a);
                entry.stamp.final_arguments_digest = Some(b);
                entry.bytes += extra;
                *bytes += extra;
            }
        }
        Ok(entry.stamp.clone())
    }
    #[allow(clippy::too_many_arguments)]
    pub(in crate::google_identity) fn claim_dedicated(
        &self,
        home: &Path,
        users: Option<&Arc<UserDb>>,
        agent: &str,
        reference: &CredentialRef,
        tool: &str,
        id: &str,
        args: &Value,
        clock: Clock,
    ) -> Result<PendingGoogleCall, Refusal> {
        let digest = gi::google_arguments_digest(args);
        let mut inner = self.inner.lock().map_err(|_| refused())?;
        Self::prune(&mut inner, clock);
        let entry = inner.values.get(id).ok_or_else(refused)?;
        if !entry.bound.matches(agent, reference, tool, clock)
            || entry.bound.final_digest.as_deref() != Some(digest.as_str())
        {
            return Err(refused());
        }
        let entry = inner.values.remove(id).ok_or_else(refused)?;
        inner.bytes = inner.bytes.saturating_sub(entry.bytes);
        drop(inner);
        // The map is already consumed and unlocked before canonical paths,
        // credential registry, People/ACL reads, future construction or await.
        let context = entry.bound.context(home, users)?;
        let claim = ClaimedSelection {
            original: entry.original,
            bound: entry.bound,
        };
        context.validate()?;
        claim.validate(&context, clock)?;
        Ok(PendingGoogleCall { context, claim })
    }
}
pub(super) struct ClaimedSelection {
    original: OriginalAccountUnion,
    bound: DedicatedBinding,
}
impl ClaimedSelection {
    pub(super) fn validate(
        &self,
        context: &GoogleCallContext,
        clock: Clock,
    ) -> Result<(), Refusal> {
        let GooglePurpose::Dedicated {
            native_tool,
            final_arguments_digest,
            credential_ref,
        } = &context.purpose
        else {
            return Err(refused());
        };
        if !self
            .bound
            .matches(&context.agent, credential_ref, native_tool, clock)
            || !self.original.live()
            || self.bound.final_digest.as_deref() != Some(final_arguments_digest.as_str())
        {
            return Err(refused());
        }
        self.original.validate(context)
    }
    pub(super) fn validate_material(
        &self,
        context: &GoogleCallContext,
        material: &Material,
    ) -> Result<(), Refusal> {
        self.validate(context, Clock::now())?;
        match (&self.original, material) {
            (
                OriginalAccountUnion::PreparedHome { prepared, .. }
                | OriginalAccountUnion::Immediate(prepared),
                Material::Variant(material),
            ) => prepared.validate_material(context, material),
            (OriginalAccountUnion::HealthyPersonal(original), Material::Personal(token))
                if token.matches_selection(original) =>
            {
                token.validate(
                    &context.home,
                    &context.agent,
                    &context.requester,
                    context.people.as_ref(),
                    context.purpose.scope_key(),
                )
            }
            (
                OriginalAccountUnion::UnavailablePersonalHomeOAuth { home, .. },
                Material::HomeOAuth(token),
            ) => home_source::validate_home_native_material(
                &context.home,
                home,
                context.purpose.scope_key(),
                token.proof(),
            )
            .map_err(home_error),
            _ => Err(refused()),
        }
    }
    #[cfg(test)]
    pub(super) fn check_legacy_resolved(
        &self,
        resolved: &crate::google_identity::resolve::Resolved,
    ) -> Result<(), Refusal> {
        match (&self.original, resolved) {
            (
                OriginalAccountUnion::HealthyPersonal(original),
                crate::google_identity::resolve::Resolved::Personal {
                    user_id,
                    email,
                    authority,
                    ..
                },
            ) if Some(user_id.as_str()) == original.user_id()
                && email == original.account_email()
                && authority.selection() == original =>
            {
                Ok(())
            }
            _ => Err(Refusal::hard(
                "approved_account_mismatch",
                "本次核准的原Google帳號不相符，請重新選擇並核准。",
            )),
        }
    }
}
pub(super) async fn resolve_claimed(
    pending: PendingGoogleCall,
) -> Result<ResolvedGoogleMaterial, Refusal> {
    let PendingGoogleCall { context, mut claim } = pending;
    context.validate()?;
    claim.validate(&context, Clock::now())?;
    if context.purpose.scope_key() == "google_status" {
        return Err(refused());
    }
    let selected = match &mut claim.original {
        OriginalAccountUnion::PreparedHome { prepared, .. }
        | OriginalAccountUnion::Immediate(prepared) => Material::Variant(prepared.take()?),
        OriginalAccountUnion::HealthyPersonal(original) => Material::Personal(
            access::resolve_pinned(
                &context.home,
                &context.agent,
                &context.requester,
                context.people.as_ref(),
                original,
                context.purpose.scope_key(),
            )
            .await
            .map_err(|mut e| {
                e.fallback_eligible = false;
                e
            })?,
        ),
        OriginalAccountUnion::UnavailablePersonalHomeOAuth { home, .. } => Material::HomeOAuth(
            home_source::resolve_home_native_pinned(
                &context.home,
                home,
                context.purpose.scope_key(),
            )
            .await
            .map_err(home_error)?,
        ),
    };
    context.validate()?;
    claim.validate_material(&context, &selected)?;
    Ok(ResolvedGoogleMaterial {
        context,
        claim: MaterialClaim::Dedicated(claim),
        selected,
    })
}
pub(super) fn metadata_status(pending: PendingGoogleCall) -> Result<Value, Refusal> {
    pending.context.validate()?;
    pending.claim.validate(&pending.context, Clock::now())?;
    if pending.context.purpose.scope_key() != "google_status" {
        return Err(refused());
    }
    let text = match &pending.claim.original {
        OriginalAccountUnion::PreparedHome { .. } | OriginalAccountUnion::Immediate(_) => {
            "目前選用已核准的公司 Google 來源；Google 主體未驗證。本次只查本機紀錄，未取得權杖或連線測試 Google。"
                .into()
        }
        OriginalAccountUnion::HealthyPersonal(original) => format!(
            "目前選用個人 Google 帳號：{}。本次只查本機授權紀錄，未連線測試 Google。",
            original.account_email()
        ),
        OriginalAccountUnion::UnavailablePersonalHomeOAuth { home, .. } => {
            if !home
                .is_metadata_status("google_status")
                .map_err(home_error)?
            {
                return Err(refused());
            }
            "目前選用公司共用 Google OAuth 連線；Google 主體未驗證。本次只查本機授權紀錄，未取得權杖或連線測試 Google。".into()
        }
    };
    pending.context.validate()?;
    pending.claim.validate(&pending.context, Clock::now())?;
    if matches!(
        &pending.claim.original,
        OriginalAccountUnion::PreparedHome { .. } | OriginalAccountUnion::Immediate(_)
    ) {
        crate::google_identity::execute::audit(
            &pending.context.home,
            &pending.context.agent,
            "google_status",
            pending.claim.audit_account(),
            true,
            pending.claim.is_home_fallback(),
            "",
        );
    }
    Ok(json!({"content":[{"type":"text","text":text}]}))
}

impl OriginalAccountUnion {
    fn live(&self) -> bool {
        match self {
            Self::PreparedHome { prepared, .. } | Self::Immediate(prepared) => prepared.live(),
            _ => true,
        }
    }
}
struct PreparationReservation<'a> {
    store: &'a SelectionStore,
    agent: String,
    active: bool,
}
impl PreparationReservation<'_> {
    fn release_locked(&mut self, entries: &mut Entries) {
        if self.active {
            if let Some(n) = entries.preparing.get_mut(&self.agent) {
                *n -= 1;
                if *n == 0 {
                    entries.preparing.remove(&self.agent);
                }
            }
            entries.reserved = entries.reserved.saturating_sub(MAX_SELECTION_BYTES);
            self.active = false;
        }
    }
}
impl Drop for PreparationReservation<'_> {
    fn drop(&mut self) {
        if self.active {
            if let Ok(mut inner) = self.store.inner.lock() {
                self.release_locked(&mut inner);
            }
        }
    }
}

/// Actual no-Ask dedicated Home/Delegate host. Never constructs a DTO claim.
pub(super) async fn prepare_immediate(
    context: GoogleCallContext,
) -> Result<PendingGoogleCall, Refusal> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(18);
    let prepared = if context.purpose.scope_key() == "google_status" {
        super::variants::Prepared::metadata(&context, false)?
    } else {
        tokio::time::timeout_at(
            deadline,
            super::variants::Prepared::immediate(&context, deadline),
        )
        .await
        .map_err(|_| refused())??
    };
    context.validate()?;
    prepared.validate(&context)?;
    if tokio::time::Instant::now() >= deadline {
        return Err(preflight_expired());
    }
    let GooglePurpose::Dedicated {
        native_tool,
        credential_ref,
        final_arguments_digest,
    } = &context.purpose
    else {
        return Err(refused());
    };
    let clock = Clock::now();
    let claim = ClaimedSelection {
        original: OriginalAccountUnion::Immediate(prepared),
        bound: DedicatedBinding {
            agent: context.agent.clone(),
            reference: credential_ref.clone(),
            tool: native_tool.clone(),
            original_digest: final_arguments_digest.clone(),
            final_digest: Some(final_arguments_digest.clone()),
            created_unix: clock.unix,
            expires_at: clock.unix + GOOGLE_SELECTION_TTL_SECS,
            created_mono: clock.mono,
        },
    };
    claim.validate(&context, clock)?;
    // Synchronous source/control/People validation may return after the budget.
    // Admission uses the same preflight deadline, before any pending capability exits.
    let final_now = tokio::time::Instant::now();
    #[cfg(test)]
    let final_now = immediate_tail_clock::observe(&context.home, final_now, deadline);
    if final_now >= deadline {
        return Err(preflight_expired());
    }
    Ok(PendingGoogleCall { context, claim })
}

impl ClaimedSelection {
    pub(super) fn is_home_fallback(&self) -> bool {
        match &self.original {
            OriginalAccountUnion::UnavailablePersonalHomeOAuth { .. }
            | OriginalAccountUnion::PreparedHome { .. } => true,
            OriginalAccountUnion::Immediate(prepared) => prepared.fallback(),
            _ => false,
        }
    }
}

#[cfg(test)]
impl SelectionStore {
    pub(in crate::google_identity) fn expire_prepared_for_test(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap();
        match &mut inner.values.get_mut(id).unwrap().original {
            OriginalAccountUnion::PreparedHome { prepared, .. }
            | OriginalAccountUnion::Immediate(prepared) => prepared.expire_for_test(),
            _ => panic!("expected W3 prepared entry"),
        }
    }
    pub(in crate::google_identity) fn reservations_for_test(&self) -> usize {
        self.inner.lock().unwrap().preparing.values().copied().sum()
    }
}

impl ClaimedSelection {
    pub(super) fn audit_account(&self) -> &str {
        match &self.original {
            OriginalAccountUnion::Immediate(prepared) => prepared.audit_account(),
            _ => "",
        }
    }
}

fn preflight_expired() -> Refusal {
    Refusal::hard("selection_expired", "Google 帳號準備逾時，請重新選擇。")
}

// Only this final admission clock is injectable. It runs after the real claim
// validation, never replaces any validator and is absent from production.
#[cfg(test)]
pub(in crate::google_identity) mod immediate_tail_clock {
    use super::*;
    use std::sync::OnceLock;
    fn entries() -> &'static Mutex<HashMap<PathBuf, (bool, usize)>> {
        static ENTRIES: OnceLock<Mutex<HashMap<PathBuf, (bool, usize)>>> = OnceLock::new();
        ENTRIES.get_or_init(Default::default)
    }
    pub(in crate::google_identity) struct Guard(PathBuf);
    impl Guard {
        pub(in crate::google_identity) fn observations(&self) -> usize {
            entries().lock().unwrap().get(&self.0).unwrap().1
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            entries().lock().unwrap().remove(&self.0);
        }
    }
    pub(in crate::google_identity) fn install(home: &Path, expired: bool) -> Guard {
        let home = home.canonicalize().unwrap();
        assert!(
            entries()
                .lock()
                .unwrap()
                .insert(home.clone(), (expired, 0))
                .is_none()
        );
        Guard(home)
    }
    pub(super) fn observe(
        home: &Path,
        actual: tokio::time::Instant,
        deadline: tokio::time::Instant,
    ) -> tokio::time::Instant {
        let mut entries = entries().lock().unwrap();
        if let Some((expired, count)) = entries.get_mut(home) {
            *count += 1;
            if *expired {
                return deadline;
            }
        }
        actual
    }
}

#[cfg(test)]
impl SelectionStore {
    pub(in crate::google_identity) fn expire_w3_oauth_mono_for_test(&self, id: &str) {
        let mut entries = self.inner.lock().unwrap();
        match &mut entries.values.get_mut(id).unwrap().original {
            OriginalAccountUnion::PreparedHome { prepared, .. }
            | OriginalAccountUnion::Immediate(prepared) => prepared.expire_oauth_mono_for_test(),
            _ => panic!("expected W3 OAuth prepared entry"),
        }
    }
}
#[cfg(test)]
impl ClaimedSelection {
    pub(in crate::google_identity) fn expire_w3_oauth_mono_for_test(&mut self) {
        match &mut self.original {
            OriginalAccountUnion::PreparedHome { prepared, .. }
            | OriginalAccountUnion::Immediate(prepared) => prepared.expire_oauth_mono_for_test(),
            _ => panic!("expected W3 OAuth claim"),
        }
    }
}
