pub(in crate::google_identity) async fn execute_immediate_native(
    home: &Path,
    users: Option<&std::sync::Arc<duduclaw_auth::UserDb>>,
    agent: &str,
    reference: &CredentialRef,
    tool: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, Refusal> {
    execute_immediate_native_pinned(home, users, agent, reference, tool, args, None, None).await
}

/// [`execute_immediate_native`] bound to the tenant source the isolation
/// bridge pinned (SR4 item 4): the host lane never decides on a config it
/// re-read after the bridge decided; a different source digest refuses.
pub(in crate::google_identity) async fn execute_immediate_native_pinned(
    home: &Path,
    users: Option<&std::sync::Arc<duduclaw_auth::UserDb>>,
    agent: &str,
    reference: &CredentialRef,
    tool: &str,
    args: serde_json::Value,
    tenant: Option<TenantPin>,
    expected_owner: Option<super::owner_snapshot::OwnerSnapshot>,
) -> Result<serde_json::Value, Refusal> {
    crate::google_workspace::validate_native_tool_inputs(tool, &args)
        .map_err(|message| Refusal::hard("native_arguments_invalid", message))?;
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
    )?
    .pin_tenant(tenant)?;
    if let Some(expected) = expected_owner {
        if context.owner_snapshot.as_ref() != Some(&expected) { return Err(changed()); }
    }
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

