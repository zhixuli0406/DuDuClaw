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

/// Validate the transport and restrict a pull subscription to the key's project.
pub fn validate_connection(connection: &str, subscription: &str, raw_key: &str) -> Result<(), String> {
    match connection {
        "http" if subscription.is_empty() => Ok(()),
        "http" => Err("HTTP 連線不使用 Pub/Sub 訂閱，請清空訂閱名稱。".into()),
        "pubsub" => {
            expected_topic(raw_key, subscription).map(|_| ())
        }
        _ => Err("Google Chat 連線類型必須是 http 或 pubsub。".into()),
    }
}

/// Bind the declared project to the service-account identity, not just to an
/// editable `project_id` field in the downloaded JSON.
fn expected_topic(raw_key: &str, subscription: &str) -> Result<String, String> {
    let key: serde_json::Value = serde_json::from_str(raw_key)
        .map_err(|_| "服務帳戶金鑰不是有效 JSON".to_string())?;
    let project = key.get("project_id").and_then(|v| v.as_str()).unwrap_or("");
    validate_subscription(subscription, project)?;
    let email = key.get("client_email").and_then(|v| v.as_str()).unwrap_or("");
    let Some((name, domain)) = email.split_once('@') else {
        return Err("服務帳戶 email 的專案必須與 project_id 及 Pub/Sub 訂閱的專案相同。".into());
    };
    if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        || domain != format!("{project}.iam.gserviceaccount.com") {
        return Err("服務帳戶 email 的專案必須與 project_id 及 Pub/Sub 訂閱的專案相同。".into());
    }
    Ok(format!("projects/{project}/topics/duduclaw-chat-events"))
}

