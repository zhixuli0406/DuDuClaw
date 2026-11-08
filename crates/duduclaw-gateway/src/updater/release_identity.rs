//! Release identity checks, after signature verification and before installation.
use super::*;
use std::io::{Read, Seek, SeekFrom};

// The parser builds the magic at runtime so its own constants cannot become a
// second marker in the release executable (including stripped/LTO builds).
static RELEASE_VERSION: &str = concat!("\0DUDUCLAW_RELEASE_ID:V1:",
    env!("CARGO_PKG_VERSION"), "\0DUDUCLAW_RELEASE_END\0");

/// Proof that a pinned-key signature binds this exact archive to its release.
/// Fields are private; only the verifier below can construct this value.
#[derive(Debug)]
pub struct VerifiedReleaseIdentity {
    version: String,
    file: String,
    trusted_comment: String,
    signature: String,
    archive_sha256: [u8; 32],
}

impl VerifiedReleaseIdentity {
    pub fn version(&self) -> &str { &self.version }
    pub fn file(&self) -> &str { &self.file }
    pub fn trusted_comment(&self) -> &str { &self.trusted_comment }
    pub fn signature(&self) -> &str { &self.signature }

    pub(super) fn check_archive(&self, bytes: &[u8]) -> Result<(), String> {
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        if digest != self.archive_sha256 {
            return Err("Verified release identity belongs to a different archive".into());
        }
        Ok(())
    }
}

/// Providers pass their own binary-pinned key and the expected control-plane
/// version/file, never values inferred solely from the downloaded signature.
pub fn verify_release_archive_with_pubkey(bytes: &[u8], checksum: &str,
    signature: &str, key: &str, version: &str, file: &str) -> Result<VerifiedReleaseIdentity, String> {
    let product = product_for_file(file, &release_suffixes())
        .ok_or("Release file must match this platform and edition")?;
    if !plain_version(version) { return Err("Invalid expected release version".into()); }
    verify_archive_with_pubkey(bytes, checksum, signature, key)?;
    let trusted_comment = format!("{product} v{version} {file}");
    verify_comment(signature, &trusted_comment)?;
    Ok(VerifiedReleaseIdentity {
        version: version.to_owned(), file: file.to_owned(), trusted_comment,
        signature: signature.to_owned(), archive_sha256: Sha256::digest(bytes).into(),
    })
}

/// Asset suffixes this platform accepts: the platform archive everywhere,
/// plus `darwin-<arch>.app.zip` on macOS only (RM-155).
fn release_suffixes() -> Vec<&'static str> {
    std::iter::once(platform_asset_suffix()).chain(bundle_asset_suffix()).collect()
}

/// `duduclaw-<suffix>` → CE, `duduclaw-pro-<suffix>` → Pro, else `None`.
fn product_for_file(file: &str, suffixes: &[&str]) -> Option<&'static str> {
    suffixes.iter().find_map(|suffix| {
        if file == format!("duduclaw-{suffix}") {
            Some("duduclaw")
        } else if file == format!("duduclaw-pro-{suffix}") {
            Some("duduclaw-pro")
        } else {
            None
        }
    })
}

/// Whether a verified release file is the macOS app bundle asset.
pub(super) fn is_bundle_file(file: &str) -> bool {
    file.ends_with(".app.zip")
}

pub(super) fn retain_release_version() {
    // Keep the build identity in stripped/LTO binaries without executing a probe.
    std::hint::black_box(RELEASE_VERSION.as_bytes());
}

fn plain_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3 && parts.iter().all(|part| !part.is_empty()
        && part.bytes().all(|b| b.is_ascii_digit()) && part.parse::<u64>().is_ok()
        && (part.len() == 1 || !part.starts_with('0')))
}

