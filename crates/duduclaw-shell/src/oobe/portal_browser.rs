// Captive-portal sign-in page opener — D4a §6 (2026-08-23), the "開啟登入
// 頁" affordance the round's decision list calls for ("captive portal M1
// 偵測＋開啟登入頁鈕").
//
// ── The URL here is hostile input, and that is the whole reason this file
//    exists rather than a one-line `Command::new("chromium").arg(url)` ────
// `NetworkStatus::portal_url` originates in the gateway's connectivity
// probe, which reads it out of a `Location:` header returned by whatever
// access point the box just joined. That access point is, by construction,
// one the operator has NOT authenticated to and may not control — a café,
// a hotel, or an attacker's rogue AP. So the string travelling into this
// module is attacker-chosen text, and every property we want from it has to
// be checked here, not assumed:
//
//   * It must be `http://` or `https://`. A `file://`, `javascript:` or
//     `chrome://` URL handed to a browser does something quite different
//     from "show me a sign-in page".
//   * It must not begin with `-`. An argv element starting with a dash is
//     read by the browser as a FLAG, not a URL — `--proxy-server=...` and
//     friends are real, and a browser started with an attacker's flags is
//     worse than no browser at all. The `http://` prefix rule already
//     implies this, but it is asserted separately so the guarantee survives
//     any future loosening of the scheme rule.
//   * It must contain no whitespace and no ASCII control characters. These
//     never appear in a legitimate URL and are the usual raw material for
//     confusing a downstream parser.
//   * It must be entirely printable ASCII. A real portal URL can carry
//     non-ASCII only percent-encoded; raw bytes above 0x7E mean either a
//     broken portal or a deliberate attempt at a homograph, and refusing to
//     open it is the honest outcome.
//   * It must be bounded in length. 2000 characters is the conventional
//     practical URL ceiling and far more than any sign-in redirect needs.
//
// Nothing here goes through a shell: the URL is always ONE argv element of
// a directly-spawned process, so there is no quoting or word-splitting
// surface even before the checks above.
//
// ── Why a fixed candidate list, not xdg-open first ───────────────────────
// The appliance image installs Chromium as a Debian package (see
// `appliance/mkosi.conf`) and runs it under this shell's own Wayland
// compositor. `--ozone-platform=wayland` is required there for the same
// measured reason `apps.rs`'s header comment already records for Flathub
// Chromium: without it Chromium picks the X11 ozone backend and no
// environment variable can override that choice. `xdg-open` is kept as the
// last resort for a dev box that has some other browser — it is tried LAST
// precisely because it cannot be given that flag.

use std::process::Command;

/// Practical URL ceiling — see this module's header comment.
const MAX_PORTAL_URL_LEN: usize = 2000;

/// Is `raw` a URL this module is willing to hand to a browser? Pure, so the
/// whole rule set is testable without spawning anything. See the module
/// header for why each clause is here; every one of them is a refusal
/// reason, and an unrecognised shape is always a refusal, never a
/// pass-through (coding convention: security gates fail closed).
pub(crate) fn is_safe_portal_url(raw: &str) -> bool {
    if raw.is_empty() || raw.len() > MAX_PORTAL_URL_LEN {
        return false;
    }
    // Scheme check is case-insensitive (`HTTP://` is legal) but anchored —
    // never a `contains` (coding convention 2): `javascript:alert('http://')`
    // contains the substring and must not pass.
    let lower = raw.to_ascii_lowercase();
    let rest = match (lower.strip_prefix("http://"), lower.strip_prefix("https://")) {
        (Some(r), _) => r,
        (_, Some(r)) => r,
        _ => return false,
    };
    // A scheme with no authority at all ("http://") points nowhere.
    if rest.is_empty() {
        return false;
    }
    // Belt-and-braces against argv flag injection — implied by the scheme
    // rule above, asserted anyway so it survives any future change to it.
    if raw.starts_with('-') {
        return false;
    }
    // Printable ASCII only: this rejects control characters, every flavour
    // of whitespace, and any raw non-ASCII byte, in one pass.
    raw.chars().all(|c| c.is_ascii_graphic())
}

/// Candidate browsers, in the order they are tried. First one that spawns
/// wins; a missing binary is not an error, it is just the next candidate.
fn candidates(url: &str) -> Vec<(&'static str, Vec<String>)> {
    vec![
        ("chromium", vec!["--ozone-platform=wayland".to_string(), url.to_string()]),
        ("chromium-browser", vec!["--ozone-platform=wayland".to_string(), url.to_string()]),
        ("xdg-open", vec![url.to_string()]),
    ]
}