/// Verify the server-side subscription binding before import and before a
/// worker starts. The production endpoint is fixed; only tests can override it.
pub(crate) async fn verify_subscription(
    creds: &crate::googlechat::GoogleChatCreds,
    raw_key: &str,
    subscription: &str,
) -> Result<(), String> {
    let expected = expected_topic(raw_key, subscription)?;
    if client_email(raw_key).as_deref() != Some(creds.client_email()) {
        return Err("Pub/Sub 驗證使用的服務帳戶與設定不符。".into());
    }
    let base = "https://pubsub.googleapis.com".to_string();
    #[cfg(test)]
    let base = crate::googlechat::test_routes::api(creds.client_email()).unwrap_or(base);
    let token = creds.pubsub_token().await
        .map_err(|_| "無法取得 Pub/Sub 驗證權杖，請檢查服務帳戶金鑰。".to_string())?;
    let mut response = creds.http.get(format!("{base}/v1/{subscription}"))
        .bearer_auth(token).send().await
        .map_err(|_| "無法讀取 Pub/Sub 訂閱，請檢查網路及服務帳戶權限。".to_string())?;
    if !response.status().is_success() {
        return Err(format!("無法驗證 Pub/Sub 訂閱（{}），請確認服務帳戶具有 pubsub.subscriptions.get 權限。", response.status()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "無法讀取 Pub/Sub 訂閱回應。")? {
        if bytes.len() + chunk.len() > 64 * 1024 {
            return Err("Pub/Sub 訂閱回應超過大小限制。".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let body: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| "Pub/Sub 訂閱回應格式不正確。")?;
    if body.get("name").and_then(|v| v.as_str()) != Some(subscription) {
        return Err("Pub/Sub 回傳的訂閱名稱不符，已拒絕啟動。".into());
    }
    if body.get("topic").and_then(|v| v.as_str()) != Some(expected.as_str()) {
        return Err("Pub/Sub 訂閱的主題不符：必須連到服務帳戶同一專案的 duduclaw-chat-events 主題，已拒絕啟動。".into());
    }
    match body.get("enableExactlyOnceDelivery") {
        None | Some(serde_json::Value::Bool(false)) => {},
        Some(serde_json::Value::Bool(true)) => return Err("目前不支援 Pub/Sub 恰好一次投遞（exactly-once）訂閱，請先在 Google Cloud 關閉此選項再匯入或啟動。".into()),
        Some(_) => return Err("Pub/Sub 恰好一次投遞設定格式不正確，已拒絕啟動。".into()),
    }
    Ok(())
}

pub fn validate_subscription(subscription: &str, project: &str) -> Result<(), String> {
    let parts: Vec<_> = subscription.split('/').collect();
    let project_ok = (6..=30).contains(&project.len())
        && project.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && project.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
        && project.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
    if parts.len() != 4 || parts[0] != "projects" || parts[2] != "subscriptions"
        || !project_ok || parts[1] != project {
        return Err("訂閱必須是 projects/<服務帳戶的 project_id>/subscriptions/<名稱>。".into());
    }
    let name = parts[3];
    if !(3..=255).contains(&name.len())
        || !name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        || name.to_ascii_lowercase().starts_with("goog")
        || !name.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.~+%".contains(&c))
        // Encoded separators / URL escapes are not accepted in REST paths.
        || name.bytes().any(|c| c == b'%') {
        return Err("Pub/Sub 訂閱名稱格式不正確。".into());
    }
    Ok(())
}

#[cfg(test)]
mod googlechat_transport_tests {
    use super::*;
    #[test]
    fn token_endpoint_rejects_credentials_paths_and_query() {
        for uri in ["https://user@oauth2.googleapis.com/token", "https://oauth2.googleapis.com/other", "https://oauth2.googleapis.com/token?redirect=evil", "https://oauth2.googleapis.com/token#fragment"] {
            assert!(!token_uri_allowed(uri), "{uri}");
        }
    }
    #[test]
    fn unknown_settings_still_fail_closed() {
        assert!(validate_fields(["connection", "pubsub_subscription"], &["connection", "pubsub_subscription"]).is_ok());
        assert!(validate_fields(["pubsub_subscriptio"], &["connection", "pubsub_subscription"]).unwrap_err().contains("不支援的欄位"));
        assert!(validate_connection("http", "projects/my-project/subscriptions/chat-events", "{}").is_err());
    }
    #[test]
    fn subscription_and_project_are_strict() {
        assert!(validate_subscription("projects/my-project/subscriptions/chat-events", "my-project").is_ok());
        for s in ["projects/other-project/subscriptions/chat-events", "projects/my-project/topics/chat-events", "projects/my-project/subscriptions/a%2fb", "projects/my-project/subscriptions/chat?x", "projects/my-project/subscriptions/../x", "projects/my-project/subscriptions/goog-test"] {
            assert!(validate_subscription(s, "my-project").is_err(), "{s}");
        }
        assert!(validate_connection("invalid", "", "{}").is_err());
        assert!(validate_connection("http", "", "{}").is_ok());
        assert!(validate_connection("pubsub", "projects/my-project/subscriptions/chat-events", "{}").is_err());
    }

    #[test]
    fn googlechat_subscription_project_must_match_service_account_identity() {
        let subscription = "projects/my-project/subscriptions/chat-events";
        let key = |email: &str, project: &str| serde_json::json!({"client_email":email,"project_id":project}).to_string();
        assert_eq!(expected_topic(&key("bot@my-project.iam.gserviceaccount.com", "my-project"), subscription).unwrap(), "projects/my-project/topics/duduclaw-chat-events");
        for (email, project) in [
            ("bot@other-project.iam.gserviceaccount.com", "my-project"),
            ("bot@my-project.iam.gserviceaccount.com", "other-project"),
            ("bot@my-project.iam.gserviceaccount.com.evil.test", "my-project"),
            ("bot@@my-project.iam.gserviceaccount.com", "my-project"),
        ] {
            assert!(validate_connection("pubsub", subscription, &key(email, project)).is_err());
        }
    }

    #[tokio::test]
    async fn googlechat_subscription_rejects_exactly_once_and_malformed_modes() {
        use crate::agent_channels::e2e_gchat_tests as fake_google;
        let fake = fake_google::start_fake().await;
        let email = format!("exactly-once-{}@test-project.iam.gserviceaccount.com", uuid::Uuid::new_v4().simple());
        let mut key: serde_json::Value = serde_json::from_str(&fake_google::service_account(&fake, &email)).unwrap();
        key["project_id"] = serde_json::json!("test-project");
        let raw = key.to_string();
        let creds = crate::googlechat::GoogleChatCreds::candidate(&raw).unwrap();
        let subscription = "projects/test-project/subscriptions/chat-events";
        for mode in [None, Some(serde_json::json!(false))] {
            fake_google::set_subscription_exactly_once(&fake, mode);
            verify_subscription(&creds, &raw, subscription).await.unwrap();
        }
        for mode in [serde_json::json!(true), serde_json::Value::Null, serde_json::json!("false"), serde_json::json!({})] {
            fake_google::set_subscription_exactly_once(&fake, Some(mode));
            assert!(verify_subscription(&creds, &raw, subscription).await.unwrap_err().contains("恰好一次投遞"));
        }
    }

    #[tokio::test]
    async fn googlechat_subscription_api_rejects_topic_mismatch() {
        use crate::agent_channels::e2e_gchat_tests as fake_google;
        let fake = fake_google::start_fake().await;
        let email = format!("verify-{}@test-project.iam.gserviceaccount.com", uuid::Uuid::new_v4().simple());
        let mut key: serde_json::Value = serde_json::from_str(&fake_google::service_account(&fake, &email)).unwrap();
        key["project_id"] = serde_json::json!("test-project");
        let raw = key.to_string();
        let creds = crate::googlechat::GoogleChatCreds::candidate(&raw).unwrap();
        let subscription = "projects/test-project/subscriptions/chat-events";
        verify_subscription(&creds, &raw, subscription).await.unwrap();
        for topic in ["projects/test-project/topics/untrusted", "projects/other-project/topics/duduclaw-chat-events", ""] {
            fake_google::set_subscription_topic(&fake, topic);
            assert!(verify_subscription(&creds, &raw, subscription).await.unwrap_err().contains("主題不符"));
        }
    }
}