pub(super) fn release_identity(url: &str) -> Result<(String, String), String> {
    let url = reqwest::Url::parse(url).map_err(|_| "Invalid release URL")?;
    let prefix = format!("/{GITHUB_REPO}/releases/download/v");
    let tail = url.path().strip_prefix(&prefix).ok_or("Invalid release URL path")?;
    let (version, file) = tail.split_once('/').ok_or("Missing release version or file name")?;
    // The public channel only ships CE assets: the platform archive, or the
    // `.app.zip` bundle on macOS.
    let expected = release_suffixes().iter().any(|suffix| file == format!("duduclaw-{suffix}"));
    if url.scheme() != "https" || url.host_str() != Some("github.com")
        || !url.username().is_empty() || url.password().is_some()
        || url.port_or_known_default() != Some(443) || url.query().is_some()
        || url.fragment().is_some() || !plain_version(version) || !expected {
        return Err("Release URL must name an exact stable version and this platform's archive".into());
    }
    Ok((version.to_owned(), file.to_owned()))
}

#[cfg(test)]
pub(super) fn verify_release_comment(data: &[u8], sig_text: &str, key: &str,
    version: &str, file: &str) -> Result<(), String> {
    // PublicKey::verify checks BOTH the payload and global signature over the comment.
    verify_minisign_signature_with_pubkey(data, sig_text, key)?;
    verify_comment(sig_text, &format!("duduclaw v{version} {file}"))
}

fn verify_comment(sig_text: &str, expected: &str) -> Result<(), String> {
    let sig = minisign_verify::Signature::decode(sig_text)
        .map_err(|e| format!("Invalid release signature: {e}"))?;
    if sig.trusted_comment() != expected {
        return Err(format!("Release signature identity mismatch — expected trusted comment: {expected}"));
    }
    Ok(())
}

pub(super) fn verify_binary_version_output(text: &str, expected: &str) -> Result<(), String> {
    let versions: Vec<_> = text.lines().filter_map(|line| line.strip_prefix("duduclaw ")).collect();
    if versions.len() != 1 || !plain_version(expected) || versions[0] != expected {
        return Err(format!("New binary version mismatch — expected duduclaw {expected}"));
    }
    Ok(())
}

