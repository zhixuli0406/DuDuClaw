//! Signed metadata routes delegate to the E1 owner's private original-union
//! store. This module cannot construct an original selection or claimed call.
use super::ExecuteState;
use crate::google_identity::Refusal;
pub(super) use crate::google_identity::effect_authority::{Clock, SelectionStore};
#[cfg(test)]
use crate::google_identity::effect_authority::{MAX_SELECTIONS_PER_AGENT, MAX_TOTAL_BYTES};
#[cfg(test)]
use crate::google_personal::authority::PersonalSelection;
use axum::{
    Json,
    body::Bytes,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use duduclaw_core::google_identity::{
    self as gi, CredentialRef, GoogleSelectionRequest, GoogleSelectionStamp,
};
#[cfg(test)]
use duduclaw_core::google_identity::{
    GOOGLE_SELECTION_ROUTE, GOOGLE_SELECTION_TTL_SECS, google_arguments_digest,
};
#[cfg(test)]
use duduclaw_knowledge::requester::RequesterIdentity;
use serde_json::{Value, json};
#[cfg(test)]
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::time::Duration;
use std::{net::SocketAddr, sync::Arc};
fn refused() -> Refusal {
    Refusal::hard(
        "identity_changed",
        "Google 帳號選擇已失效或授權已變更，請重新選擇帳號並核准。",
    )
}
#[cfg(test)]
fn live_key(
    state: &ExecuteState,
    agent: &str,
    reference: &CredentialRef,
    tool: &str,
) -> Result<(), Refusal> {
    crate::google_identity::effect_authority::host_live_key(&state.home, agent, reference, tool)
}
#[cfg(test)]
fn select_account(
    state: &ExecuteState,
    agent: &str,
    reference: &CredentialRef,
    tool: &str,
    args: &Value,
    clock: Clock,
) -> Result<Option<GoogleSelectionStamp>, Refusal> {
    state.selections.select_dedicated(
        &state.home,
        state.users.as_ref(),
        agent,
        reference,
        tool,
        args,
        clock,
    )
}
#[allow(clippy::too_many_arguments)]
fn bind_account(
    state: &ExecuteState,
    agent: &str,
    reference: &CredentialRef,
    tool: &str,
    id: &str,
    original_digest: &str,
    approval_digest: &str,
    args: &Value,
    clock: Clock,
) -> Result<GoogleSelectionStamp, Refusal> {
    state.selections.bind_dedicated(
        &state.home,
        state.users.as_ref(),
        agent,
        reference,
        tool,
        id,
        original_digest,
        approval_digest,
        args,
        clock,
    )
}
pub(super) fn claim_selection(
    state: &ExecuteState,
    agent: &str,
    reference: &CredentialRef,
    tool: &str,
    id: &str,
    args: &Value,
    clock: Clock,
) -> Result<crate::google_identity::effect_authority::PendingGoogleCall, Refusal> {
    state.selections.claim_dedicated(
        &state.home,
        state.users.as_ref(),
        agent,
        reference,
        tool,
        id,
        args,
        clock,
    )
}

pub(super) async fn handle(
    State(state): State<Arc<ExecuteState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<Bytes, axum::extract::rejection::BytesRejection>,
) -> Response {
    use crate::computer_use_sessions::auth::{authenticate, check_peer};
    if check_peer(peer).is_err() {
        return super::unauthorized();
    }
    let Ok(body) = body else {
        return super::refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "bad_request",
            "Google 帳號選擇的請求過大。",
        );
    };
    let clock = Clock::now();
    let caller = match authenticate(&state.home, peer, &headers, &body, clock.unix) {
        Ok(c) => c,
        Err(_) => return super::unauthorized(),
    };
    if !state.rate.allow(&caller.agent_id, clock.mono) {
        return super::refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Google 請求過於頻繁，請稍後再試。",
        );
    }
    if !state.replay.record(&caller.nonce, clock.mono) {
        return super::unauthorized();
    }
    let request: GoogleSelectionRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => {
            return super::refuse(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "Google 帳號選擇的請求格式不正確。",
            );
        }
    };
    if gi::native_google_scope(request.tool()).is_none()
        || !request.arguments().is_object()
        || !request.credential_ref().is_well_formed()
    {
        return super::refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "Google 帳號選擇的工具或參數不正確。",
        );
    }
    // Capture/credential resolution is owned by this request future. Only
    // the bounded final map commit is synchronous; no blocking worker owns it.
    let result = match request {
        GoogleSelectionRequest::SelectGoogleAccount {
            tool,
            arguments,
            credential_ref,
        } => {
            state
                .selections
                .select_dedicated_async(
                    &state.home,
                    state.users.as_ref(),
                    &caller.agent_id,
                    &credential_ref,
                    &tool,
                    &arguments,
                    Clock::now(),
                )
                .await
        }
        GoogleSelectionRequest::BindGoogleArguments {
            opaque_id,
            tool,
            credential_ref,
            original_arguments_digest,
            account_binding_digest,
            arguments,
        } => bind_account(
            &state,
            &caller.agent_id,
            &credential_ref,
            &tool,
            &opaque_id,
            &original_arguments_digest,
            &account_binding_digest,
            &arguments,
            Clock::now(),
        )
        .map(Some),
    };
    match result {
        Ok(selection) => Json(json!({"ok":true,"selection":selection})).into_response(),
        Err(error) => {
            let status = if error.code == "selection_capacity" {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::FORBIDDEN
            };
            super::refuse(status, error.code, &error.message_zh)
        }
    }
}
#[cfg(test)]
#[path = "account_selection_tests.rs"]
mod tests;