/// Opens the captive-portal sign-in page, or does nothing and says so.
///
/// Refuses outright (logging one line, never silently) when
/// [`is_safe_portal_url`] rejects the URL — the operator sees no browser and
/// the reason is on stderr, which is strictly better than opening whatever
/// the hostile network asked us to open.
///
/// Never blocks: the child is spawned and deliberately not waited on (this
/// runs from a gpui click handler; see `steps::network`'s own header comment
/// on why no I/O may block that thread). The child is not reaped either —
/// same accepted trade-off `apps::launch` already makes for app launches on
/// this single-purpose, long-lived session.
pub(crate) fn open_portal(url: &str) {
    if !is_safe_portal_url(url) {
        // Deliberately does NOT echo the rejected URL: it is attacker-chosen
        // text and this line lands in the journal on an appliance.
        eprintln!("[oobe/network] refusing to open the captive-portal URL — it is not a plain http(s) URL of a safe shape");
        return;
    }
    for (program, args) in candidates(url) {
        match Command::new(program).args(&args).spawn() {
            Ok(_child) => {
                if crate::diag_enabled() {
                    eprintln!("[oobe/network] opened the captive-portal sign-in page via {program}");
                }
                return;
            }
            Err(e) => {
                if crate::diag_enabled() {
                    eprintln!("[oobe/network] {program} unavailable for the captive-portal page ({e}) — trying the next candidate");
                }
            }
        }
    }
    // Always logged, not DIAG-gated: this happens once per click and is the
    // only trace the operator's click leaves when nothing could be launched.
    eprintln!("[oobe/network] could not open the captive-portal sign-in page — no browser found (tried chromium, chromium-browser, xdg-open)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_http_and_https_urls() {
        assert!(is_safe_portal_url("http://192.168.1.1/login"));
        assert!(is_safe_portal_url("https://portal.example.com/auth?token=abc123&next=%2F"));
        assert!(is_safe_portal_url("HTTP://Portal.Example.COM/Login"), "scheme match must be case-insensitive");
    }

    #[test]
    fn rejects_every_non_http_scheme() {
        for bad in [
            "file:///etc/shadow",
            "javascript:alert(1)",
            "chrome://settings",
            "data:text/html,<h1>hi</h1>",
            "ftp://example.com/",
            "//example.com/login",
            "example.com/login",
        ] {
            assert!(!is_safe_portal_url(bad), "{bad} must be refused");
        }
    }

    #[test]
    fn a_non_http_scheme_that_merely_contains_the_substring_is_still_refused() {
        // The exact reason the scheme check is anchored rather than a
        // `contains` (coding convention 2).
        assert!(!is_safe_portal_url("javascript:location='http://evil.example'"));
    }

    #[test]
    fn rejects_argv_flag_shapes() {
        assert!(!is_safe_portal_url("--proxy-server=http://evil.example:8080"));
        assert!(!is_safe_portal_url("-http://example.com"));
    }

    #[test]
    fn rejects_whitespace_and_control_characters() {
        assert!(!is_safe_portal_url("http://example.com/a b"));
        assert!(!is_safe_portal_url("http://example.com/a\tb"));
        assert!(!is_safe_portal_url("http://example.com/a\nb"));
        assert!(!is_safe_portal_url("http://example.com/\u{0}"));
        assert!(!is_safe_portal_url("http://example.com/login "));
    }

    #[test]
    fn rejects_raw_non_ascii() {
        // A legitimate portal URL percent-encodes these; raw bytes mean a
        // broken portal or a homograph attempt, and refusing is honest.
        assert!(!is_safe_portal_url("http://例え.example/login"));
        assert!(!is_safe_portal_url("http://example.com/\u{202e}gnol"));
    }

    #[test]
    fn rejects_empty_authority_and_over_long_urls() {
        assert!(!is_safe_portal_url(""));
        assert!(!is_safe_portal_url("http://"));
        assert!(!is_safe_portal_url("https://"));
        let too_long = format!("http://example.com/{}", "a".repeat(MAX_PORTAL_URL_LEN));
        assert!(!is_safe_portal_url(&too_long));
    }

    #[test]
    fn candidates_pass_the_url_as_one_argv_element_and_never_through_a_shell() {
        let url = "https://portal.example.com/login?a=1&b=2";
        for (_program, args) in candidates(url) {
            assert!(args.iter().any(|a| a == url), "the URL must appear verbatim as its own argv element");
            assert!(!args.iter().any(|a| a.contains(' ') && a != url), "no argv element may smuggle extra words");
        }
    }

    #[test]
    fn chromium_candidates_carry_the_wayland_ozone_flag() {
        // Not cosmetic: without it Chromium picks the X11 ozone backend under
        // this shell's compositor and no env var can override that (measured,
        // see apps.rs's own header comment).
        let url = "https://portal.example.com/login";
        for (program, args) in candidates(url) {
            if program.starts_with("chromium") {
                assert!(args.iter().any(|a| a == "--ozone-platform=wayland"), "{program} must request the Wayland ozone backend");
            }
        }
    }

    #[test]
    fn opening_a_refused_url_is_a_safe_noop() {
        // The assertion that matters is that this does not spawn anything and
        // does not panic; the refusal itself is covered above.
        open_portal("javascript:alert(1)");
    }
}
