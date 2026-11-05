//! Download-only path for the root-managed service. Never executes an asset.
use super::*;

fn release_url(version: &str, suffix: &str) -> Result<String, String> {
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()
        || !part.bytes().all(|b| b.is_ascii_digit()) || part.parse::<u64>().is_err()
        || (part.len() > 1 && part.starts_with('0')))
        || !matches!(suffix, "linux-x64.tar.gz" | "linux-arm64.tar.gz") {
        return Err("Protected upgrade requires X.Y.Z and a supported Linux architecture".into());
    }
    Ok(format!("https://github.com/{GITHUB_REPO}/releases/download/v{version}/duduclaw-{suffix}"))
}

fn allowed_redirect(url: &reqwest::Url) -> bool {
    url.scheme() == "https" && url.username().is_empty() && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && match url.host_str() {
            Some("objects.githubusercontent.com" | "release-assets.githubusercontent.com") => true,
            Some("github.com") => url.path().starts_with(&format!("/{GITHUB_REPO}/")),
            _ => false,
        }
}

/// Fetch and authenticate the exact release archive using the existing updater
/// key and integrity verifier, then stream the executable into the caller's
/// root-only staging file, hashing and counting bytes without executing them.
/// The staging file must be opened read/write for static version inspection.
/// Returns the version, compressed size and executable size after fsync.
/// The operator CLI owns the staging file and atomic destination swap.
pub async fn download_protected_release(version: Option<&str>, output: &mut std::fs::File) -> Result<(String, u64, u64), String> {
    let version = match version {
        Some(version) => version.to_owned(),
        None => check_update().await?.latest_version,
    };
    duduclaw_core::root_key::check_protected_version(&version, current_version(), None, true)?;
    let url = release_url(&version, platform_asset_suffix())?;
    let client = reqwest::Client::builder()
        .user_agent(format!("duduclaw/{}", current_version()))
        .no_proxy()
        .timeout(std::time::Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 5 && allowed_redirect(attempt.url()) {
                attempt.follow()
            } else { attempt.error("Protected upgrade redirect refused") }
        }))
        .build().map_err(|e| format!("HTTP client error: {e}"))?;
    let checksum = format!("{url}.sha256");
    let signature = format!("{url}.minisig");
    let (archive, identity) = with_retries(&DOWNLOAD_RETRY_DELAYS_SECS, &|_, _| {}, |_| {
        download_and_verify_once(&client, &url, &checksum, &signature)
    }).await.map_err(|e| e.message)?;
    identity.check_archive(&archive)?;
    let size = archive.len() as u64;
    eprintln!("Release archive bytes: {size}");
    let binary_size = extract_binary_to(&archive, "duduclaw", output)?;
    if binary_size == 0 { return Err("Release binary is empty".into()); }
    release_identity::verify_binary_version(output, identity.version())?;
    output.sync_all().map_err(|e| format!("Failed to sync staged binary: {e}"))?;
    eprintln!("Release binary bytes: {binary_size}");
    Ok((version, size, binary_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_release_urls_are_fixed_and_version_is_strict() {
        assert_eq!(release_url("1.79.3", "linux-x64.tar.gz").unwrap(),
            "https://github.com/zhixuli0406/DuDuClaw/releases/download/v1.79.3/duduclaw-linux-x64.tar.gz");
        assert!(release_url("1.79.3", "linux-arm64.tar.gz").is_ok());
        for version in ["", "latest", "v1.79.3", "01.79.3", "1.2", "1.2.3/evil", "1.2.3?x", "1.2.3\n", "1.2.3-beta"] {
            assert!(release_url(version, "linux-x64.tar.gz").is_err(), "{version:?}");
        }
        assert!(release_url("1.2.3", "unknown").is_err());
    }

    #[test]
    fn protected_release_redirects_use_exact_hosts_and_repo_boundary() {
        for url in ["https://github.com/zhixuli0406/DuDuClaw/releases/download/v1.2.3/asset",
            "https://release-assets.githubusercontent.com/asset", "https://objects.githubusercontent.com/asset"] {
            assert!(allowed_redirect(&url.parse().unwrap()));
        }
        for url in ["http://github.com/zhixuli0406/DuDuClaw/asset", "https://github.com/zhixuli0406/DuDuClaw-evil/asset",
            "https://github.com.evil.com/zhixuli0406/DuDuClaw/asset", "https://evil.com/asset",
            "https://github.com:444/zhixuli0406/DuDuClaw/asset", "https://user@github.com/zhixuli0406/DuDuClaw/asset"] {
            assert!(!allowed_redirect(&url.parse().unwrap()), "{url}");
        }
    }
}
