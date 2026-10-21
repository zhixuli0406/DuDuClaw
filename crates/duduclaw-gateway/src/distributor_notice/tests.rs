use super::build_input::{generate, read_notice_file, validate_notice_path};
use super::sanitize::{
    MAX_NOTICE_BYTES, MAX_NOTICE_IMAGE_BYTES, NoticeError, notice_image_ok, notice_link,
    sanitize_notice, sanitize_notice_html,
};
use super::{distributor_notice, is_compiled_in, resanitize};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};

const VECTORS: &str = include_str!("vectors.json");
/// SHA-256 of `vectors.json`. The license console keeps a byte-identical copy
/// and asserts the same value; change the vectors in both places together.
const VECTORS_SHA256: &str = "8a08aa2c8a1bc6839e811118d7f66edbda364ec921d72b402159f93458d55471";
const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

fn vectors() -> Vec<serde_json::Value> {
    let parsed: serde_json::Value = serde_json::from_str(VECTORS).expect("vectors.json parses");
    parsed["vectors"].as_array().expect("vectors array").clone()
}

#[test]
fn shared_vector_file_is_the_pinned_revision() {
    use sha2::{Digest, Sha256};
    assert_eq!(hex::encode(Sha256::digest(VECTORS.as_bytes())), VECTORS_SHA256);
}

#[test]
fn shared_vectors_produce_the_recorded_output() {
    let all = vectors();
    assert!(all.len() >= 25, "vector file unexpectedly small");
    for vector in all {
        let name = vector["name"].as_str().unwrap();
        let input = vector["input"].as_str().unwrap();
        let actual = sanitize_notice_html(input).ok();
        let expected = vector["expected"].as_str().map(str::to_string);
        assert_eq!(actual, expected, "vector {name}");
    }
}

fn sorted_chars(s: &str) -> Vec<char> {
    let mut chars: Vec<char> = s.chars().collect();
    chars.sort_unstable();
    chars
}

/// The runtime pass re-sanitizes build-time output. It must not drop
/// anything; ammonia may only reorder attributes (it swap-removes dropped
/// ones), and a third pass is a fixed point.
#[test]
fn sanitized_output_survives_a_second_pass() {
    for vector in vectors() {
        let Some(expected) = vector["expected"].as_str() else { continue };
        let again = sanitize_notice_html(expected).expect("second pass accepts");
        assert_eq!(sorted_chars(&again), sorted_chars(expected), "vector {}", vector["name"]);
        assert_eq!(sanitize_notice_html(&again).as_deref(), Ok(again.as_str()), "vector {}", vector["name"]);
    }
}

#[test]
fn hostile_markup_never_survives() {
    for vector in vectors() {
        let Some(out) = vector["expected"].as_str() else { continue };
        let lower = out.to_ascii_lowercase();
        for needle in ["<script", "onclick", "onerror", "onmouseover", "style=", "class=", "javascript:", "<iframe", "<form", "<object", "<svg", "data:text/html", "data:image/svg"] {
            assert!(!lower.contains(needle), "{needle} survived in {}", vector["name"]);
        }
    }
}

