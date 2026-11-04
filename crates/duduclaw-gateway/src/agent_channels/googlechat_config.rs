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
    if let Some(uri) = v.get("token_uri").and_then(|u| u.as_str())
        && !token_uri_allowed(uri)
    {
        return Err("服務帳戶金鑰的 token_uri 必須是 https://oauth2.googleapis.com/token，請使用 Google Cloud 下載的原始金鑰檔。".into());
    }
    Ok(ServiceAccountInfo {
        client_email: email.to_string(),
    })
}

/// Where a service-account assertion may be posted: Google's OAuth token
/// endpoint over https. Debug builds also accept a loopback `http://` URL
/// (local fakes in tests).
pub fn token_uri_allowed(uri: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(uri.trim()) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() || url.path() != "/token" {
        return false;
    }
    if url.scheme() == "https" && url.host_str() == Some("oauth2.googleapis.com") && url.port().is_none() {
        return true;
    }
    #[cfg(debug_assertions)]
    if url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")) {
        return true;
    }
    false
}

/// `client_email` of an already-validated key (for status display).
pub fn client_email(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    v.get("client_email")?.as_str().map(str::to_string)
}

/// Refuse unknown channel settings rather than silently accepting them.
pub fn validate_fields<'a>(fields: impl IntoIterator<Item = &'a str>, allowed: &[&str]) -> Result<(), String> {
    for field in fields {
        if !allowed.contains(&field) {
            return Err(format!("Google Chat 設定包含不支援的欄位「{field}」，請移除後再試。"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod googlechat_config_tests {
    use super::*;
    #[test]
    fn token_endpoint_rejects_credentials_paths_and_query() {
        for uri in ["https://user@oauth2.googleapis.com/token", "https://oauth2.googleapis.com/other", "https://oauth2.googleapis.com/token?redirect=evil", "https://oauth2.googleapis.com/token#fragment"] {
            assert!(!token_uri_allowed(uri), "{uri}");
        }
    }
    #[test]
    fn agent_config_rejects_unknown_settings_in_chinese() {
        let error = toml::from_str::<duduclaw_core::types::GoogleChatChannelConfig>("project_number = \"123456789\"\nconnection = \"http\"\n").unwrap_err();
        assert!(error.to_string().contains("不支援的欄位「connection」"));
        assert!(toml::from_str::<duduclaw_core::types::GoogleChatChannelConfig>("project_number = \"123456789\"").is_ok());
    }
    #[test]
    fn unknown_connection_is_refused_with_actionable_error() {
        let error = validate_fields(["project_number", "connection"], &["project_number"]).unwrap_err();
        assert!(error.contains("不支援的欄位「connection」"));
    }
}
