//! Home SA lexical owner; raw key, assertion and access token stay here.
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/google_identity/assertion_material.inc.rs"
));
pub(crate) async fn resolve(
    home: &Path,
    config: &crate::google_service_account::ServiceAccountConfig,
    deadline: tokio::time::Instant,
    permit: &crate::google_identity::effect_authority::GooglePreflightPermit<'_>,
) -> Result<Resolved, crate::google_identity::effect_authority::CredentialResolveFailure> {
    let _resolution_lease = credential_lease(home, deadline, permit).await?;
    check_deadline(deadline)
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    permit
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    let (source, bytes) = FileProof::capture(&config.key_file, 16 * 1024)
        .map_err(|_| Refusal::soft("account_unavailable", "公司服務帳號金鑰無法讀取。"))?;
    let key = serde_json::from_slice::<crate::google_service_account::ServiceAccountKey>(&bytes)
        .map_err(|_| Refusal::soft("account_unavailable", "公司服務帳號金鑰內容錯誤。"))?;
    if key.client_email.trim().is_empty() || key.private_key.trim().is_empty() {
        return Err(unavailable().into());
    }
    drop(bytes);
    check_deadline(deadline)
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    permit
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    mint(key, &config.subject, source, home, deadline, permit).await
}
fn validate_permit(permit: &AssertionEffectPermit<'_>, actual: &Resolved) -> Result<(), Refusal> {
    permit.validate_home_sa(actual)
}
fn validate_native(
    permit: &AssertionEffectPermit<'_>,
    actual: &Resolved,
    template: &NativeCallTemplate,
) -> Result<(), Refusal> {
    validate_permit(permit, actual)?;
    permit.validate_native(template)
}
fn validate_mcp(
    permit: &AssertionEffectPermit<'_>,
    actual: &Resolved,
    bound: &RemoteRequestBound,
) -> Result<(), Refusal> {
    validate_permit(permit, actual)?;
    permit.validate_mcp(bound)
}

fn source_notice(material: &Resolved) -> String {
    format!(
        "本次使用已核准的公司共用 Google 服務帳號連線。宣告主體：{}（未經額外驗證）。",
        material.subject
    )
}
