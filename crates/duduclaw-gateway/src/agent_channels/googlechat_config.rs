//! Validation of an employee's Google Chat app settings at the save boundary.

/// The parts of a service-account key the channel uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAccountInfo {
    pub client_email: String,
}

/// A Cloud project number is the decimal number Google signs the Chat app's
/// requests for (the JWT audience). Returns the trimmed value or a zh-TW
/// refusal.
pub fn validate_project_number(raw: &str) -> Result<String, String> {
    let p = raw.trim();
    if (6..=20).contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit()) {
        Ok(p.to_string())
    } else {
        Err("專案編號（Project number）應該是一串數字，可在 Google Cloud 主控台的專案資訊卡找到；請不要填專案 ID（英文名稱）。".into())
    }
}

/// Check a pasted service-account JSON key: it must be a service-account key
/// with `client_email` and an RSA `private_key` the token grant can sign
/// with. Nothing is sent anywhere.
pub fn validate_service_account_json(raw: &str) -> Result<ServiceAccountInfo, String> {
    let bad =
        || "服務帳戶金鑰格式不正確：請貼上從 Google Cloud 下載的整份 JSON 金鑰檔內容。".to_string();
    let v: serde_json::Value = serde_json::from_str(raw.trim()).map_err(|_| bad())?;
    if v.get("type")
        .and_then(|t| t.as_str())
        .is_some_and(|t| t != "service_account")
    {
        return Err(bad());
    }
    let email = v
        .get("client_email")
        .and_then(|e| e.as_str())
        .map(str::trim)
        .filter(|e| e.contains('@'))
        .ok_or_else(bad)?;
    let key = v
        .get("private_key")
        .and_then(|k| k.as_str())
        .ok_or_else(bad)?;
    jsonwebtoken::EncodingKey::from_rsa_pem(key.as_bytes()).map_err(|_| bad())?;
    Ok(ServiceAccountInfo {
        client_email: email.to_string(),
    })
}

/// `client_email` of an already-validated key (for status display).
pub fn client_email(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    v.get("client_email")?.as_str().map(str::to_string)
}