pub(super) fn verify_binary_version<R: Read + Seek>(binary: &mut R, expected: &str) -> Result<(), String> {
    if !plain_version(expected) { return Err("Invalid expected binary version".into()); }
    binary.seek(SeekFrom::Start(0)).map_err(|e| format!("Cannot seek staged binary: {e}"))?;
    // Encoding prevents linker string pooling from putting parser literals
    // next to each other and accidentally creating another magic occurrence.
    let magic: Vec<u8> = std::hint::black_box([
        0xe1, 0xf0, 0xe1, 0xf0, 0xe6, 0xe9, 0xe4, 0xf2, 0xfa, 0xf7, 0xe0, 0xe9, 0xe0, 0xe4, 0xf6, 0xe0, 0xfa, 0xec, 0xe1,
    ]).iter().map(|byte| byte ^ 0xa5).collect();
    let header = [b"\0".as_slice(), &magic, b":V1:"].concat();
    let suffix = b"\0DUDUCLAW_RELEASE_END\0";
    let mut found = false;
    let mut window = Vec::new();
    let mut scanned = 0;
    let mut chunk = [0u8; 64 * 1024];
    let max_record = header.len() + 62 + suffix.len();
    loop {
        let n = binary.read(&mut chunk).map_err(|e| format!("Cannot read staged binary: {e}"))?;
        window.extend_from_slice(&chunk[..n]);
        let scan_end = if n == 0 { window.len() }
            else { window.len().saturating_sub(max_record) };
        for offset in scanned..scan_end {
            if window.get(offset..offset + magic.len()) != Some(magic.as_slice()) { continue; }
            if found { return Err("Duplicate binary release marker".into()); }
            if offset == 0 || window.get(offset - 1..offset - 1 + header.len()) != Some(header.as_slice()) {
                return Err("Malformed binary release marker header".into());
            }
            let tail = &window[offset - 1 + header.len()..];
            let end = tail.iter().position(|byte| *byte == 0)
                .ok_or("Unterminated binary release marker")?;
            if end > 62 || tail.get(end..end + suffix.len()) != Some(suffix.as_slice()) {
                return Err("Malformed binary release marker suffix".into());
            }
            let actual = std::str::from_utf8(&tail[..end]).map_err(|_| "Invalid binary version marker")?;
            if !plain_version(actual) || actual != expected {
                return Err(format!("Extracted binary version mismatch — expected {expected}, found {actual}"));
            }
            found = true;
        }
        if n == 0 { break; }
        // Keep the previous byte as well as every incomplete candidate. Each
        // magic occurrence is inspected exactly once, even across read boundaries.
        let discard = scan_end.saturating_sub(1);
        window.drain(..discard);
        scanned = scan_end - discard;
    }
    if !found { return Err("Extracted binary has no release version marker — refusing update".into()); }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::io::Cursor;

    // Generated with a disposable minisign test keypair; no production key is used.
    const TEST_KEY: &str = "RWSogglyHN/WxGMZkSWGV5VM6rEFkQpHuX2repAN8x2kIYCJTh3ckasn";
    const GOOD: &str = "untrusted comment: signature from minisign secret key\nRUSogglyHN/WxHXHQJaqA+U93A5J73DUZ7Yy0kc0oD1IYIZWHR5RCJSYeDin5JBR1GD/qhtP+419UvV7um4ul/sG7aAD894QuAE=\ntrusted comment: duduclaw v1.80.0 duduclaw-linux-x64.tar.gz\neRtCE2ym3+U26FCm9bszom44IRZJZTAiYhqnXnAXMnzcHJJ41voHrS3efG81atkOX+lF5Lc7ekzyR8vmaKWaDg==\n";
    const OLD: &str = "untrusted comment: signature from minisign secret key\nRUSogglyHN/WxHXHQJaqA+U93A5J73DUZ7Yy0kc0oD1IYIZWHR5RCJSYeDin5JBR1GD/qhtP+419UvV7um4ul/sG7aAD894QuAE=\ntrusted comment: duduclaw v1.79.3 duduclaw-linux-x64.tar.gz\nlfwj8xTwLyhEMXT3D1EKVkNvYPjmrqutk2esuRBxljXC/RkKwEucfoFrty1YMWLJShqkAMixM6g3I6OWuk/CDw==\n";
    const ARCH: &str = "untrusted comment: signature from minisign secret key\nRUSogglyHN/WxHXHQJaqA+U93A5J73DUZ7Yy0kc0oD1IYIZWHR5RCJSYeDin5JBR1GD/qhtP+419UvV7um4ul/sG7aAD894QuAE=\ntrusted comment: duduclaw v1.80.0 duduclaw-linux-arm64.tar.gz\n7znOt/Y784XHwcCSWB5eTVhBtfa3qHa/t8llTWRwwW9EN3ob8FqWo1fF5wWWt1lvN9v8wbngy2hSkIEAmCPXAg==\n";
    const FILE: &str = "untrusted comment: signature from minisign secret key\nRUSogglyHN/WxHXHQJaqA+U93A5J73DUZ7Yy0kc0oD1IYIZWHR5RCJSYeDin5JBR1GD/qhtP+419UvV7um4ul/sG7aAD894QuAE=\ntrusted comment: duduclaw v1.80.0 other.tar.gz\n89BvgBwEdQQqUXypqRbG4cE4ymuAxrTr4CcvUryPagd1AAGNl12n1oH4DTsukpxCGlF45OogO0NpoMzrdlefCA==\n";
    const DEFAULT: &str = "untrusted comment: signature from minisign secret key\nRUSogglyHN/WxHXHQJaqA+U93A5J73DUZ7Yy0kc0oD1IYIZWHR5RCJSYeDin5JBR1GD/qhtP+419UvV7um4ul/sG7aAD894QuAE=\ntrusted comment: timestamp:1793824731\tfile:archive\thashed\n9tqhfVagP2+rCmJD3d/jJdVQDiRbGoCe6hl8ZXzpjaurEp9uV9cIl8IECksqrlo37kcgEWlwZuJ2cdmDreZ9Cg==\n";

    #[test]
    fn release_identity_signed_comment_matches_exact_version_and_file() {
        for (sig, valid) in [(GOOD, true), (OLD, false), (ARCH, false), (FILE, false), (DEFAULT, false)] {
            let result = verify_release_comment(b"release identity fixture\n", sig, TEST_KEY,
                "1.80.0", "duduclaw-linux-x64.tar.gz");
            assert_eq!(result.is_ok(), valid, "{result:?}");
        }
    }

    #[test]
    fn release_identity_global_signature_covers_comment() {
        let forged = OLD.replace("v1.79.3", "v1.80.0");
        let error = verify_release_comment(b"release identity fixture\n", &forged, TEST_KEY,
            "1.80.0", "duduclaw-linux-x64.tar.gz").unwrap_err();
        assert!(error.contains("FAILED"), "{error}");
        let missing = GOOD.lines().take(2).collect::<Vec<_>>().join("\n");
        assert!(verify_release_comment(b"release identity fixture\n", &missing, TEST_KEY,
            "1.80.0", "duduclaw-linux-x64.tar.gz").is_err());
    }

    fn marker(version: &str) -> Vec<u8> {
        [b"\0DUDUCLAW_RELEASE_ID:V1:".as_slice(), version.as_bytes(), b"\0DUDUCLAW_RELEASE_END\0"].concat()
    }

    fn extracted(version: &str) -> Cursor<Vec<u8>> {
        let binary = marker(version);
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut archive = tar::Builder::new(gzip);
        let mut header = tar::Header::new_gnu();
        header.set_size(binary.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive.append_data(&mut header, "duduclaw", binary.as_slice()).unwrap();
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        let mut output = Cursor::new(Vec::new());
        extract_binary_to(&bytes, "duduclaw", &mut output).unwrap();
        output
    }

    #[test]
    fn release_identity_extracted_binary_version_must_match() {
        assert!(verify_binary_version(&mut extracted("1.80.0"), "1.80.0").is_ok());
        let error = verify_binary_version(&mut extracted("1.79.3"), "1.80.0").unwrap_err();
        assert!(error.contains("version mismatch"), "{error}");
        assert!(verify_binary_version(&mut Cursor::new(b"old binary"), "1.80.0").is_err());
        assert!(verify_binary_version(&mut extracted("01.80.0"), "1.80.0").is_err());
    }

    #[test]
    fn release_identity_marker_survives_chunk_boundaries_and_rejects_conflicts() {
        for offset in [0, 65500, 65535, 65536, 131070] {
            let mut bytes = vec![b'x'; offset];
            bytes.extend(marker("1.80.0"));
            assert!(verify_binary_version(&mut Cursor::new(&bytes), "1.80.0").is_ok());
            bytes.extend(marker("1.79.3"));
            assert!(verify_binary_version(&mut Cursor::new(&bytes), "1.80.0").is_err());
        }
        assert!(verify_binary_version(&mut Cursor::new(RELEASE_VERSION.as_bytes()),
            env!("CARGO_PKG_VERSION")).is_ok());
    }

    #[test]
    fn release_identity_url_requires_exact_platform_and_version() {
        let file = format!("duduclaw-{}", platform_asset_suffix());
        let base = format!("https://github.com/{GITHUB_REPO}/releases/download/v1.80.0/{file}");
        assert_eq!(release_identity(&base).unwrap(), ("1.80.0".into(), file));
        for bad in [base.replace("v1.80.0", "v1.79.3/extra"), base.replace("v1.80.0", "v01.80.0"),
            base.replace("v1.80.0", "v1.80.0-rc.1"), base.replace("github.com", "github.com.evil.com"),
            format!("{base}?file=other"), format!("{base}#other"), format!("{base}.sha256"),
            base.replace("duduclaw-", "other-")] {
            assert!(release_identity(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn release_identity_bundle_asset_is_macos_only() {
        let mac = ["darwin-arm64.tar.gz", "darwin-arm64.app.zip"];
        assert_eq!(product_for_file("duduclaw-darwin-arm64.app.zip", &mac), Some("duduclaw"));
        assert_eq!(product_for_file("duduclaw-pro-darwin-arm64.app.zip", &mac), Some("duduclaw-pro"));
        assert_eq!(product_for_file("duduclaw-darwin-arm64.tar.gz", &mac), Some("duduclaw"));
        assert_eq!(product_for_file("duduclaw-darwin-x64.app.zip", &mac), None);
        assert_eq!(product_for_file("duduclaw-linux-x64.app.zip", &["linux-x64.tar.gz"]), None);
        assert!(is_bundle_file("duduclaw-darwin-arm64.app.zip"));
        assert!(!is_bundle_file("duduclaw-darwin-arm64.tar.gz"));
        // This host: the bundle asset is accepted iff it is macOS.
        let suffixes = release_suffixes();
        assert_eq!(suffixes.iter().any(|s| s.ends_with(".app.zip")), cfg!(target_os = "macos"));
        let bundle = format!("https://github.com/{GITHUB_REPO}/releases/download/v1.81.0/duduclaw-{}",
            bundle_asset_suffix().unwrap_or("darwin-arm64.app.zip"));
        assert_eq!(release_identity(&bundle).is_ok(), cfg!(target_os = "macos"));
        let pro = bundle.replace("/duduclaw-", "/duduclaw-pro-");
        assert!(release_identity(&pro).is_err(), "public channel never serves Pro");
    }

    #[test]
    fn release_identity_binary_version_output_is_exact() {
        assert!(verify_binary_version_output("log\nduduclaw 1.80.0\n", "1.80.0").is_ok());
        for output in ["duduclaw 1.79.3\n", "duduclaw 1.80.0-rc.1\n", "log 1.80.0\n",
            "duduclaw 1.80.0 extra\n", "duduclaw 1.80.0\nduduclaw 1.79.3\n", ""] {
            assert!(verify_binary_version_output(output, "1.80.0").is_err(), "{output}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn release_identity_binary_mismatch_precedes_swap() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("duduclaw");
        let staged = dir.path().join("new-duduclaw");
        std::fs::write(&installed, b"installed binary").unwrap();
        std::fs::write(&staged, b"#!/bin/sh\nprintf 'duduclaw 1.79.3\\n'\n").unwrap();
        let error = apply_update_inner(&staged, &installed, dir.path(), "1.80.0")
            .await.unwrap_err();
        assert!(error.contains("version mismatch"), "{error}");
        assert_eq!(std::fs::read(&installed).unwrap(), b"installed binary");
        assert!(!dir.path().join("duduclaw.bak").exists());
        std::fs::write(&staged, b"#!/bin/sh\nprintf 'duduclaw 1.80.0\\n'\n").unwrap();
        assert!(apply_update_inner(&staged, &installed, dir.path(), "1.80.0")
            .await.unwrap().success);
        assert_eq!(std::fs::read(&installed).unwrap(), b"#!/bin/sh\nprintf 'duduclaw 1.80.0\\n'\n");
    }


    const PROVIDER_KEY: &str = "RWRbEa6SmVPIRbbBjq6dOIaRfIdRQmk1DOl0OODuB+3KhTl7FGJgRFjm";
    const OLD_ARCHIVE: &[u8] = include_bytes!("fixtures/old-release.tar.gz");

    fn signed_fixture(product: &str, version: &str) -> (String, &'static str) {
        let file = format!("{product}-{}", platform_asset_suffix());
        let signature = match (file.as_str(), version) {
            ("duduclaw-linux-x64.tar.gz", "1.80.0") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw v1.80.0 duduclaw-linux-x64.tar.gz\njmkkoElXcqU4QAV1w+KzUDJLcXNhb4/IeNUBx8fh5M0MuhrBbf14F/+n2STO1Hc5uSYt8C8OJBTQtAdPB2Y8DQ==\n",
            ("duduclaw-linux-x64.tar.gz", "1.79.3") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw v1.79.3 duduclaw-linux-x64.tar.gz\ngNItK3KxeABH29k3pdeik+SN8+y/m8a/biQ9lURoDw+a5ukbCXY7UO4DVSW6+fkaQzwMB7GnWpyjQb7+xdPRBA==\n",
            ("duduclaw-pro-linux-x64.tar.gz", "1.80.0") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw-pro v1.80.0 duduclaw-pro-linux-x64.tar.gz\nTZAWRyT966dRzWH8vDdaRsv2P8KXq8fZI55NuKQ1Heie4WRE3QN3MSG1xTV2zruz6gxpstdH8Ubjg1Skxz9lCg==\n",
            ("duduclaw-pro-linux-x64.tar.gz", "1.79.3") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw-pro v1.79.3 duduclaw-pro-linux-x64.tar.gz\nfQjJYYEHJAnu25hbmnlF1SllAeBcFLKNIglAszhX7bXXgDnbh+iGQUvXxYkNJvq9JpvrIBIaqCXyt0IRohCtCQ==\n",
            ("duduclaw-linux-arm64.tar.gz", "1.80.0") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw v1.80.0 duduclaw-linux-arm64.tar.gz\nQLNV2LxtpODnUvPLBn12fTDytdiWjgxJxZZEcstcSE+A8ujL2A3vLt3e++B07merDNyIiRThMEF87gR1I3LfDg==\n",
            ("duduclaw-linux-arm64.tar.gz", "1.79.3") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw v1.79.3 duduclaw-linux-arm64.tar.gz\ns0R5/rD71S9XkvdhUl+pFhJdOvo0pR+isNiZC2uYlmjBToLtuT9Dh7uPvvRi33AB98DRprj6Mi9NUZEcWEBcCg==\n",
            ("duduclaw-pro-linux-arm64.tar.gz", "1.80.0") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw-pro v1.80.0 duduclaw-pro-linux-arm64.tar.gz\n0YdLApB9X9r1x7uRz8oJwpkf0VLWBHjdmLUv9+tNgceiK0FyJ6F1DDdPMtXnKA4BQI0l/giAgQJzpTRAwYMLCg==\n",
            ("duduclaw-pro-linux-arm64.tar.gz", "1.79.3") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw-pro v1.79.3 duduclaw-pro-linux-arm64.tar.gz\ngKF7hIHxMtN5eA8y+INckkJSutM+ZDqVeQvg7anXFnMG8P/aWSybSN6xW5bvjg0C2ni5bRTmnz1KnQ2gbbqXAg==\n",
            ("duduclaw-darwin-x64.tar.gz", "1.80.0") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw v1.80.0 duduclaw-darwin-x64.tar.gz\naas9tq/wPcK4wNwC9Xswt2Gfrq1Qt3JDusYJAfsXkMavenwoLqk9AA+4ZAO92/sXSlzH8ox6642TtN/YPU77BA==\n",
            ("duduclaw-darwin-x64.tar.gz", "1.79.3") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw v1.79.3 duduclaw-darwin-x64.tar.gz\nm8x3q5JPRIaqYhoJcHfDEgozwYS7ke6XkkVOZoBo8K/zml+jrSw+KabshrjouDd3v9gTOK6WxbVBv349kueGDw==\n",
            ("duduclaw-pro-darwin-x64.tar.gz", "1.80.0") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw-pro v1.80.0 duduclaw-pro-darwin-x64.tar.gz\niUoe8EyDPf9LJ2+i4BI5iZb8yesJPRe/ZCzwgZBbCEPU/Gvo8zh6L8QW4SoLPil7UrmSg560rbcMxJy5lG+SDA==\n",
            ("duduclaw-pro-darwin-x64.tar.gz", "1.79.3") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw-pro v1.79.3 duduclaw-pro-darwin-x64.tar.gz\n2C0LXXRVhgPBkF13akNdPLq+gXpv36H1sFKuGA4mBWCGTHIOc7mHajVf2Cq2XiJhJ8S098qkQWT6Xh49RCpaAg==\n",
            ("duduclaw-darwin-arm64.tar.gz", "1.80.0") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw v1.80.0 duduclaw-darwin-arm64.tar.gz\nDgugB+vOsNEQPNnfkwy/m21VUYl5enwTELpOdNDk1OjZ0Um7VfCFy4gvFbrAxQHBKaSnkellz2EnZ2yDauuvBA==\n",
            ("duduclaw-darwin-arm64.tar.gz", "1.79.3") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw v1.79.3 duduclaw-darwin-arm64.tar.gz\nygUpaFhFS78yEGXvHMKh9BjF6PnQe32ZFN7v8/aRKnMKZOP7hicWhyZrgAywrxCvbeW4vD08wRuEeIjZrTIGAg==\n",
            ("duduclaw-pro-darwin-arm64.tar.gz", "1.80.0") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw-pro v1.80.0 duduclaw-pro-darwin-arm64.tar.gz\nIW77X3YRcIxqqz7Yq5j23n6ckKm2iMGyxkbFGjaQ/goRX4Tfvp2h6xiHuOAGkZbiKk2cFhsf+0NVKH9/hwwFBA==\n",
            ("duduclaw-pro-darwin-arm64.tar.gz", "1.79.3") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw-pro v1.79.3 duduclaw-pro-darwin-arm64.tar.gz\n+wKXFu0PGOwUn/ZFQAUN/5sUBQ+Hz9SAtC3ksWvQ1hknDtYMRjios2Ec/TEgc//8/ea2ZuOnOAWKIrHi/XjiAA==\n",
            ("duduclaw-windows-x64.zip", "1.80.0") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw v1.80.0 duduclaw-windows-x64.zip\nHLD6vyok8zX0FqI/7/TYbxduhycVr+uOqOolsnOf6vcWcJGBTszAKHcfEiXk1XSA4Hns3Qm1Qs2Qm/6xMYq5CA==\n",
            ("duduclaw-windows-x64.zip", "1.79.3") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw v1.79.3 duduclaw-windows-x64.zip\nJAu5ej3eHFI3EbS7GMIndJGYTFUvcG7bDo7//hA3xlrhP9EiYZfZVtq2mF4FwbXAIHmSk+YAJ1w5S2+W6agfCg==\n",
            ("duduclaw-pro-windows-x64.zip", "1.80.0") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw-pro v1.80.0 duduclaw-pro-windows-x64.zip\n272mwYw0smMii19/bPiv+xs86tjP353g4X87CeSIEbhLxOo3GnYt4hDLDXzS9Wdd5T5eF5YbWjxnpU/KEDeHCQ==\n",
            ("duduclaw-pro-windows-x64.zip", "1.79.3") => "untrusted comment: signature from minisign secret key\nRURbEa6SmVPIRegx5sjmwuZ9S0pvkYUPlzbxkvzzjKxirE61CehZUMb77qytA/TCiUkmdVEp7lw/jQRKzlMr7J8pUXp488Wq2Ac=\ntrusted comment: duduclaw-pro v1.79.3 duduclaw-pro-windows-x64.zip\n/RK2HBgpsEm/t35RPatl+2Qa9k8dcVrpIdw4GOT9h/k4h3Ov6Lq5FNAxihVpi7rwwER3LSl60BOAKlRAHJc4Bw==\n",
            _ => panic!("unsupported fixture platform"),
        };
        (file, signature)
    }

    pub(crate) fn verified_fixture() -> (&'static [u8], VerifiedReleaseIdentity) {
        let (file, signature) = signed_fixture("duduclaw", "1.80.0");
        let sum = format!("{:x}", Sha256::digest(OLD_ARCHIVE));
        (OLD_ARCHIVE, verify_release_archive_with_pubkey(OLD_ARCHIVE, &sum,
            signature, PROVIDER_KEY, "1.80.0", &file).unwrap())
    }

    #[tokio::test]
    async fn release_identity_provider_signed_old_archive_rejected_before_swap() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("duduclaw");
        std::fs::write(&installed, b"installed binary").unwrap();
        let sum = format!("{:x}", Sha256::digest(OLD_ARCHIVE));
        for product in ["duduclaw", "duduclaw-pro"] {
            let (file, signature) = signed_fixture(product, "1.79.3");
            // Integrity alone succeeds: the old payload and global signature are valid.
            assert!(verify_archive_with_pubkey(OLD_ARCHIVE, &sum, signature, PROVIDER_KEY).is_ok());
            let error = verify_release_archive_with_pubkey(OLD_ARCHIVE, &sum,
                signature, PROVIDER_KEY, "1.80.0", &file).unwrap_err();
            assert!(error.contains("identity mismatch"), "{error}");
            let (file, signature) = signed_fixture(product, "1.80.0");
            let identity = verify_release_archive_with_pubkey(OLD_ARCHIVE, &sum,
                signature, PROVIDER_KEY, "1.80.0", &file).unwrap();
            assert_eq!(identity.version(), "1.80.0");
            assert_eq!(identity.file(), file);
            assert_eq!(identity.signature(), signature);
            assert_eq!(identity.trusted_comment(), format!("{product} v1.80.0 {file}"));
            // A proof for one archive cannot authorize different bytes.
            assert!(install_archive_bytes(b"different archive", &installed, dir.path(), &identity).await.is_err());
            #[cfg(unix)]
            {
                // Even a signed comment for the requested version cannot hide an old binary.
                let error = install_archive_bytes(OLD_ARCHIVE, &installed, dir.path(), &identity)
                    .await.unwrap_err();
                assert!(error.contains("version mismatch"), "{error}");
            }
            assert_eq!(std::fs::read(&installed).unwrap(), b"installed binary");
            assert!(!dir.path().join("duduclaw.bak").exists());
        }
    }

    #[test]
    fn release_identity_marker_io_failures_are_refused() {
        struct Broken { seek_fails: bool }
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("read failed"))
            }
        }
        impl Seek for Broken {
            fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
                if self.seek_fails { Err(std::io::Error::other("seek failed")) } else { Ok(0) }
            }
        }
        for seek_fails in [true, false] {
            assert!(verify_binary_version(&mut Broken { seek_fails }, "1.80.0").is_err());
        }
    }

    #[test]
    fn release_identity_marker_rejects_every_malformed_or_duplicate_magic() {
        for offset in [0, 65450, 65520, 65535, 65536, 131070] {
            let valid = marker("1.80.0");
            let mut malformed = vec![
                b"DUDUCLAW_RELEASE_ID:V1:1.80.0\0DUDUCLAW_RELEASE_END\0".to_vec(),
                b"\0DUDUCLAW_RELEASE_ID:V2:1.80.0\0DUDUCLAW_RELEASE_END\0".to_vec(),
                b"\0DUDUCLAW_RELEASE_ID:V1:1.80.0".to_vec(),
                b"\0DUDUCLAW_RELEASE_ID:V1:1.80.0\0bad".to_vec(),
                marker("01.80.0"), marker("1.79.3"), marker(&"1".repeat(63)),
                marker("1.80.0"),
            ];
            malformed.push(b"\0DUDUCLAW_RELEASE_ID".to_vec());
            for bad in malformed {
                for pair in [[bad.as_slice(), valid.as_slice()], [valid.as_slice(), bad.as_slice()]] {
                    let mut bytes = vec![b'x'; offset];
                    bytes.extend_from_slice(pair[0]);
                    bytes.extend_from_slice(pair[1]);
                    assert!(verify_binary_version(&mut Cursor::new(bytes), "1.80.0").is_err());
                }
                let mut bytes = vec![b'x'; offset];
                bytes.extend(bad);
                // A single valid marker is the only accepted case in the table.
                if bytes.ends_with(&valid) && bytes.len() == offset + valid.len() { continue; }
                assert!(verify_binary_version(&mut Cursor::new(bytes), "1.80.0").is_err());
            }
        }
    }

}
