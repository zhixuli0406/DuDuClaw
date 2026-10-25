//! Durable nonsecret publication barrier. No payload, bearer, resolver or
//! provider call is stored or replayed here. Caller values remain quarantined
//! until both data durability and gate cleanup are confirmed.
use super::*;
#[cfg(not(windows))]
use std::fs::File;
use std::fs::OpenOptions;
pub(super) const GATE_FILE: &str = ".google-home-publication-intent.json";
const MAX_GATE: u64 = 16 * 1024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RowProof {
    digest: RawDigest,
    generation: Option<String>,
    credential: Option<Credential>,
    mutation: Option<Mutation>,
    ticket: Option<TicketRecord>,
    payload_digest: Option<RawDigest>,
}
impl RowProof {
    fn of(raw: Option<&Value>) -> Result<Self> {
        let control: Option<Control> = raw
            .and_then(|r| r.get("google_home"))
            .map(|v| serde_json::from_value(v.clone()).map_err(|_| Error::Invalid))
            .transpose()?;
        let payload_digest = raw
            .filter(|r| r.get("access_token").is_some())
            .map(|r| {
                let mut payload = serde_json::Map::new();
                for name in TOKEN_FIELDS {
                    payload.insert(name.into(), r.get(name).cloned().unwrap_or(Value::Null));
                }
                digest(&Value::Object(payload))
            })
            .transpose()?;
        Ok(Self {
            digest: digest(raw.unwrap_or(&Value::Null))?,
            generation: control.as_ref().map(|c| c.source_generation.clone()),
            credential: control.as_ref().and_then(|c| c.credential.clone()),
            mutation: control.as_ref().and_then(|c| c.pending_mutation.clone()),
            ticket: control.and_then(|c| c.active_connect),
            payload_digest,
        })
    }
    fn validate(&self) -> Result<()> {
        if self.generation.as_ref().is_some_and(|g| !valid_id(g))
            || self.credential.as_ref().is_some_and(|c| {
                !valid_id(&c.credential_id) || c.logical_epoch == 0 || c.token_revision == 0
            })
            || self
                .mutation
                .as_ref()
                .is_some_and(|m| !valid_id(&m.mutation_id))
            || self.ticket.as_ref().is_some_and(|t| {
                !valid_id(&t.ticket_id) || !valid_id(&t.generation) || t.expires_at == 0
            })
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
/// An explicit ticket proof is mandatory on each connect operation, including
/// Start (whose new ticket previously existed only in the producer's T).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConnectProof {
    ticket: TicketRecord,
    mutation_binding: MutationBinding,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum MutationBinding {
    Pending { mutation_id: String },
    TicketOnly { ticket_id: String },
}
impl ConnectProof {
    pub(super) fn pending(ticket: TicketRecord, mutation_id: String) -> Self {
        Self {
            ticket,
            mutation_binding: MutationBinding::Pending { mutation_id },
        }
    }
    pub(super) fn ticket(ticket: TicketRecord) -> Self {
        Self {
            mutation_binding: MutationBinding::TicketOnly {
                ticket_id: ticket.ticket_id.clone(),
            },
            ticket,
        }
    }
    fn valid(&self) -> bool {
        valid_id(&self.ticket.ticket_id)
            && valid_id(&self.ticket.generation)
            && self.ticket.expires_at > 0
            && match &self.mutation_binding {
                MutationBinding::Pending { mutation_id } => valid_id(mutation_id),
                MutationBinding::TicketOnly { ticket_id } => *ticket_id == self.ticket.ticket_id,
            }
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum PublicationOp {
    MetadataMigration,
    MetadataMigrationTickets { proofs: Vec<ConnectProof> },
    ConnectStart { proof: ConnectProof },
    ConnectPublishStart { proof: ConnectProof },
    TicketClaim { proof: ConnectProof },
    BeginClaimedFinish { proof: ConnectProof },
    ClaimedPayload { proof: ConnectProof },
    ConnectFinish { proof: ConnectProof },
    Refresh,
    RefreshTickets { proofs: Vec<ConnectProof> },
    ConfigFence,
    ConfigFenceTickets { proofs: Vec<ConnectProof> },
    ConfigFinish,
    Revoke,
    RevokeTickets { proofs: Vec<ConnectProof> },
}
impl PublicationOp {
    fn actual_tickets(before: &RowProof, after: &RowProof) -> Result<Vec<ConnectProof>> {
        let mut proofs = Vec::new();
        for r in [before, after] {
            if r.mutation
                .as_ref()
                .is_some_and(|m| m.kind != MutationKind::Config)
                && r.ticket.is_none()
            {
                return Err(Error::Invalid);
            }
            if let Some(t) = &r.ticket {
                let proof = match &r.mutation {
                    Some(m) => ConnectProof::pending(t.clone(), m.mutation_id.clone()),
                    None => ConnectProof::ticket(t.clone()),
                };
                if !proof.valid() {
                    return Err(Error::Invalid);
                }
                if !proofs.contains(&proof) {
                    proofs.push(proof);
                }
            }
        }
        Ok(proofs)
    }
    /// Producer-only constructor for an operation that may retire an old
    /// ticket. Ordinary nonconnect variants explicitly reject ticket rows.
    pub(super) fn with_existing_tickets(
        self,
        before: Option<&Value>,
        after: Option<&Value>,
    ) -> Result<Self> {
        let proofs = Self::actual_tickets(&RowProof::of(before)?, &RowProof::of(after)?)?;
        if proofs.is_empty() {
            return Ok(self);
        }
        match self {
            Self::MetadataMigration => Ok(Self::MetadataMigrationTickets { proofs }),
            Self::Refresh => Ok(Self::RefreshTickets { proofs }),
            Self::ConfigFence => Ok(Self::ConfigFenceTickets { proofs }),
            Self::Revoke => Ok(Self::RevokeTickets { proofs }),
            _ => Err(Error::Invalid),
        }
    }
    fn check_existing_tickets(&self, before: &RowProof, after: &RowProof) -> Result<()> {
        let actual = Self::actual_tickets(before, after)?;
        if self.connect().is_some() {
            return Ok(());
        }
        match self {
            Self::MetadataMigrationTickets { proofs }
            | Self::RefreshTickets { proofs }
            | Self::ConfigFenceTickets { proofs }
            | Self::RevokeTickets { proofs } => {
                if proofs.is_empty() || *proofs != actual {
                    return Err(Error::Invalid);
                }
            }
            _ if !actual.is_empty() => return Err(Error::Invalid),
            _ => {}
        }
        Ok(())
    }
    fn connect(&self) -> Option<&ConnectProof> {
        match self {
            Self::ConnectStart { proof }
            | Self::ConnectPublishStart { proof }
            | Self::TicketClaim { proof }
            | Self::BeginClaimedFinish { proof }
            | Self::ClaimedPayload { proof }
            | Self::ConnectFinish { proof } => Some(proof),
            _ => None,
        }
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "producer", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ProducerProof {
    Start {
        generation: String,
        mutation_id: String,
        ticket: TicketRecord,
    },
    Ticket {
        ticket: TicketRecord,
    },
    Claim {
        ticket: TicketRecord,
    },
    Change {
        generation: String,
        mutation_id: String,
        claimed_ticket: Option<String>,
    },
    Metadata {
        generation: String,
        credential: Option<Credential>,
    },
    Unit,
}
pub(super) trait PublicationValue {
    fn publication_proof(&self) -> ProducerProof;
}
impl PublicationValue for StartConnect {
    fn publication_proof(&self) -> ProducerProof {
        ProducerProof::Start {
            generation: self.generation.clone(),
            mutation_id: self.mutation_id.clone(),
            ticket: self.ticket.clone(),
        }
    }
}
impl PublicationValue for GoogleConnectTicket {
    fn publication_proof(&self) -> ProducerProof {
        ProducerProof::Ticket {
            ticket: self.record.clone(),
        }
    }
}
impl PublicationValue for ClaimedGoogleConnect {
    fn publication_proof(&self) -> ProducerProof {
        ProducerProof::Claim {
            ticket: self.record.clone(),
        }
    }
}
impl PublicationValue for ChangeTicket {
    fn publication_proof(&self) -> ProducerProof {
        ProducerProof::Change {
            generation: self.generation.clone(),
            mutation_id: self.mutation_id.clone(),
            claimed_ticket: self.claimed_ticket.clone(),
        }
    }
}
impl PublicationValue for Metadata {
    fn publication_proof(&self) -> ProducerProof {
        ProducerProof::Metadata {
            generation: self.source_generation.clone(),
            credential: self.credential.clone(),
        }
    }
}
impl PublicationValue for () {
    fn publication_proof(&self) -> ProducerProof {
        ProducerProof::Unit
    }
}

/// Not serializable, no public constructor. Every mutation returns this with
/// the plan and its own T created together. A transaction cannot accept an
/// operation label/None plus an unrelated capability.
pub(crate) struct Planned<T> {
    value: T,
    plan: TypedPublicationPlan,
    proof_of: fn(&T) -> ProducerProof,
    staged_source: Option<PreparedSourceWrite>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TypedPublicationPlan {
    op: PublicationOp,
    before: RowProof,
    after: RowProof,
    source: Option<SourceBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_after: Option<SourceBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_write: Option<SourceFileProof>,
    producer: ProducerProof,
}
impl<T: PublicationValue> Planned<T> {
    pub(super) fn new(
        value: T,
        before: Option<&Value>,
        after: Option<&Value>,
        source: Option<&ObservedSource>,
        op: PublicationOp,
    ) -> Result<Self> {
        Self::build(value, before, after, source, None, None, op)
    }
    pub(super) fn new_source_transition(
        value: T,
        before: Option<&Value>,
        after: Option<&Value>,
        source: &ObservedSource,
        desired: SourceBinding,
        write: Option<PreparedSourceWrite>,
        op: PublicationOp,
    ) -> Result<Self> {
        let source_after = if write.is_some() {
            Some(desired)
        } else {
            if desired != *source.binding() {
                return Err(Error::Invalid);
            }
            None
        };
        Self::build(value, before, after, Some(source), source_after, write, op)
    }
    fn build(
        value: T,
        before: Option<&Value>,
        after: Option<&Value>,
        source: Option<&ObservedSource>,
        source_after: Option<SourceBinding>,
        staged_source: Option<PreparedSourceWrite>,
        op: PublicationOp,
    ) -> Result<Self> {
        let plan = TypedPublicationPlan {
            op,
            before: RowProof::of(before)?,
            after: RowProof::of(after)?,
            source: source.map(|s| s.binding().clone()),
            source_after,
            source_write: staged_source.as_ref().map(|w| w.proof.clone()),
            producer: value.publication_proof(),
        };
        plan.validate()?;
        Ok(Self {
            value,
            plan,
            proof_of: T::publication_proof,
            staged_source,
        })
    }
    #[cfg(test)]
    pub(super) fn unit_for_test(self) -> Planned<()> {
        Planned {
            value: (),
            plan: self.plan,
            proof_of: <() as PublicationValue>::publication_proof,
            staged_source: self.staged_source,
        }
    }
    #[cfg(test)]
    pub(super) fn strip_ticket_variant_for_test(&mut self) {
        self.plan.op = match &self.plan.op {
            PublicationOp::RefreshTickets { .. } => PublicationOp::Refresh,
            PublicationOp::ConfigFenceTickets { .. } => PublicationOp::ConfigFence,
            PublicationOp::RevokeTickets { .. } => PublicationOp::Revoke,
            _ => PublicationOp::MetadataMigration,
        };
    }
    #[cfg(test)]
    pub(super) fn tamper_for_test(&mut self, field: &str) {
        if field == "operation" {
            self.plan.op = PublicationOp::MetadataMigration;
            return;
        }
        let proof = match &mut self.plan.op {
            PublicationOp::ConnectStart { proof } => proof,
            _ => panic!("start fixture only"),
        };
        match field {
            "expires_at" => proof.ticket.expires_at += 1,
            "state_hash" => proof.ticket.state_hash = RawDigest::of(b"tampered"),
            "ticket_id" => proof.ticket.ticket_id = id(),
            "generation" => proof.ticket.generation = id(),
            _ => panic!("unknown tamper"),
        }
    }
}
impl TypedPublicationPlan {
    fn has_connect(&self) -> bool {
        self.op.connect().is_some()
            || self.before.ticket.is_some()
            || self.after.ticket.is_some()
            || self
                .before
                .mutation
                .as_ref()
                .is_some_and(|m| m.kind != MutationKind::Config)
            || self
                .after
                .mutation
                .as_ref()
                .is_some_and(|m| m.kind != MutationKind::Config)
    }
    fn validate(&self) -> Result<()> {
        self.before.validate()?;
        self.after.validate()?;
        self.op.check_existing_tickets(&self.before, &self.after)?;
        if self.after.generation.is_none() {
            return Err(Error::Invalid);
        }
        let after_gen = self.after.generation.as_ref().ok_or(Error::Invalid)?;
        let after_mutation = self.after.mutation.as_ref();
        let before_mutation = self.before.mutation.as_ref();
        match (&self.source_write, &self.source_after) {
            (None, None) => {}
            (Some(write), Some(target)) => {
                let m = after_mutation.ok_or(Error::Invalid)?;
                if Some(&m.expected_before) != self.source.as_ref()
                    || &m.expected_after != target
                    || m.source_write.as_ref() != Some(write)
                {
                    return Err(Error::Invalid);
                }
                let allowed = matches!(
                    (&self.op, write.kind),
                    (PublicationOp::ConnectStart { .. }, SourceFileKind::Client)
                        | (PublicationOp::ConfigFence, SourceFileKind::Config)
                        | (PublicationOp::ConfigFence, SourceFileKind::Client)
                        | (
                            PublicationOp::ConfigFenceTickets { .. },
                            SourceFileKind::Config
                        )
                        | (
                            PublicationOp::ConfigFenceTickets { .. },
                            SourceFileKind::Client
                        )
                        | (PublicationOp::ClaimedPayload { .. }, SourceFileKind::Config)
                );
                if !allowed {
                    return Err(Error::Invalid);
                }
            }
            _ => return Err(Error::Invalid),
        }
        let pending_matches = |proof: &ConnectProof, mutation: Option<&Mutation>| match (
            &proof.mutation_binding,
            mutation,
        ) {
            (MutationBinding::Pending { mutation_id }, Some(m)) => mutation_id == &m.mutation_id,
            _ => false,
        };
        if let Some(proof) = self.op.connect() {
            let ticket_source = match self.op {
                PublicationOp::ConnectFinish { .. } => before_mutation.map(|m| &m.expected_before),
                PublicationOp::ConnectStart { .. } => {
                    self.source_after.as_ref().or(self.source.as_ref())
                }
                _ => self.source.as_ref(),
            };
            if !proof.valid() || ticket_source.is_none_or(|s| !proof.ticket.matches_source(s)) {
                return Err(Error::Invalid);
            }
        }
        let valid = match (&self.op, &self.producer) {
            (
                PublicationOp::MetadataMigration | PublicationOp::MetadataMigrationTickets { .. },
                ProducerProof::Metadata {
                    generation,
                    credential,
                },
            ) => {
                generation == after_gen
                    && credential == &self.after.credential
                    && self.before.ticket == self.after.ticket
                    && before_mutation.is_none()
                    && after_mutation.is_none()
                    && (self.before == self.after || self.before.generation.is_none())
            }
            (
                PublicationOp::ConnectStart { proof },
                ProducerProof::Start {
                    generation,
                    mutation_id,
                    ticket,
                },
            ) => {
                ticket == &proof.ticket
                    && generation == after_gen
                    && generation == &ticket.generation
                    && self.before.generation.as_ref() != Some(generation)
                    && self.after.ticket.as_ref() == Some(ticket)
                    && after_mutation.is_some_and(|m| {
                        m.kind == MutationKind::ConnectStart && &m.mutation_id == mutation_id
                    })
                    && pending_matches(proof, after_mutation)
            }
            (PublicationOp::ConnectPublishStart { proof }, ProducerProof::Ticket { ticket }) => {
                ticket == &proof.ticket
                    && self.before.ticket.as_ref() == Some(ticket)
                    && self.after.ticket.as_ref() == Some(ticket)
                    && self.before.generation == self.after.generation
                    && after_mutation.is_none()
                    && before_mutation.is_some_and(|m| m.kind == MutationKind::ConnectStart)
                    && pending_matches(proof, before_mutation)
            }
            (PublicationOp::TicketClaim { proof }, ProducerProof::Claim { ticket }) => {
                let mut pending = ticket.clone();
                pending.phase = Phase::Pending;
                ticket == &proof.ticket
                    && ticket.phase == Phase::Exchanging
                    && self.before.ticket.as_ref() == Some(&pending)
                    && self.after.ticket.as_ref() == Some(ticket)
                    && self.before.generation == self.after.generation
                    && before_mutation.is_none()
                    && after_mutation.is_none()
                    && matches!(&proof.mutation_binding, MutationBinding::TicketOnly { ticket_id } if ticket_id == &ticket.ticket_id)
            }
            (
                PublicationOp::BeginClaimedFinish { proof },
                ProducerProof::Change {
                    generation,
                    mutation_id,
                    claimed_ticket,
                },
            ) => {
                generation == after_gen
                    && self.before.generation == self.after.generation
                    && self.before.ticket.as_ref() == Some(&proof.ticket)
                    && self.after.ticket == self.before.ticket
                    && claimed_ticket.as_ref() == Some(&proof.ticket.ticket_id)
                    && after_mutation.is_some_and(|m| {
                        m.kind == MutationKind::ConnectFinish && &m.mutation_id == mutation_id
                    })
                    && pending_matches(proof, after_mutation)
            }
            (PublicationOp::ClaimedPayload { proof }, ProducerProof::Unit) => {
                self.before.ticket.as_ref() == Some(&proof.ticket)
                    && self.after.ticket == self.before.ticket
                    && self.before.generation == self.after.generation
                    && self.before.mutation == self.after.mutation
                    && after_mutation.is_some_and(|m| {
                        m.kind == MutationKind::ConnectFinish
                            && m.payload.as_ref().is_some_and(|p| {
                                Some(&p.digest) == self.after.payload_digest.as_ref()
                                    && self.after.credential.as_ref().is_some_and(|c| {
                                        c.credential_id == p.credential_id
                                            && c.logical_epoch == p.logical_epoch
                                            && c.token_revision == 1
                                    })
                            })
                    })
                    && pending_matches(proof, after_mutation)
            }
            (PublicationOp::ConnectFinish { proof }, ProducerProof::Unit) => {
                self.before.ticket.as_ref() == Some(&proof.ticket)
                    && self.after.ticket.is_none()
                    && self.before.generation != self.after.generation
                    && after_mutation.is_none()
                    && self.before.credential == self.after.credential
                    && self.before.payload_digest == self.after.payload_digest
                    && before_mutation.is_some_and(|m| m.kind == MutationKind::ConnectFinish)
                    && pending_matches(proof, before_mutation)
            }
            (
                PublicationOp::ConfigFence | PublicationOp::ConfigFenceTickets { .. },
                ProducerProof::Change {
                    generation,
                    mutation_id,
                    claimed_ticket,
                },
            ) => {
                generation == after_gen
                    && self.before.generation != self.after.generation
                    && claimed_ticket.is_none()
                    && self.after.ticket.is_none()
                    && after_mutation.is_some_and(|m| {
                        m.kind == MutationKind::Config && &m.mutation_id == mutation_id
                    })
                    && !before_mutation.is_some_and(|m| m.kind == MutationKind::ConnectFinish)
            }
            (PublicationOp::ConfigFinish, ProducerProof::Unit) => {
                self.before.generation == self.after.generation
                    && before_mutation.is_some_and(|m| m.kind == MutationKind::Config)
                    && after_mutation.is_none()
                    && self.after.ticket.is_none()
                    && self.before.ticket.is_none()
                    && self.before.payload_digest == self.after.payload_digest
                    && self.before.credential == self.after.credential
            }
            (
                PublicationOp::Refresh | PublicationOp::RefreshTickets { .. },
                ProducerProof::Unit,
            ) => {
                before_mutation.is_none()
                    && after_mutation.is_none()
                    && self
                        .before
                        .credential
                        .as_ref()
                        .zip(self.after.credential.as_ref())
                        .is_some_and(|(b, a)| {
                            b.credential_id == a.credential_id
                                && b.token_revision.checked_add(1) == Some(a.token_revision)
                                && (a.logical_epoch == b.logical_epoch
                                    || b.logical_epoch.checked_add(1) == Some(a.logical_epoch))
                        })
            }
            (PublicationOp::Revoke | PublicationOp::RevokeTickets { .. }, ProducerProof::Unit) => {
                self.before.generation != self.after.generation
                    && self.after.credential.is_none()
                    && self.after.payload_digest.is_none()
                    && self.after.ticket.is_none()
                    && after_mutation.is_none()
            }
            _ => false,
        };
        // Nonconnect operations do not discard their before/after ticket
        // evidence: has_connect permanently prevents generic stabilization.
        if !valid {
            return Err(Error::Invalid);
        }
        if !matches!(
            self.op,
            PublicationOp::Revoke | PublicationOp::RevokeTickets { .. }
        ) && self.source.is_none()
        {
            return Err(Error::Invalid);
        }
        if let Some(s) = &self.source {
            let expected = match self.op {
                PublicationOp::ConfigFinish | PublicationOp::ConnectFinish { .. } => {
                    before_mutation.map(|m| &m.expected_after)
                }
                PublicationOp::ConnectStart { .. }
                | PublicationOp::ConfigFence
                | PublicationOp::ConfigFenceTickets { .. }
                | PublicationOp::BeginClaimedFinish { .. } => {
                    after_mutation.map(|m| &m.expected_before)
                }
                _ => None,
            };
            if expected.is_some_and(|e| e != s) {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }
    fn check_source_before(&self, g: &SourceGuard) -> Result<()> {
        if let Some(expected) = &self.source {
            home_source::revalidate_binding(g, expected)?;
        }
        // Finish consumes the same mutation; its held Config proof must
        // still match actual AFTER bytes at both final publication fences.
        if matches!(&self.op, PublicationOp::ConnectFinish { .. }) {
            if let Some(write) = self
                .before
                .mutation
                .as_ref()
                .and_then(|m| m.source_write.as_ref())
            {
                verify_source_file(g.home(), write, false)?;
            }
        }
        Ok(())
    }
    fn check_source_after(&self, g: &SourceGuard) -> Result<()> {
        if let Some(expected) = self.source_after.as_ref().or(self.source.as_ref()) {
            home_source::revalidate_binding(g, expected)?;
        }
        if let Some(write) = &self.source_write {
            verify_source_file(g.home(), write, false)?;
        }
        // Finish consumes the same mutation; its held Config proof must
        // still match actual AFTER bytes at both final publication fences.
        if matches!(&self.op, PublicationOp::ConnectFinish { .. }) {
            if let Some(write) = self
                .before
                .mutation
                .as_ref()
                .and_then(|m| m.source_write.as_ref())
            {
                verify_source_file(g.home(), write, false)?;
            }
        }
        Ok(())
    }
    fn fresh_ticket(&self, home: &Path) -> Result<()> {
        if self
            .op
            .connect()
            .is_some_and(|p| p.ticket.expires_at <= host_now(home))
        {
            Err(Error::Stale)
        } else {
            Ok(())
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationIntent {
    schema: u32,
    publication_id: String,
    plan: TypedPublicationPlan,
}
impl PublicationIntent {
    fn valid(&self) -> Result<()> {
        if self.schema != 1 || !valid_id(&self.publication_id) {
            return Err(Error::Invalid);
        }
        self.plan.validate()
    }
}
pub(super) enum PublicationDisposition<T> {
    NotPublished { publication_id: String },
    PublishedDurabilityUnconfirmed { publication_id: String },
    Committed { value: T },
    CommittedCleanupUnconfirmed { publication_id: String },
}
impl<T> PublicationDisposition<T> {
    fn into_result(self) -> Result<T> {
        match self {
            Self::Committed { value } => Ok(value),
            Self::NotPublished { .. } => Err(Error::NotPublished),
            Self::PublishedDurabilityUnconfirmed { .. } => {
                Err(Error::PublishedDurabilityUnconfirmed)
            }
            Self::CommittedCleanupUnconfirmed { .. } => Err(Error::CommittedCleanupUnconfirmed),
        }
    }
}
/// Only an unambiguous NotFound is absence; malformed/partial/inaccessible
/// gates block identically, including legacy Google rows with no control.
pub(super) fn blocked(home: &Path) -> bool {
    !matches!(std::fs::symlink_metadata(home.join(GATE_FILE)), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}
pub(super) fn ensure_unblocked(home: &Path) -> Result<()> {
    if blocked(home) {
        Err(Error::PublicationBlocked)
    } else {
        Ok(())
    }
}
fn options() -> OpenOptions {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    o
}
pub(super) fn sync_dir(home: &Path) -> Result<()> {
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        // CreateFileW requires BACKUP_SEMANTICS for directories; flushing
        // with FlushFileBuffers also requires a writable handle.
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(home)
    };
    #[cfg(not(windows))]
    let file = File::open(home);
    file.and_then(|f| f.sync_all()).map_err(|_| Error::Io)
}
fn sync_existing_file(path: &Path) -> Result<()> {
    #[cfg(windows)]
    let file = options().write(true).open(path);
    #[cfg(not(windows))]
    let file = options().open(path);
    file.and_then(|f| f.sync_all()).map_err(|_| Error::Io)
}
fn gate_bytes(home: &Path) -> Result<Vec<u8>> {
    let path = home.join(GATE_FILE);
    let metadata = std::fs::symlink_metadata(&path).map_err(|_| Error::Io)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_GATE {
        return Err(Error::Invalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(Error::Invalid);
        }
    }
    let mut file = options().open(path).map_err(|_| Error::Io)?;
    let mut b = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_GATE + 1)
        .read_to_end(&mut b)
        .map_err(|_| Error::Io)?;
    if b.len() as u64 > MAX_GATE {
        return Err(Error::Invalid);
    }
    Ok(b)
}
fn write_gate(home: &Path, intent: &PublicationIntent) -> Result<()> {
    let bytes = serde_json::to_vec(intent).map_err(|_| Error::Invalid)?;
    if bytes.len() as u64 > MAX_GATE {
        return Err(Error::Invalid);
    }
    hit(home, Fault::GateCreate)?;
    let mut file = options()
        .write(true)
        .create_new(true)
        .open(home.join(GATE_FILE))
        .map_err(|_| Error::Io)?;
    hit(home, Fault::GateWrite)?;
    file.write_all(&bytes).map_err(|_| Error::Io)?;
    hit(home, Fault::GateFileSync)?;
    file.sync_all().map_err(|_| Error::Io)?;
    hit(home, Fault::GateDirSync)?;
    sync_dir(home)
}
fn cleanup(home: &Path) -> Result<()> {
    hit(home, Fault::GateUnlink)?;
    std::fs::remove_file(home.join(GATE_FILE)).map_err(|_| Error::Io)?;
    hit(home, Fault::CleanupDirSync)?;
    sync_dir(home)
}
pub(crate) fn publication_transaction<T>(
    g: &SourceGuard,
    f: impl FnOnce(&mut Rows) -> Result<Planned<T>>,
) -> Result<T> {
    publication_transaction_checked(g, || Ok(()), f)
}
/// RAM-only callback freshness hook. It cannot replace any sealed plan check,
/// alter a producer, or extend gate lifetime. Existing callers use no-op.
pub(crate) fn publication_transaction_checked<T>(
    g: &SourceGuard,
    check: impl Fn() -> Result<()>,
    f: impl FnOnce(&mut Rows) -> Result<Planned<T>>,
) -> Result<T> {
    duduclaw_core::with_file_lock(&g.home().join(TOKEN_FILE), || {
        Ok((|| {
            check()?; // 1: lock acquired, before producer.
            ensure_unblocked(g.home())?;
            let mut rows = Rows::read(g.home())?;
            let original = RowProof::of(rows.google())?;
            let made = f(&mut rows)?;
            check()?; // 2: producer complete, before any publication.
            made.plan.validate()?;
            if (made.proof_of)(&made.value) != made.plan.producer
                || original != made.plan.before
                || RowProof::of(rows.google())? != made.plan.after
            {
                return Err(Error::Invalid);
            }
            made.plan.check_source_before(g)?;
            if made.staged_source.as_ref().map(|w| &w.proof) != made.plan.source_write.as_ref() {
                return Err(Error::Invalid);
            }
            if let Some(write) = &made.staged_source {
                verify_source_file(g.home(), &write.proof, true)?;
            }
            made.plan.fresh_ticket(g.home())?;
            if !Rows::read(g.home())?.same_original(&rows) {
                return Err(Error::Stale);
            }
            if original == made.plan.after {
                if !matches!(
                    made.plan.op,
                    PublicationOp::MetadataMigration
                        | PublicationOp::MetadataMigrationTickets { .. }
                ) {
                    return Err(Error::Invalid);
                }
                if digest(&Value::Array(rows.rows.clone()))? != rows.original_rows_digest {
                    return Err(Error::Invalid);
                }
                return Ok(made.value);
            }
            // Exact actual raw output is bounded BEFORE creating a gate.
            // Keep these owned bytes through the data write; do not re-render
            // a second serializer after an irreversible durability boundary.
            let prepared = rows.prepare_write()?;
            let intent = PublicationIntent {
                schema: 1,
                publication_id: id(),
                plan: made.plan,
            };
            let publication_id = intent.publication_id.clone();
            check()?; // 3: actual bytes prepared, before gate.
            let result = if write_gate(g.home(), &intent).is_err() {
                PublicationDisposition::NotPublished { publication_id }
            } else if check().is_err() {
                // 4: gate durable, no data attempted.
                PublicationDisposition::NotPublished { publication_id }
            } else {
                match atomic_write(g.home(), &prepared) {
                    Err(DataWriteFailure::BeforeRename) => {
                        PublicationDisposition::NotPublished { publication_id }
                    }
                    Err(DataWriteFailure::AfterRename) => {
                        PublicationDisposition::PublishedDurabilityUnconfirmed { publication_id }
                    }
                    Ok(()) => {
                        let final_check = (|| {
                            check()?; // 5: data durable; refusal keeps gate.
                            hit(g.home(), Fault::FinalProof)?;
                            if RowProof::of(Rows::read(g.home())?.google())? != intent.plan.after {
                                return Err(Error::Stale);
                            }
                            check()?; // 6: immediately before source payload.
                            if let Some(write) = &made.staged_source {
                                write.publish(g.home())?;
                            }
                            check()?; // 7: immediately after source payload.
                            intent.plan.check_source_after(g)?;
                            intent.plan.fresh_ticket(g.home())?;
                            check() // 8: source payload/after proof complete.
                        })();
                        if final_check.is_err() {
                            PublicationDisposition::PublishedDurabilityUnconfirmed {
                                publication_id,
                            }
                        } else if check().is_err() {
                            // 9: before cleanup; keep gate.
                            PublicationDisposition::PublishedDurabilityUnconfirmed {
                                publication_id,
                            }
                        } else if cleanup(g.home()).is_err() {
                            PublicationDisposition::CommittedCleanupUnconfirmed { publication_id }
                        } else if check().is_err() {
                            // 10: durable commit, no caller T.
                            PublicationDisposition::CommittedCleanupUnconfirmed { publication_id }
                        } else {
                            PublicationDisposition::Committed { value: made.value }
                        }
                    }
                }
            };
            result.into_result()
        })())
    })
    .map_err(|_| Error::Io)?
}

/// Closed path kind; the Pending row and the one existing publication gate
/// persist only its digests. Payload bytes are owned, zeroizing RAM only.
pub(crate) struct PreparedSourceWrite {
    proof: SourceFileProof,
    bytes: zeroize::Zeroizing<Vec<u8>>,
}
impl PreparedSourceWrite {
    pub(super) fn client(before: Option<&[u8]>, after: Vec<u8>) -> Result<Self> {
        if after.len() > 4 * 1024 * 1024 {
            return Err(Error::Invalid);
        }
        let proof = SourceFileProof {
            kind: SourceFileKind::Client,
            before_digest: source_bytes_digest(before),
            after_digest: source_bytes_digest(Some(&after)),
        };
        Ok(Self {
            proof,
            bytes: zeroize::Zeroizing::new(after),
        })
    }
    pub(super) fn config(before: Option<&[u8]>, after: Vec<u8>) -> Result<Self> {
        if after.len() > 4 * 1024 * 1024 {
            return Err(Error::Invalid);
        }
        let proof = SourceFileProof {
            kind: SourceFileKind::Config,
            before_digest: source_bytes_digest(before),
            after_digest: source_bytes_digest(Some(&after)),
        };
        Ok(Self {
            proof,
            bytes: zeroize::Zeroizing::new(after),
        })
    }
    pub(super) fn proof(&self) -> &SourceFileProof {
        &self.proof
    }
    pub(super) fn publish(&self, home: &Path) -> Result<()> {
        verify_source_file(home, &self.proof, true)?;
        #[cfg(test)]
        super::test_probe::source_payload(home);
        let target = source_path(home, self.proof.kind);
        let tmp = home.join(format!(
            ".google-source-payload-{}.tmp",
            uuid::Uuid::new_v4()
        ));
        struct Temp(std::path::PathBuf);
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _temp = Temp(tmp.clone());
        hit(home, Fault::SourceOpen)?;
        let mut file = options()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|_| Error::Io)?;
        hit(home, Fault::SourceWrite)?;
        file.write_all(&self.bytes).map_err(|_| Error::Io)?;
        hit(home, Fault::SourceFileSync)?;
        file.sync_all().map_err(|_| Error::Io)?;
        hit(home, Fault::SourceRename)?;
        std::fs::rename(&tmp, &target).map_err(|_| Error::Io)?;
        hit(home, Fault::SourceDirSync)?;
        sync_dir(home)
    }
}
fn source_path(home: &Path, kind: SourceFileKind) -> std::path::PathBuf {
    home.join(match kind {
        SourceFileKind::Client => super::super::CLIENT_CONFIG_FILE,
        SourceFileKind::Config => "config.toml",
    })
}
fn source_bytes_digest(bytes: Option<&[u8]>) -> RawDigest {
    let mut hash = Sha256::new();
    hash.update([u8::from(bytes.is_some())]);
    if let Some(bytes) = bytes {
        hash.update(bytes);
    }
    RawDigest(hash.finalize().into())
}
pub(super) fn read_source_file(
    home: &Path,
    kind: SourceFileKind,
) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
    let path = source_path(home, kind);
    let mut file = match options().open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::Io),
    };
    if !file.metadata().map_err(|_| Error::Io)?.is_file() {
        return Err(Error::Invalid);
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    std::io::Read::by_ref(&mut file)
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Io)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(Error::Invalid);
    }
    Ok(Some(bytes))
}
fn verify_source_file(home: &Path, proof: &SourceFileProof, before: bool) -> Result<()> {
    let bytes = read_source_file(home, proof.kind)?;
    let actual = source_bytes_digest(bytes.as_ref().map(|b| b.as_slice()));
    let expected = if before {
        &proof.before_digest
    } else {
        &proof.after_digest
    };
    if &actual == expected {
        Ok(())
    } else {
        Err(Error::Stale)
    }
}

/// Opaque inspection also represents malformed regular gates, so explicit
/// abort can destroy uncertain payload without trusting a partial intent.
pub(crate) struct PublicationInspection {
    digest: RawDigest,
}
pub(crate) fn inspect_publication(home: &Path) -> Result<PublicationInspection> {
    home_source::with_source_guard(home, |g| {
        duduclaw_core::with_file_lock(&g.home().join(TOKEN_FILE), || {
            Ok(gate_bytes(g.home()).map(|b| PublicationInspection {
                digest: RawDigest::of(&b),
            }))
        })
        .map_err(|_| Error::Io)?
    })
}
fn exact_intent(g: &SourceGuard, expected: &PublicationInspection) -> Result<PublicationIntent> {
    let bytes = gate_bytes(g.home())?;
    if RawDigest::of(&bytes) != expected.digest {
        return Err(Error::Stale);
    }
    let intent: PublicationIntent = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
    intent.valid()?;
    Ok(intent)
}
pub(crate) fn stabilize_exact_publication(
    home: &Path,
    expected: &PublicationInspection,
) -> Result<()> {
    home_source::with_exact_publication_recovery(home, expected, |g, _| {
        duduclaw_core::with_file_lock(&g.home().join(TOKEN_FILE), || {
            Ok((|| {
                let intent = exact_intent(g, expected)?;
                if intent.plan.has_connect() {
                    return Err(Error::NotReady);
                }
                if RowProof::of(Rows::read(g.home())?.google())? != intent.plan.after {
                    return Err(Error::Stale);
                }
                intent.plan.check_source_after(g)?;
                sync_existing_file(&g.home().join(TOKEN_FILE))
                    .map_err(|_| Error::PublishedDurabilityUnconfirmed)?;
                if let Some(write) = &intent.plan.source_write {
                    verify_source_file(g.home(), write, false)?;
                    sync_existing_file(&source_path(g.home(), write.kind))
                        .map_err(|_| Error::PublishedDurabilityUnconfirmed)?;
                }
                sync_dir(g.home()).map_err(|_| Error::PublishedDurabilityUnconfirmed)?;
                // Recheck actual bytes/source under the same lock. Recovery never
                // writes rows, enables a source, returns T or replays a provider.
                if RowProof::of(Rows::read(g.home())?.google())? != intent.plan.after {
                    return Err(Error::Stale);
                }
                intent.plan.check_source_after(g)?;
                cleanup(g.home()).map_err(|_| Error::CommittedCleanupUnconfirmed)
            })())
        })
        .map_err(|_| Error::Io)?
    })
}
pub(crate) fn abort_publication_with_revoke(
    home: &Path,
    expected: &PublicationInspection,
) -> Result<()> {
    home_source::with_source_guard(home, |g| {
        duduclaw_core::with_file_lock(&g.home().join(TOKEN_FILE), || {
            Ok((|| {
                let b = gate_bytes(g.home())?;
                if RawDigest::of(&b) != expected.digest {
                    return Err(Error::Stale);
                }
                // Even a partial regular gate must become durable before abort
                // writes a fresh tombstone. It is never replaced with a flag.
                sync_existing_file(&g.home().join(GATE_FILE))
                    .map_err(|_| Error::NotPublished)?;
                sync_dir(g.home()).map_err(|_| Error::NotPublished)?;
                let mut rows = Rows::read(g.home())?;
                let made = rows.revoke()?;
                made.plan.validate()?;
                if !Rows::read(g.home())?.same_original(&rows) {
                    return Err(Error::Stale);
                }
                let prepared = rows.prepare_write()?;
                atomic_write(g.home(), &prepared).map_err(|e| match e {
                    DataWriteFailure::BeforeRename => Error::NotPublished,
                    DataWriteFailure::AfterRename => Error::PublishedDurabilityUnconfirmed,
                })?;
                if RowProof::of(Rows::read(g.home())?.google())? != made.plan.after {
                    return Err(Error::PublishedDurabilityUnconfirmed);
                }
                cleanup(g.home()).map_err(|_| Error::CommittedCleanupUnconfirmed)
            })())
        })
        .map_err(|_| Error::Io)?
    })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    GateCreate,
    GateWrite,
    GateFileSync,
    GateDirSync,
    DataOpen,
    DataWrite,
    DataFileSync,
    DataRename,
    DataDirSync,
    FinalProof,
    SourceOpen,
    SourceWrite,
    SourceFileSync,
    SourceRename,
    SourceDirSync,
    GateUnlink,
    CleanupDirSync,
}
#[cfg(test)]
thread_local! { static INJECT: std::cell::RefCell<Option<(std::path::PathBuf, Fault)>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
pub(super) struct FaultGuard;
#[cfg(test)]
impl FaultGuard {
    pub(super) fn at(home: &Path, fault: Fault) -> Self {
        INJECT.with(|s| {
            assert!(s.borrow().is_none());
            *s.borrow_mut() = Some((home.canonicalize().unwrap(), fault));
        });
        Self
    }
}
#[cfg(test)]
impl Drop for FaultGuard {
    fn drop(&mut self) {
        INJECT.with(|s| {
            *s.borrow_mut() = None;
        });
    }
}
pub(super) fn hit(home: &Path, fault: Fault) -> Result<()> {
    #[cfg(test)]
    {
        if INJECT.with(|s| {
            s.borrow()
                .as_ref()
                .is_some_and(|(h, f)| h == home && *f == fault)
        }) {
            return Err(Error::Io);
        }
    }
    let _ = (home, fault);
    Ok(())
}
#[cfg(test)]
thread_local! { static CLOCK: std::cell::Cell<Option<(u64,u64)>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
pub(super) struct ClockGuard {
    installed: bool,
}
#[cfg(test)]
impl ClockGuard {
    /// Fixture helpers may nest beneath an explicit expiry clock. Only the
    /// guard that installed a clock may clear it on Drop.
    pub(super) fn fixed_if_unset(now: u64) -> Self {
        let installed = CLOCK.with(|s| {
            if s.get().is_some() {
                false
            } else {
                s.set(Some((now, now)));
                true
            }
        });
        Self { installed }
    }
    pub(super) fn after_preflight(before: u64, after: u64) -> Self {
        CLOCK.with(|s| {
            assert!(s.get().is_none());
            s.set(Some((before, after)));
        });
        Self { installed: true }
    }
}
#[cfg(test)]
impl Drop for ClockGuard {
    fn drop(&mut self) {
        if self.installed {
            CLOCK.with(|s| s.set(None));
        }
    }
}
pub(super) fn host_now(_home: &Path) -> u64 {
    #[cfg(test)]
    {
        if let Some((now, next)) = CLOCK.with(|s| s.get()) {
            CLOCK.with(|s| s.set(Some((next, next))));
            return now;
        }
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(u64::MAX, |d| d.as_secs())
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    const NOW: u64 = 1_800_000_000;
    #[test]
    fn fixed_fixture_clock_is_nested_safe_and_preserves_explicit_expiry_clock() {
        assert!(CLOCK.with(|s| s.get()).is_none());
        {
            let _fixed = ClockGuard::fixed_if_unset(NOW);
            assert_eq!(host_now(Path::new("unused")), NOW);
            {
                let _nested = ClockGuard::fixed_if_unset(NOW + 99);
                assert_eq!(host_now(Path::new("unused")), NOW);
            }
            assert_eq!(host_now(Path::new("unused")), NOW);
        }
        assert!(CLOCK.with(|s| s.get()).is_none());
        {
            let _expiry = ClockGuard::after_preflight(NOW, NOW + 501);
            {
                let _fixture = ClockGuard::fixed_if_unset(NOW);
                assert_eq!(host_now(Path::new("unused")), NOW);
            }
            assert_eq!(host_now(Path::new("unused")), NOW + 501);
        }
        assert!(CLOCK.with(|s| s.get()).is_none());
    }
}

#[cfg(all(test, windows))]
mod windows_durability_tests {
    use super::*;

    #[test]
    fn directory_sync_succeeds_after_create_replace_and_remove() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("target");
        std::fs::write(&target, b"old").unwrap();
        sync_dir(d.path()).unwrap();
        let replacement = d.path().join("replacement");
        std::fs::write(&replacement, b"new").unwrap();
        std::fs::rename(replacement, &target).unwrap();
        sync_dir(d.path()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        std::fs::remove_file(target).unwrap();
        sync_dir(d.path()).unwrap();
    }

    #[test]
    fn existing_file_sync_preserves_bytes_and_never_creates_a_missing_file() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("existing");
        let expected = b"owned bytes must not be truncated";
        std::fs::write(&path, expected).unwrap();
        sync_existing_file(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        let missing = d.path().join("missing");
        assert_eq!(sync_existing_file(&missing), Err(Error::Io));
        assert!(!missing.exists());
    }

    #[test]
    fn exact_source_recovery_flushes_existing_files_without_replaying_the_write() {
        use crate::handlers::config_commit;
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("config.toml");
        let before = "[integrations]\ngoogle_workspace=false\n";
        let after = "[integrations]\ngoogle_workspace=true\n";
        std::fs::write(&path, before).unwrap();
        let probe = super::super::test_probe::Guard::install(d.path());
        let fault = FaultGuard::at(d.path(), Fault::SourceDirSync);
        assert!(
            config_commit::commit_text_locked(&path, &config_commit::content_hash(before), after)
                .is_err()
        );
        drop(fault);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
        assert!(blocked(d.path()));
        assert_eq!(probe.writer_counts(), [1, 1]);
        let token_path = d.path().join(TOKEN_FILE);
        let committed_tokens = std::fs::read(&token_path).unwrap();
        let inspection = inspect_publication(d.path()).unwrap();
        stabilize_exact_publication(d.path(), &inspection).unwrap();
        assert!(!blocked(d.path()));
        assert_eq!(std::fs::read(token_path).unwrap(), committed_tokens);
        assert_eq!(std::fs::read_to_string(path).unwrap(), after);
        assert_eq!(probe.writer_counts(), [1, 1]);
        assert!(!d.path().join(".keyfile").exists());
    }

    #[test]
    fn partial_gate_abort_flushes_a_tombstone_and_preserves_other_provider_fields() {
        let d = tempfile::tempdir().unwrap();
        let token_path = d.path().join(TOKEN_FILE);
        let other = json!({
            "provider_id": "github",
            "access_token": "enc:v1:owned-unusable",
            "future_field": {"keep": true}
        });
        std::fs::write(&token_path, serde_json::to_vec(&json!([other.clone()])).unwrap())
            .unwrap();
        std::fs::write(d.path().join(GATE_FILE), b"{partial-gate").unwrap();
        assert!(blocked(d.path()));
        let inspection = inspect_publication(d.path()).unwrap();
        abort_publication_with_revoke(d.path(), &inspection).unwrap();
        assert!(!blocked(d.path()));
        let rows: Value = serde_json::from_slice(&std::fs::read(token_path).unwrap()).unwrap();
        assert_eq!(rows[0], other);
        assert_eq!(rows[1]["provider_id"], "google");
        assert!(rows[1].get("access_token").is_none());
        assert!(rows[1]["google_home"]["credential"].is_null());
        assert!(!d.path().join(".keyfile").exists());
    }
}
