//! Private one-attempt refresh transport over the original SourceOwner capture.
use super::*;
use std::time::Duration;
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const MAX_BODY: usize = 4 * 1024 * 1024;
pub(super) const MAX_TOKEN: usize = 1024 * 1024;

struct ResponseTokens {
    access: Zeroizing<String>,
    refresh: Option<Zeroizing<String>>,
    expires_at: chrono::DateTime<chrono::Utc>,
    scopes: Vec<String>,
}
pub(super) struct Refreshed {
    pub(super) access: Zeroizing<String>,
    pub(super) payload: home_store::PreparedGooglePayload,
}
pub(super) fn bearer(value: &str) -> bool {
    if value.len() > MAX_TOKEN {
        return false;
    }
    let mut split = value.splitn(2, '=');
    let prefix = split.next().unwrap_or("");
    !prefix.is_empty()
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
        && split.next().is_none_or(|s| s.bytes().all(|b| b == b'='))
}
struct JsonBody(Value);
impl Drop for JsonBody {
    fn drop(&mut self) {
        scrub(&mut self.0);
    }
}
fn parse_tokens(body: Value, captured_scopes: &[String], now: u64) -> Result<ResponseTokens> {
    let body = JsonBody(body);
    let value = &body.0;
    let access = value["access_token"]
        .as_str()
        .filter(|s| bearer(s))
        .ok_or(Error::Invalid)?;
    if !value["token_type"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case("Bearer"))
    {
        return Err(Error::Invalid);
    }
    let refresh = match value.get("refresh_token") {
        None => None,
        Some(Value::String(s))
            if !s.is_empty() && s.len() <= MAX_TOKEN && s.bytes().all(|b| b.is_ascii_graphic()) =>
        {
            Some(Zeroizing::new(s.to_owned()))
        }
        _ => return Err(Error::Invalid),
    };
    let seconds = value["expires_in"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 366 * 24 * 60 * 60)
        .ok_or(Error::Invalid)?;
    let expires_at = i64::try_from(now)
        .ok()
        .and_then(|n| chrono::DateTime::from_timestamp(n, 0))
        .and_then(|n| n.checked_add_signed(chrono::Duration::seconds(seconds as i64)))
        .ok_or(Error::Invalid)?;
    let scopes = match value.get("scope") {
        None => home_store::normalized_scopes(captured_scopes.to_vec())?,
        Some(Value::String(scope)) if !scope.is_empty() && scope.len() <= 128 * 2049 => {
            let pieces: Vec<_> = scope.split(' ').map(str::to_owned).collect();
            if pieces.len() > 128
                || pieces.iter().any(|s| {
                    s.is_empty()
                        || s.bytes().any(|b| {
                            !(b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b))
                        })
                })
            {
                return Err(Error::Invalid);
            }
            home_store::normalized_scopes(pieces)?
        }
        _ => return Err(Error::Invalid),
    };
    // Actual changed scopes are publishable. finish_refresh owns epoch/gen
    // rotation; the old resolver refuses material AFTER confirmed publication.
    Ok(ResponseTokens {
        access: Zeroizing::new(access.to_owned()),
        refresh,
        expires_at,
        scopes,
    })
}
fn client(budget: ResolveBudget<'_>) -> Result<reqwest::Client> {
    let builder = match budget {
        ResolveBudget::Legacy => reqwest::Client::builder(),
        ResolveBudget::Remote(..) | ResolveBudget::Preflight(_, _) => {
            reqwest::Client::builder().no_proxy()
        }
    };
    builder
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| Error::Io)
}
fn endpoint(_home: &Path) -> String {
    #[cfg(test)]
    if let Some(url) = testing::endpoint(_home) {
        return url;
    }
    TOKEN_ENDPOINT.to_owned()
}
fn endpoint_for_budget(home: &Path, budget: ResolveBudget<'_>) -> Result<String> {
    match budget {
        ResolveBudget::Legacy => Ok(endpoint(home)),
        ResolveBudget::Remote(..) | ResolveBudget::Preflight(_, _) => {
            #[cfg(test)]
            {
                return testing::endpoint(home).ok_or(Error::Invalid);
            }
            #[cfg(not(test))]
            {
                let _ = home;
                Ok(TOKEN_ENDPOINT.to_owned())
            }
        }
    }
}
fn seal(
    home: &Path,
    receipt: &HomeSourceReceipt,
    capture: &HomeOAuthCapture,
    value: &str,
) -> Result<String> {
    validate_capture(home, receipt, capture)?;
    #[cfg(test)]
    {
        home_store::test_probe::crypto(home);
        home_store::test_probe::google_crypto(home);
    }
    let sealed = crate::config_crypto::encrypt_value(value, home)
        .map(|s| format!("enc:v1:{s}"))
        .ok_or(Error::Io);
    #[cfg(test)]
    testing_owner::boundary(home, 8);
    validate_capture(home, receipt, capture)?;
    sealed
}
pub(super) async fn refresh_once(
    home: &Path,
    receipt: &HomeSourceReceipt,
    capture: &HomeOAuthCapture,
) -> Result<Refreshed> {
    refresh_once_budget(home, receipt, capture, ResolveBudget::Legacy).await
}
pub(super) async fn refresh_once_budget(
    home: &Path,
    receipt: &HomeSourceReceipt,
    capture: &HomeOAuthCapture,
    budget: ResolveBudget<'_>,
) -> Result<Refreshed> {
    budget.check()?;
    validate_capture(home, receipt, capture)?;
    // Remote cfg(test) must have its separate owned refresh endpoint before
    // any unseal, provider observation or request. Legacy keeps its policy.
    let endpoint = endpoint_for_budget(home, budget)?;
    let raw = &capture.client.as_ref().ok_or(Error::Invalid)?._raw;
    validate_google_client_raw(raw)?;
    if raw["token_url"] != TOKEN_ENDPOINT {
        return Err(Error::Invalid);
    }
    let client_id = Zeroizing::new(raw["client_id"].as_str().ok_or(Error::Invalid)?.to_owned());
    let refresh = open_captured(
        home,
        receipt,
        capture,
        capture
            .refresh
            .as_deref()
            .map(String::as_str)
            .ok_or(Error::Invalid)?,
        false,
    )?;
    budget.check()?;
    let secret = open_captured(
        home,
        receipt,
        capture,
        raw["client_secret"].as_str().ok_or(Error::Invalid)?,
        true,
    )?;
    budget.check()?;
    let sender = client(budget)?;
    #[cfg(test)]
    testing_owner::boundary(home, 4);
    validate_capture(home, receipt, capture)?;
    #[cfg(test)]
    home_store::test_probe::provider(&client_id);
    budget.check()?;
    #[cfg(test)]
    testing::send_ready(home);
    let setup_stamp = crate::mcp_oauth::google_token_stamp(home);
    let sent = budget
        .wait(
            sender
                .post(endpoint)
                .header(reqwest::header::ACCEPT, "application/json")
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh.as_str()),
                    ("client_id", client_id.as_str()),
                    ("client_secret", secret.as_str()),
                ])
                .send(),
        )
        .await?;
    #[cfg(test)]
    testing_owner::boundary(home, 5);
    budget.check()?;
    validate_capture(home, receipt, capture)?;
    let mut response = sent.map_err(|_| Error::Io)?;
    let success = response.status().is_success();
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err(Error::Invalid);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    loop {
        budget.check()?;
        validate_capture(home, receipt, capture)?;
        let chunk = budget.wait(response.chunk()).await?;
        #[cfg(test)]
        {
            testing_owner::boundary(home, 6);
            if matches!(&chunk, Ok(None)) {
                testing_owner::boundary(home, 12);
            }
            if chunk.is_err() {
                testing_owner::boundary(home, 13);
            }
        }
        // Check even on transport error or EOF before inspecting the result.
        budget.check()?;
        validate_capture(home, receipt, capture)?;
        let Some(chunk) = chunk.map_err(|_| Error::Io)? else {
            break;
        };
        if bytes
            .len()
            .checked_add(chunk.len())
            .is_none_or(|n| n > MAX_BODY)
        {
            return Err(Error::Invalid);
        }
        bytes.extend_from_slice(&chunk);
    }
    if !success {
        let body = JsonBody(serde_json::from_slice(&bytes).unwrap_or(Value::Null));
        if body.0["error"].as_str() == Some("invalid_grant") {
            validate_capture(home, receipt, capture)?;
            crate::mcp_oauth::record_google_invalid_grant(home, &setup_stamp).map_err(|_| Error::Io)?;
        }
        return Err(Error::Io);
    }
    let parsed = parse_tokens(
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?,
        &capture.row.scopes,
        home_store::wall_time_now(home),
    )?;
    #[cfg(test)]
    testing_owner::boundary(home, 7);
    let retained_cipher = if parsed.refresh.is_none() {
        capture
            .refresh
            .as_deref()
            .filter(|s| s.starts_with("enc:v1:"))
    } else {
        None
    };
    let refresh_plain = parsed
        .refresh
        .as_deref()
        .or_else(|| retained_cipher.is_none().then_some(&*refresh));
    budget.check()?;
    with_metadata(home, |g, source| {
        require_receipt(g, source, receipt)?;
        home_store::preflight_refresh_payload(
            g,
            source,
            &capture.original,
            &capture.identity,
            parsed.access.len(),
            refresh_plain.map(String::len),
            retained_cipher.map(String::len),
            parsed.expires_at,
            &parsed.scopes,
        )
    })?;
    budget.check()?;
    let access = seal(home, receipt, capture, &parsed.access)?;
    budget.check()?;
    let refresh_token = match (retained_cipher, refresh_plain) {
        (Some(cipher), _) => Some(cipher.to_string()),
        (None, Some(plain)) => Some(seal(home, receipt, capture, plain)?),
        _ => return Err(Error::Invalid),
    };
    validate_capture(home, receipt, capture)?;
    budget.check()?;
    let payload = home_store::prepare_refresh_payload(crate::mcp_oauth::McpOAuthToken {
        provider_id: "google".into(),
        access_token: access,
        refresh_token,
        expires_at: Some(parsed.expires_at),
        scopes: parsed.scopes,
    })?;
    budget.check()?;
    Ok(Refreshed {
        access: parsed.access,
        payload,
    })
}

#[cfg(test)]
#[path = "oauth_refresh_test_wire.rs"]
pub(super) mod testing;
#[cfg(test)]
#[path = "oauth_refresh_io_tests.rs"]
mod tests;
#[cfg(test)]
use super::testing as testing_owner;
