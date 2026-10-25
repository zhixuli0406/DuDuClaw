//! Host-only call state. Model params/_meta can never nominate a selection.
use super::{AuthenticatedCall, Principal};
use duduclaw_core::google_identity::{self as gi, Effective, GoogleSelectionStamp};
use serde_json::Value;
use std::path::Path;

pub(crate) struct GoogleLane {
    receipt: AuthenticatedCall,
    tool: String,
    prepared: bool,
    initial_identity: Option<Effective>,
    selection: Option<GoogleSelectionStamp>,
    original_arguments: Option<Value>,
    final_arguments: Option<Value>,
    mapping_receipt: Option<duduclaw_redaction::vault::MappingReceipt>,
    mapping_session: Option<String>,
    mapping_scopes: Option<Vec<String>>,
    restored_arguments: Option<Value>,
}
pub(crate) fn accept_google_lane(
    home: &Path,
    principal: &Principal,
    tool: &str,
) -> Option<GoogleLane> {
    super::dedicated_google_tool_allowed(home, principal, tool).then_some(())?;
    Some(GoogleLane {
        receipt: super::current_call_matches(principal)?,
        tool: tool.into(),
        prepared: false,
        initial_identity: None,
        selection: None,
        original_arguments: None,
        final_arguments: None,
        mapping_receipt: None,
        mapping_session: None,
        mapping_scopes: None,
        restored_arguments: None,
    })
}
#[derive(Clone)]
pub(crate) struct AcceptedGoogleCall {
    receipt: AuthenticatedCall,
    tool: String,
    arguments: Value,
    selection: Option<GoogleSelectionStamp>,
}
impl std::fmt::Debug for AcceptedGoogleCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcceptedGoogleCall")
            .field("employee", &self.principal().client_id)
            .field("tool", &self.tool)
            .finish_non_exhaustive()
    }
}
fn changed() -> String {
    "Google 帳號、參數或還原對應已變更，請重新選擇帳號並核准。".into()
}
impl GoogleLane {
    fn agent(&self) -> &str {
        &self.receipt.principal().client_id
    }
    fn redaction_agent(&self) -> &str {
        if self.receipt.principal().is_external {
            "external"
        } else {
            self.agent()
        }
    }
    fn reference(&self) -> Result<&gi::CredentialRef, String> {
        self.receipt.credential_ref().ok_or_else(changed)
    }
    pub(crate) fn approval_account(&self) -> Option<gi::GoogleAccountApproval> {
        self.selection.as_ref().map(|s| s.approval.clone())
    }
    fn payload(&self, params: &Value) -> Result<Value, String> {
        let mut payload = params.clone();
        let obj = payload.as_object_mut().ok_or_else(changed)?;
        obj.remove(gi::GOOGLE_ACCOUNT_APPROVAL_KEY);
        if let Some(stamp) = &self.selection {
            obj.insert(
                gi::GOOGLE_ACCOUNT_APPROVAL_KEY.into(),
                serde_json::to_value(&stamp.approval).map_err(|_| changed())?,
            );
        }
        Ok(payload)
    }
    fn check_identity(&self, home: &Path) -> Result<(), String> {
        let current = gi::effective_for(home, Some(self.agent())).effective;
        if !self.prepared
            || (self.selection.is_some() && self.initial_identity.as_ref() != Some(&current))
            || (self.selection.is_none() && matches!(current, Effective::Personal { .. }))
        {
            return Err(changed());
        }
        if !super::dedicated_google_tool_allowed(home, self.receipt.principal(), &self.tool) {
            return Err(changed());
        }
        Ok(())
    }
    fn pin_mapping(
        &mut self,
        args: &Value,
        layer: Option<&crate::mcp_redaction::McpRedactionLayer>,
    ) -> Result<(), String> {
        if let Some(layer) = layer {
            let receipt = layer
                .manager
                .vault()
                .mapping_receipt(args, self.redaction_agent(), Some(&layer.session_id))
                .map_err(|_| changed())?;
            self.mapping_receipt = Some(receipt);
            self.mapping_session = Some(layer.session_id.clone());
            self.mapping_scopes = Some(crate::mcp_redaction::redaction_scopes_snapshot());
        }
        Ok(())
    }
    pub(crate) fn check_mapping(
        &self,
        layer: Option<&crate::mcp_redaction::McpRedactionLayer>,
    ) -> Result<(), String> {
        if self.selection.is_none() {
            return Ok(());
        }
        match (&self.mapping_receipt, layer) {
            (None, None) => Ok(()),
            (Some(receipt), Some(layer))
                if self.mapping_session.as_deref() == Some(layer.session_id.as_str())
                    && self.mapping_scopes.as_ref()
                        == Some(&crate::mcp_redaction::redaction_scopes_snapshot()) =>
            {
                let args = self.original_arguments.as_ref().ok_or_else(changed)?;
                let current = layer
                    .manager
                    .vault()
                    .mapping_receipt(args, self.redaction_agent(), Some(&layer.session_id))
                    .map_err(|_| changed())?;
                if *receipt == current {
                    Ok(())
                } else {
                    Err(changed())
                }
            }
            _ => Err(changed()),
        }
    }
    /// Account and encrypted alias identity are fixed before the existing Ask
    /// wait. This does not reveal an alias value or evaluate egress early.
    pub(crate) async fn approval_payload(
        &mut self,
        home: &Path,
        params: &Value,
        layer: Option<&crate::mcp_redaction::McpRedactionLayer>,
    ) -> Result<Value, String> {
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if !self.prepared {
            if self.initial_identity.is_some() {
                return Err(changed());
            }
            let identity = gi::effective_for(home, Some(self.agent())).effective;
            self.initial_identity = Some(identity.clone());
            self.original_arguments = Some(args.clone());
            if matches!(identity, Effective::Personal { .. }) {
                self.pin_mapping(&args, layer)?;
                let selected = crate::mcp::google_selection_client::select(
                    home,
                    self.agent(),
                    self.reference()?,
                    &self.tool,
                    &args,
                )
                .await?
                .ok_or_else(changed)?;
                if selected.original_arguments_digest != gi::google_arguments_digest(&args)
                    || selected.final_arguments_digest.is_some()
                {
                    return Err(changed());
                }
                self.selection = Some(selected);
            }
            self.prepared = true;
        } else if self.original_arguments.as_ref() != Some(&args) {
            return Err(changed());
        }
        self.check_identity(home)?;
        self.check_mapping(layer)?;
        self.payload(params)
    }
    /// Called only at the existing restoration boundary, before and after it.
    /// The post value is private state, never a field supplied by the model.
    pub(crate) fn record_restored(
        &mut self,
        args: Value,
        layer: Option<&crate::mcp_redaction::McpRedactionLayer>,
    ) -> Result<(), String> {
        if self.selection.is_some() {
            self.check_mapping(layer)?;
            self.restored_arguments = Some(args);
        }
        Ok(())
    }
    pub(crate) async fn final_approval_payload(
        &mut self,
        home: &Path,
        params: &Value,
        layer: Option<&crate::mcp_redaction::McpRedactionLayer>,
    ) -> Result<Value, String> {
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if !self.prepared {
            return Err(changed());
        }
        self.check_identity(home)?;
        self.check_mapping(layer)?;
        if let Some(stamp) = &self.selection {
            // Only unchanged args or the exact value recorded immediately
            // after trusted restoration may receive the final binding.
            if self.original_arguments.as_ref() != Some(&args)
                && self.restored_arguments.as_ref() != Some(&args)
            {
                return Err(changed());
            }
            if self
                .final_arguments
                .as_ref()
                .is_some_and(|final_args| *final_args != args)
            {
                return Err(changed());
            }
            let bound = crate::mcp::google_selection_client::bind(
                home,
                self.agent(),
                self.reference()?,
                &self.tool,
                stamp,
                &args,
            )
            .await?;
            self.selection = Some(bound);
            self.final_arguments = Some(args);
        }
        self.check_identity(home)?;
        self.check_mapping(layer)?;
        self.payload(params)
    }
    pub(crate) async fn validate(
        &self,
        home: &Path,
        args: &Value,
        layer: Option<&crate::mcp_redaction::McpRedactionLayer>,
    ) -> Result<(), String> {
        self.check_identity(home)?;
        self.check_mapping(layer)?;
        if let Some(stamp) = &self.selection {
            if self.final_arguments.as_ref() != Some(args) {
                return Err(changed());
            }
            crate::mcp::google_selection_client::bind(
                home,
                self.agent(),
                self.reference()?,
                &self.tool,
                stamp,
                args,
            )
            .await?;
        }
        self.check_identity(home)?;
        self.check_mapping(layer)
    }
    pub(crate) fn bind(self, arguments: Value) -> AcceptedGoogleCall {
        AcceptedGoogleCall {
            receipt: self.receipt,
            tool: self.tool,
            arguments,
            selection: self.selection,
        }
    }
}
impl AcceptedGoogleCall {
    pub(crate) fn principal(&self) -> &Principal {
        self.receipt.principal()
    }
    pub(crate) fn credential_ref(&self) -> Option<&gi::CredentialRef> {
        self.receipt.credential_ref()
    }
    pub(crate) fn selection_stamp(&self) -> Option<&GoogleSelectionStamp> {
        self.selection.as_ref()
    }
    pub(crate) fn matches(&self, tool: &str, args: &Value) -> bool {
        self.tool == tool && self.arguments == *args
    }
}
tokio::task_local! {static ACCEPTED_GOOGLE:Option<AcceptedGoogleCall>;}
pub(crate) fn with_accepted_google_call<F: std::future::Future>(
    call: Option<AcceptedGoogleCall>,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    Box::pin(ACCEPTED_GOOGLE.scope(call, Box::pin(future)))
}
pub(crate) fn current_accepted_google_call() -> Option<AcceptedGoogleCall> {
    ACCEPTED_GOOGLE.try_with(Clone::clone).ok().flatten()
}

tokio::task_local! {static GOOGLE_ACCOUNT_APPROVAL:Option<gi::GoogleAccountApproval>;}
pub(crate) fn with_google_account_approval<F: std::future::Future>(
    account: Option<gi::GoogleAccountApproval>,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    GOOGLE_ACCOUNT_APPROVAL.scope(account, Box::pin(future))
}
pub(crate) fn current_google_account_approval() -> Option<gi::GoogleAccountApproval> {
    GOOGLE_ACCOUNT_APPROVAL
        .try_with(Clone::clone)
        .ok()
        .flatten()
}
