//! Delegate lexical owner: unsealing happens inline before any provider await.
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/google_identity/assertion_material.inc.rs"
));
pub(crate) async fn resolve(
    home: &Path,
    email: &str,
    deadline: tokio::time::Instant,
    permit: &crate::google_identity::effect_authority::GooglePreflightPermit<'_>,
) -> Result<Resolved, crate::google_identity::effect_authority::CredentialResolveFailure> {
    let _resolution_lease = credential_lease(home, deadline, permit).await?;
    check_deadline(deadline)
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    permit
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    // Preserve the existing protection-before-missing-record order. The OS
    // call is finite but not preemptible; no detached worker is created.
    let protected = crate::google_identity::protection_gate(home)?;
    check_deadline(deadline)
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    permit
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    let path = super::path(home);
    if !path.exists() {
        return Err(Refusal::soft("delegate_unavailable", "公司授權代表尚未匯入金鑰。").into());
    }
    let (source, bytes) = FileProof::capture(&path, 64 * 1024)
        .map_err(|_| Refusal::hard("credential_protection", "封存金鑰無法讀取。"))?;
    let record: super::Record = serde_json::from_slice(&bytes)
        .map_err(|_| Refusal::hard("credential_protection", "封存金鑰紀錄損壞。"))?;
    if record.version != 1 {
        return Err(Refusal::hard("credential_protection", "封存金鑰版本錯誤。").into());
    }
    let plain = protected.unseal(&record.sealed).map_err(|error| {
        if error.code == "key_changed" {
            super::report_key_replaced(home, &record);
        }
        Refusal::hard("credential_protection", "封存金鑰無法解開，請重新匯入。")
    })?;
    drop(protected);
    check_deadline(deadline)
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    permit
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    source
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    let key = serde_json::from_slice::<crate::google_service_account::ServiceAccountKey>(&plain)
        .map_err(|_| Refusal::hard("credential_protection", "封存金鑰內容錯誤。"))?;
    if key.client_email.trim() != record.client_email
        || key.client_id.clone().filter(|id| !id.trim().is_empty()) != record.client_id
    {
        return Err(Refusal::hard("credential_protection", "封存金鑰與匯入資料不符。").into());
    }
    drop(record);
    drop(plain);
    drop(bytes);
    check_deadline(deadline)
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    permit
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    mint(key, email, source, home, deadline, permit).await
}
fn validate_permit(permit: &AssertionEffectPermit<'_>, actual: &Resolved) -> Result<(), Refusal> {
    permit.validate_delegate(actual)
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
        "本次使用已核准的公司 Google 授權代表連線。宣告主體：{}（未經額外驗證）。",
        material.subject
    )
}
