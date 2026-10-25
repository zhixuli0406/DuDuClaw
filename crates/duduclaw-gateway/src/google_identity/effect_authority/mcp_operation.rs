//! One-shot owned operation. Only owner-completed typed replies cross out.
use super::*;
use duduclaw_llm::{OwnedScopedFailure, OwnedScopedOperation, OwnedScopedReply};

pub(super) struct GoogleMcpOperation {
    authority: GoogleEffectAuthority,
}
impl GoogleMcpOperation {
    pub(super) fn new(authority: GoogleEffectAuthority) -> Self {
        Self { authority }
    }
}
#[async_trait::async_trait]
impl OwnedScopedOperation for GoogleMcpOperation {
    async fn execute(self: Box<Self>) -> Result<OwnedScopedReply, OwnedScopedFailure> {
        let authority = &self.authority;
        authority
            .validate()
            .map_err(|_| OwnedScopedFailure::Denied)?;
        let GooglePurpose::ClosedRemote(bound) = &authority.material.context.purpose else {
            return Err(OwnedScopedFailure::Denied);
        };
        let reply = match &authority.material.selected {
            Material::Variant(material) => material.mcp(authority, bound).await?,
            Material::Personal(material) => material
                .execute_mcp(PersonalMcpEffectPermit { authority }, bound)
                .await?
                .into_reply(),
            Material::HomeOAuth(material) => material
                .borrow_for_effect(HomeOAuthEffectPermit { authority })
                .map_err(|_| OwnedScopedFailure::Denied)?
                .execute_mcp(bound)
                .await?
                .into_reply(),
        };
        authority
            .validate()
            .map_err(|_| OwnedScopedFailure::Unconfirmed)?;
        Ok(reply)
    }
}
pub(crate) struct PersonalMcpEffectPermit<'a> {
    authority: &'a GoogleEffectAuthority,
}
impl PersonalMcpEffectPermit<'_> {
    pub(crate) fn validate(
        &self,
        material: &PinnedPersonalToken,
        bound: &RemoteRequestBound,
    ) -> Result<(), Refusal> {
        match &self.authority.material.selected {
            Material::Personal(expected) if std::ptr::eq(expected, material) => {
                validate_bound(self.authority, bound)
            }
            _ => Err(changed()),
        }
    }
}
pub(super) fn validate_bound(
    authority: &GoogleEffectAuthority,
    bound: &RemoteRequestBound,
) -> Result<(), Refusal> {
    let GooglePurpose::ClosedRemote(expected) = &authority.material.context.purpose else {
        return Err(changed());
    };
    if !std::ptr::eq(expected, bound) {
        return Err(changed());
    }
    authority.validate()
}
impl HomeOAuthEffectPermit<'_> {
    pub(crate) fn validate_mcp(
        &self,
        material: &HomeOAuthResolved,
        bound: &RemoteRequestBound,
    ) -> Result<(), Refusal> {
        self.validate_home_oauth(material)?;
        validate_bound(self.authority, bound)
    }
}
#[cfg(test)]
impl PersonalMcpEffectPermit<'_> {
    pub(crate) fn test_boundary(&self, stage: &str) {
        super::remote_claim::testing::boundary(&self.authority.material.context.home, stage)
    }
    pub(crate) fn test_completed(&self, reply: &OwnedScopedReply) {
        super::remote_claim::testing::completed(&self.authority.material.context.home, reply)
    }
}
#[cfg(test)]
impl HomeOAuthEffectPermit<'_> {
    pub(crate) fn test_mcp_boundary(&self, stage: &str) {
        super::remote_claim::testing::boundary(&self.authority.material.context.home, stage)
    }
    pub(crate) fn test_mcp_completed(&self, reply: &OwnedScopedReply) {
        super::remote_claim::testing::completed(&self.authority.material.context.home, reply)
    }
}