#[test]
fn every_surviving_link_is_stamped_with_rel_and_blank_target() {
    let out = sanitize_notice_html(r#"<a href="https://example.com" target="_top">x</a>"#).unwrap();
    assert!(out.contains(r#"rel="nofollow noopener noreferrer""#), "{out}");
    assert!(out.contains(r#"target="_blank""#), "{out}");
    assert!(!out.contains("_top"), "{out}");
}

#[test]
fn oversize_documents_are_refused_not_truncated() {
    let at_limit = format!("<p>{}</p>", "a".repeat(MAX_NOTICE_BYTES - 7));
    assert_eq!(at_limit.len(), MAX_NOTICE_BYTES);
    assert!(sanitize_notice_html(&at_limit).is_ok());
    let over = format!("{at_limit} ");
    assert_eq!(
        sanitize_notice_html(&over),
        Err(NoticeError::TooLarge { bytes: MAX_NOTICE_BYTES + 1 })
    );
}

#[test]
fn output_that_grows_past_the_limit_is_refused_at_build_time() {
    // Each bare link gains rel/target during sanitization (~60 bytes).
    let link = r#"<a href="https://e.co">x</a>"#;
    let raw = link.repeat(MAX_NOTICE_BYTES / link.len());
    assert!(raw.len() <= MAX_NOTICE_BYTES);
    match sanitize_notice_html(&raw) {
        Err(NoticeError::TooLargeAfterSanitizing { bytes }) => assert!(bytes > MAX_NOTICE_BYTES),
        other => panic!("expected TooLargeAfterSanitizing, got {other:?}"),
    }
}

#[test]
fn image_policy_checks_magic_bytes_and_decoded_size() {
    assert!(notice_image_ok(&format!("data:image/png;base64,{PNG_1X1}")));
    let mut big = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    big.resize(MAX_NOTICE_IMAGE_BYTES + 1, 0);
    assert!(!notice_image_ok(&format!("data:image/png;base64,{}", BASE64.encode(&big))));
    big.truncate(MAX_NOTICE_IMAGE_BYTES);
    assert!(notice_image_ok(&format!("data:image/png;base64,{}", BASE64.encode(&big))));
    let svg = BASE64.encode(b"<svg><script>alert(1)</script></svg>");
    assert!(!notice_image_ok(&format!("data:image/png;base64,{svg}")));
    assert!(!notice_image_ok(&format!("data:image/svg+xml;base64,{svg}")));
    assert!(!notice_image_ok("data:image/gif;base64,R0lGODlhAQABAAAAACw="));
    assert!(!notice_image_ok("https://example.com/a.png"));
}

#[test]
fn link_policy_allows_only_absolute_http_https_and_mailto() {
    assert_eq!(notice_link("https://example.com/a").as_deref(), Some("https://example.com/a"));
    assert_eq!(notice_link("HTTP://Example.com").as_deref(), Some("http://example.com/"));
    assert_eq!(notice_link("mailto:a@example.com").as_deref(), Some("mailto:a@example.com"));
    for refused in ["https://user:pass@example.com/", "https://user@example.com/", "javascript:alert(1)", " java\tscript:alert(1)", "data:text/html,x", "/relative", "//example.com", "vbscript:x", "file:///etc/passwd", "mailto:", "https://"] {
        assert_eq!(notice_link(refused), None, "{refused}");
    }
}

#[test]
fn report_lists_what_the_allowlist_removed() {
    let raw = r#"<p onclick="x()" style="a">t</p><script>var s = "<b>";</script><a href="javascript:alert(1)">j</a><img src="data:image/svg+xml;base64,AAAA"><a href="https://ok.example">ok</a>"#;
    let report = sanitize_notice(raw).unwrap();
    assert!(report.has_removals());
    assert!(report.removed_elements.contains("script"));
    assert!(!report.removed_elements.contains("b"), "script body must not be scanned");
    assert!(report.removed_attributes.contains("p[onclick]"));
    assert!(report.removed_attributes.contains("p[style]"));
    assert_eq!(report.rejected_urls.len(), 2, "{:?}", report.rejected_urls);
    assert!(report.rejected_urls.iter().any(|u| u.starts_with("a[href] javascript:")));
    assert!(report.rejected_urls.iter().any(|u| u == "img[src] data URI"));

    let clean = sanitize_notice("<p>plain</p>").unwrap();
    assert!(!clean.has_removals());
    // Re-sanitizing output (a console export) reports nothing removed.
    let again = sanitize_notice(&sanitize_notice_html(raw).unwrap()).unwrap();
    assert!(!again.has_removals(), "{again:?}");
}

#[test]
fn runtime_resanitization_fails_closed() {
    assert_eq!(resanitize(None), None);
    assert_eq!(resanitize(Some("<script>x</script>")), None);
    assert_eq!(resanitize(Some("<p onclick=\"x\">a</p>")).as_deref(), Some("<p>a</p>"));
}

#[test]
fn compiled_notice_matches_the_build_input() {
    // CI and public builds leave DUDUCLAW_DISTRIBUTOR_NOTICE_PATH unset.
    assert_eq!(distributor_notice().is_some(), is_compiled_in());
}

#[test]
fn build_input_path_rules() {
    assert!(validate_notice_path("").is_err());
    assert!(validate_notice_path("relative/notice.html").is_err());
    assert!(validate_notice_path("/tmp/a\nb.html").is_err());
    assert!(validate_notice_path("/tmp/notice.html").is_ok());
}

#[test]
fn build_input_file_rules() {
    let dir = tempfile::tempdir().unwrap();
    let ok = dir.path().join("ok.html");
    std::fs::write(&ok, "\u{feff}<p>總經銷</p>").unwrap();
    assert_eq!(read_notice_file(&ok, MAX_NOTICE_BYTES).unwrap(), "<p>總經銷</p>");

    let big = dir.path().join("big.html");
    std::fs::write(&big, vec![b'a'; MAX_NOTICE_BYTES + 1]).unwrap();
    assert!(read_notice_file(&big, MAX_NOTICE_BYTES).unwrap_err().contains("larger than 64 KB"));

    let bad = dir.path().join("bad.html");
    std::fs::write(&bad, [0xff, 0xfe, 0x00]).unwrap();
    assert!(read_notice_file(&bad, MAX_NOTICE_BYTES).unwrap_err().contains("UTF-8"));

    assert!(read_notice_file(&dir.path().join("missing.html"), MAX_NOTICE_BYTES).is_err());
    assert!(read_notice_file(dir.path(), MAX_NOTICE_BYTES).is_err());
    #[cfg(unix)]
    {
        let link = dir.path().join("link.html");
        std::os::unix::fs::symlink(&ok, &link).unwrap();
        assert!(read_notice_file(&link, MAX_NOTICE_BYTES).unwrap_err().contains("symlinks"));
    }
}

#[test]
fn generated_source_is_a_valid_literal() {
    assert!(generate(None).contains("Option<&str> = None;"));
    let source = generate(Some("<p>\"引號\" \\ </p>"));
    assert!(source.contains(r#"Some("<p>\"引號\" \\ </p>")"#), "{source}");
}
