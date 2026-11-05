//! Keep the OAuth response out of generic JSON values and wipe owned buffers.
use serde::{Deserialize, Deserializer};
use zeroize::{Zeroize, Zeroizing};
use super::{rpc_error, TranscriptionError};

const RESPONSE_LIMIT: usize = 64 * 1024;
const TOKEN_LIMIT: usize = 16 * 1024;

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(deserialize_with = "secret_string")]
    access_token: Zeroizing<String>,
}

fn secret_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Zeroizing<String>, D::Error> {
    String::deserialize(deserializer).map(Zeroizing::new)
}

fn invalid() -> TranscriptionError {
    TranscriptionError::new("Google Cloud 未回傳有效的存取憑證。")
}

fn parse(data: &[u8]) -> Result<Zeroizing<String>, TranscriptionError> {
    let response: TokenResponse = serde_json::from_slice(data).map_err(|_| invalid())?;
    if response.access_token.is_empty() || response.access_token.len() > TOKEN_LIMIT { return Err(invalid()); }
    Ok(response.access_token)
}

pub(super) async fn read_response(mut response: reqwest::Response) -> Result<Zeroizing<String>, TranscriptionError> {
    match response.status().as_u16() {
        401 | 403 => return Err(rpc_error(7)),
        429 => return Err(rpc_error(8)),
        _ if !response.status().is_success() => return Err(invalid()),
        _ => {},
    }
    if response.content_length().is_some_and(|len| len > RESPONSE_LIMIT as u64) { return Err(invalid()); }
    // Reserve the entire bound so growth never leaves an old token allocation.
    let mut data = Zeroizing::new(Vec::with_capacity(RESPONSE_LIMIT));
    while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
        let fits = data.len().saturating_add(chunk.len()) <= RESPONSE_LIMIT;
        // reqwest may share transport buffers; wipe a chunk only when it can
        // transfer exclusive ownership. Shared HTTP/TLS buffers are not ours.
        match chunk.try_into_mut() {
            Ok(mut owned) => {
                if fits { data.extend_from_slice(&owned); }
                owned.as_mut().zeroize();
            },
            Err(shared) => { if fits { data.extend_from_slice(&shared); } },
        }
        if !fits { return Err(invalid()); }
    }
    parse(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcription_google_token_typed_response_ignores_extra_fields() {
        let raw = Zeroizing::new(br#"{"access_token":"token-canary","expires_in":3600,"extra":{"secret":"ignored"}}"#.to_vec());
        let token: Zeroizing<String> = parse(&raw).unwrap();
        assert_eq!(token.as_str(), "token-canary");
    }

    #[test]
    fn transcription_google_token_invalid_responses_are_sanitized() {
        for raw in [r#"{}"#, r#"{"access_token":null}"#, r#"{"access_token":123}"#,
            r#"{"access_token":""}"#, r#"{"access_token":"token-canary","access_token":"duplicate"}"#,
            r#"{"access_token":"token-canary", invalid}"#] {
            let error = parse(raw.as_bytes()).unwrap_err().to_string();
            assert!(!error.contains("token-canary"));
        }
        let raw = Zeroizing::new(format!(r#"{{"access_token":"{}"}}"#, "x".repeat(TOKEN_LIMIT + 1)));
        assert!(parse(raw.as_bytes()).is_err());
    }
}
