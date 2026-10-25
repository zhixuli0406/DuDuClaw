//! Shared classification for OAuth routes that must not downgrade Google data
//! access into a generic provider flow.

pub(crate) fn is_google_oauth_route(
    provider_id: &str,
    issuer: &str,
    authorization_endpoint: &str,
    token_endpoint: &str,
    scopes: impl IntoIterator<Item = impl AsRef<str>>,
) -> bool {
    if provider_id.eq_ignore_ascii_case("google") {
        return true;
    }
    let google_host = |raw: &str| {
        url::Url::parse(raw)
            .ok()
            .and_then(|url| {
                url.host_str()
                    .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
            })
            .is_some_and(|host| {
                host == "google.com"
                    || host.ends_with(".google.com")
                    || host == "googleapis.com"
                    || host.ends_with(".googleapis.com")
            })
    };
    if [issuer, authorization_endpoint, token_endpoint]
        .into_iter()
        .any(google_host)
    {
        return true;
    }
    scopes.into_iter().any(|scope| {
        let scope = scope.as_ref().trim().to_ascii_lowercase();
        scope.starts_with("https://www.googleapis.com/auth/")
            || scope.starts_with("https://mail.google.com/")
            || scope.starts_with("https://www.google.com/m8/")
            || scope.starts_with("https://sites.google.com/feeds/")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_google_aliases_by_endpoint_or_workspace_scope() {
        assert!(is_google_oauth_route(
            "renamed",
            "https://accounts.google.com",
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
            ["openid"],
        ));
        assert!(is_google_oauth_route(
            "renamed",
            "https://auth.example.test",
            "https://auth.example.test/authorize",
            "https://auth.example.test/token",
            ["https://www.googleapis.com/auth/gmail.readonly"],
        ));
        assert!(is_google_oauth_route(
            "renamed",
            "",
            "https://accounts.google.com./o/oauth2/v2/auth",
            "https://oauth2.googleapis.com./token",
            ["openid"],
        ));
        assert!(!is_google_oauth_route(
            "renamed",
            "",
            "https://google.com.evil.example/authorize",
            "https://example.test/token",
            ["openid"],
        ));
        assert!(!is_google_oauth_route(
            "notion",
            "https://api.notion.com",
            "https://api.notion.com/v1/oauth/authorize",
            "https://api.notion.com/v1/oauth/token",
            ["read_content"],
        ));
    }
}
