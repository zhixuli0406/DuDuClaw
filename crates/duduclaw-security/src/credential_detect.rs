//! Bounded, deterministic credential detection. Findings never own secret values.

use regex::Regex;
use std::ops::Range;
use std::sync::OnceLock;

pub const MAX_SCAN_BYTES: usize = 64 * 1024;
pub const UNAVAILABLE_MASK: &str = "[憑證偵測不可用，已遮罩此訊息]";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub kind: String,
    pub span: Range<usize>,
    pub service_hint: Option<String>,
    pub account_hint: Option<String>,
    /// Only a complete provider or PEM structure may be fingerprinted.
    pub fingerprint_eligible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DetectError;

struct Rule {
    kind: &'static str,
    service: Option<&'static str>,
    pattern: Regex,
    structural: bool,
}

fn rules() -> Result<&'static Vec<Rule>, DetectError> {
    static RULES: OnceLock<Result<Vec<Rule>, DetectError>> = OnceLock::new();
    RULES.get_or_init(|| {
        let mut specs = [
            ("ssh_key", None, r"(?s)(?P<value>-----BEGIN (?:RSA |EC |OPENSSH |DSA |ENCRYPTED )?PRIVATE KEY-----.*?(?:-----END (?:RSA |EC |OPENSSH |DSA |ENCRYPTED )?PRIVATE KEY-----|\z))", true),
            ("certificate", None, r"(?s)(?P<value>-----BEGIN CERTIFICATE-----.*?(?:-----END CERTIFICATE-----|\z))", true),
            ("connection_string", None, r#"(?i)(?:^|[^A-Za-z0-9_])(?P<value>(?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis|mssql)://[^\s/:@]+:[^\s@]+@[^\s<>"']+)"#, false),
            ("api_key", Some("Google"), r"(?:^|[^A-Za-z0-9_])(?P<value>(?:AIza[A-Za-z0-9_-]{20,}|GOCSPX-[A-Za-z0-9_-]{16,}))", true),
            ("api_key", Some("AWS"), r"(?:^|[^A-Za-z0-9_])(?P<value>(?:AKIA|ASIA)[A-Z0-9]{16})", true),
            ("oauth_token", Some("Google"), r"(?:^|[^A-Za-z0-9_])(?P<value>ya29\.[A-Za-z0-9_.-]{16,})", true),
            ("api_key", None, r"(?:^|[^A-Za-z0-9_])(?P<value>(?:sk-|xox[bpaes]-|xapp-|gh[pousr]_|github_pat_|ddc_|glpat-|EAA)[A-Za-z0-9_-]{12,})", true),
            ("oauth_token", None, r"(?:^|[^A-Za-z0-9_])(?P<value>[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{20,})", false),
            ("oauth_token", Some("Telegram"), r"(?:^|[^A-Za-z0-9_])(?P<value>[0-9]{5,16}[:-][A-Za-z0-9_-]{20,})", true),
            ("password", None, r#"(?i)(?:^|[^A-Za-z0-9_])(?:password|passwd|pwd)["']?\s*(?P<assignment>:|=|\bis\b)\s*["'](?P<value>[^"'\r\n]+)["']"#, false),
            ("password", None, r#"密碼\s*(?:是|為|：|:|=)\s*[「『"'](?P<value>[^」』"'\r\n]+)[」』"']"#, false),
            ("password", None, r#"(?i)(?:^|[^A-Za-z0-9_])(?:password|passwd|pwd)["']?\s*(?P<assignment>:|=|\bis\b)\s*["']?(?P<value>[^\s"'<>，。；、\[\]]+)"#, false),
            ("password", None, r#"密碼\s*(?:是|為|：|:|=)\s*[「『"']?(?P<value>[^\s「」『』"'<>，。；、\[\]]+)"#, false),
            ("api_key", None, r#"(?i)(?:^|[^A-Za-z0-9_])(?:api[ _-]?key)["']?\s*(?:是|為|：|:|=|\bis\b)\s*["']?(?P<value>[^\s"'<>，。；、\[\]]+)"#, false),
            ("api_key", None, r#"金鑰\s*(?:是|為|：|:|=)\s*[「『"']?(?P<value>[^\s「」『』"'<>，。；、\[\]]+)"#, false),
            ("oauth_token", None, r#"(?i)(?:^|[^A-Za-z0-9_])(?:token)["']?\s*(?:是|為|：|:|=|\bis\b)\s*["']?(?P<value>[^\s"'<>，。；、\[\]]+)"#, false),
            ("oauth_token", None, r"(?i)(?:^|[^A-Za-z0-9_])Bearer\s+(?P<value>[A-Za-z0-9_./+=-]{20,})", false),
        ];
        specs.sort_by_key(|(kind, _, _, _)| match *kind {
            "connection_string" => 0, "password" => 1, "ssh_key" | "certificate" => 2, _ => 3,
        });
        specs.into_iter().map(|(kind, service, pattern, structural)| {
            Ok(Rule { kind, service, pattern: Regex::new(pattern).map_err(|_| DetectError)?, structural })
        }).collect()
    }).as_ref().map_err(|_| DetectError)
}

/// Deliberately narrower than masking: incomplete PEM, JWTs, bearer strings
/// and arbitrary labelled values must never become offline guessing oracles.
pub(crate) fn strict_structure(kind: &str, value: &str) -> bool {
    static PROVIDERS: OnceLock<Result<Regex, regex::Error>> = OnceLock::new();
    if matches!(kind, "ssh_key" | "certificate") {
        let mut lines = value.lines();
        let Some(begin) = lines.next() else { return false };
        let labels: &[&str] = if kind == "certificate" {
            &["CERTIFICATE"]
        } else {
            &["PRIVATE KEY", "RSA PRIVATE KEY", "EC PRIVATE KEY", "OPENSSH PRIVATE KEY", "DSA PRIVATE KEY", "ENCRYPTED PRIVATE KEY"]
        };
        let Some(label) = labels.iter().find(|label| begin == format!("-----BEGIN {label}-----")) else { return false };
        let end = format!("-----END {label}-----");
        let mut body = String::new();
        let mut ended = false;
        for line in lines {
            if ended { return false; }
            if line == end { ended = true; continue; }
            if line.is_empty() || !line.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')) {
                return false;
            }
            body.push_str(line);
        }
        if !ended || body.len() < 64 { return false; }
        use base64::Engine as _;
        return base64::engine::general_purpose::STANDARD.decode(&body).is_ok();
    }
    if !matches!(kind, "api_key" | "oauth_token") { return false; }
    let Ok(pattern) = PROVIDERS.get_or_init(|| Regex::new(
        r"\A(?:AIza[A-Za-z0-9_-]{35}|GOCSPX-[A-Za-z0-9_-]{28}|(?:AKIA|ASIA)[A-Z0-9]{16}|ya29\.[A-Za-z0-9_.-]{32,}|(?:sk-(?:proj-|ant-api03-)?|xox[bpaes]-|xapp-|gh[pousr]_|github_pat_|ddc_|glpat-|EAA)[A-Za-z0-9_-]{32,}|[0-9]{5,16}:[A-Za-z0-9_-]{35})\z",
    )) else { return false };
    pattern.is_match(value)
}

/// Mask JSON at persistence boundaries, including dynamic object keys and
/// values whose credential context is supplied by their field name.
pub fn mask_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = mask(text),
        serde_json::Value::Array(values) => values.iter_mut().for_each(mask_json),
        serde_json::Value::Object(values) => {
            let original = std::mem::take(values);
            for (key, mut child) in original {
                let normalized: String = key.chars().filter(|c| c.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase).collect();
                if matches!(normalized.as_str(), "password" | "passwd" | "pwd" | "token" | "apikey"
                    | "secret" | "clientsecret" | "accesstoken" | "refreshtoken" | "authorization"
                    | "connectionstring" | "privatekey") && child.is_string()
                {
                    child = serde_json::Value::String("[已遮罩的密碼]".into());
                } else {
                    mask_json(&mut child);
                }
                let key = mask(&key);
                if values.insert(key, child).is_some() {
                    values.clear();
                    return;
                }
            }
        }
        _ => (),
    }
}

/// Only the first character and a syntactically valid domain survive.
pub fn mask_account_hint(account: &str) -> String {
    let account = account.trim();
    let first = account.chars().next().unwrap_or('*');
    let domain = account.rsplit_once('@').map(|(_, domain)| domain).filter(|domain| {
        !domain.is_empty() && domain.split('.').count() >= 2
            && domain.split('.').all(|label| !label.is_empty()
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    });
    match domain {
        Some(domain) => format!("{first}***@{domain}"),
        None => format!("{first}***"),
    }
}

fn account_hint(text: &str) -> Result<Option<String>, DetectError> {
    static ACCOUNT: OnceLock<Result<Regex, regex::Error>> = OnceLock::new();
    let regex = ACCOUNT.get_or_init(|| Regex::new(
        r#"(?i)(?:帳號\s*(?:是|為|：|:|=)?\s*|(?:^|[^A-Za-z0-9_])(?:account|username|user)\s*[:=]\s*)["']?([^\s"'，。；、]+)"#,
    )).as_ref().map_err(|_| DetectError)?;
    Ok(regex.captures(text).and_then(|c| c.get(1)).map(|m| mask_account_hint(m.as_str())))
}

fn detect_inner(text: &str, limit: usize) -> Result<Vec<Finding>, DetectError> {
    let text = duduclaw_core::truncate_bytes(text, limit);
    let account = account_hint(text)?;
    let mut findings: Vec<Finding> = Vec::new();
    let mut occupied = vec![None; text.len()];
    for rule in rules()? {
        for captures in rule.pattern.captures_iter(text) {
            let value = captures.name("value").ok_or(DetectError)?;
            if rule.service == Some("AWS") && text.get(value.end()..)
                .and_then(|tail| tail.chars().next())
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                continue;
            }
            // Placeholders and routine status statements are not credentials.
            let lower = value.as_str().to_ascii_lowercase();
            if matches!(lower.as_str(), "***" | "****")
                || (captures.name("assignment").is_some_and(|m| m.as_str().eq_ignore_ascii_case("is"))
                    && matches!(lower.as_str(), "policy" | "required" | "invalid" | "expired" | "configured" | "not")) {
                continue;
            }
            let span = value.range();
            for owner in occupied.get_mut(span.clone()).ok_or(DetectError)? {
                // Context that identifies a password or connection string
                // wins over a vendor-shaped substring: H6 forbids hashing it.
                if owner.is_none() || matches!(rule.kind, "password" | "connection_string") {
                    *owner = Some(findings.len());
                }
            }
            findings.push(Finding {
                kind: rule.kind.into(), span,
                service_hint: rule.service.map(str::to_string), account_hint: account.clone(),
                fingerprint_eligible: rule.structural && strict_structure(rule.kind, value.as_str()),
            });
        }
    }
    // Union overlapping rules: a vendor-shaped prefix inside a longer password
    // must not leave the remainder exposed.
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < occupied.len() {
        let Some(mut index) = occupied[offset] else { offset += 1; continue; };
        let start = offset;
        while offset < occupied.len() {
            let Some(owner) = occupied[offset] else { break };
            if matches!(findings[owner].kind.as_str(), "password" | "connection_string") {
                index = owner;
            }
            offset += 1;
        }
        let mut finding = findings[index].clone();
        finding.fingerprint_eligible &= finding.span == (start..offset);
        finding.span = start..offset;
        result.push(finding);
    }
    Ok(result)
}

/// A panic or rule initialization failure is a value-free error.
pub fn detect(text: &str) -> Result<Vec<Finding>, DetectError> {
    guarded_detect(|| detect_inner(text, MAX_SCAN_BYTES))
}

fn guarded_detect<T>(scan: impl FnOnce() -> Result<T, DetectError> + std::panic::UnwindSafe)
    -> Result<T, DetectError>
{
    std::panic::catch_unwind(scan).unwrap_or(Err(DetectError))
}

/// Quotas limit ledger rows, never masking. Missing ids use a generic marker.
pub fn mask_with_ids(text: &str, findings: &[Finding], ids: &[String]) -> String {
    mask_with_ids_limit(text, findings, ids, MAX_SCAN_BYTES)
}

fn mask_with_ids_limit(text: &str, findings: &[Finding], ids: &[String], limit: usize) -> String {
    if text.len() > limit {
        return UNAVAILABLE_MASK.into();
    }
    let mut out = String::with_capacity(text.len());
    let mut end = 0;
    for (index, finding) in findings.iter().enumerate() {
        if finding.span.start < end || finding.span.end < finding.span.start {
            return UNAVAILABLE_MASK.into();
        }
        let Some(prefix) = text.get(end..finding.span.start) else { return UNAVAILABLE_MASK.into(); };
        if text.get(finding.span.clone()).is_none() { return UNAVAILABLE_MASK.into(); }
        out.push_str(prefix);
        if let Some(id) = ids.get(index).filter(|id| id.starts_with("cred_")
            && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
            out.push_str(&format!("[已遮罩的密碼 {id}]"));
        } else {
            out.push_str("[已遮罩的密碼]");
        }
        end = finding.span.end;
    }
    let Some(tail) = text.get(end..) else { return UNAVAILABLE_MASK.into(); };
    out.push_str(tail);
    out
}

/// All derived stores use this even when conversation mode is `audit`.
pub fn mask(text: &str) -> String {
    mask_detected(text, detect(text))
}

/// Mask a complete document. The caller must enforce its own size bound.
pub fn mask_document(text: &str) -> Result<String, DetectError> {
    guarded_detect(|| {
        let findings = detect_inner(text, text.len())?;
        let masked = mask_with_ids_limit(text, &findings, &[], text.len());
        if masked == UNAVAILABLE_MASK { return Err(DetectError); }
        Ok(duduclaw_core::secret_redact::redact_secrets(&masked).into_owned())
    })
}

fn mask_detected(text: &str, detected: Result<Vec<Finding>, DetectError>) -> String {
    match detected {
        Ok(findings) => {
            let masked = mask_with_ids(text, &findings, &[]);
            duduclaw_core::secret_redact::redact_secrets(&masked).into_owned()
        }
        Err(_) => UNAVAILABLE_MASK.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_and_traditional_chinese_contexts() {
        for text in ["password: violet-test", "pwd=test-only", "password is violet-test", "password=required", "密碼：無效",
            "密碼是測試值", "帳號 alice@example.com 密碼：violet-test", "token 是 test-token", "API key：test-key", "金鑰：test-key"] {
            assert!(!detect(text).unwrap().is_empty(), "{text}");
        }
    }

    #[test]
    fn prose_and_identifier_boundaries_are_not_credentials() {
        for text in ["password policy", "password policy: strict", "password is required",
            "newpassword: prose", "mytoken=prose", "API keyboard: prose", "bare-example-value"] {
            assert!(detect(text).unwrap().is_empty(), "{text}");
        }
    }

    #[test]
    fn cjk_boundaries_and_multibyte_values_are_masked() {
        assert_eq!(mask("你的密碼是測試值。謝謝"), "你的密碼是[已遮罩的密碼]。謝謝");
        assert_eq!(mask("請用password:測試值，登入"), "請用password:[已遮罩的密碼]，登入");
    }

    #[test]
    fn account_hint_keeps_only_first_character_and_domain() {
        assert_eq!(mask_account_hint("alice@example.com"), "a***@example.com");
        assert_eq!(mask_account_hint("王小明"), "王***");
        assert_eq!(mask_account_hint("alice@invalid/secret"), "a***");
    }

    #[test]
    fn shapes_include_google_aws_pem_and_connection_strings() {
        for value in [
            ["AIza", &"x".repeat(30)].concat(),
            ["AKIA", &"A".repeat(16)].concat(),
            ["ya29.", &"x".repeat(30)].concat(),
            ["GOCSPX-", &"x".repeat(30)].concat(),
            "-----BEGIN PRIVATE KEY-----\nfixture\n-----END PRIVATE KEY-----".into(),
            "postgres://alice:fixture@localhost/db".into(),
        ] {
            assert!(!detect(&value).unwrap().is_empty());
            assert!(!mask(&value).contains(&value));
        }
    }

    #[test]
    fn masking_does_not_stop_at_ledger_quota() {
        let text = (0..9).map(|i| format!("password: fixture{i}")).collect::<Vec<_>>().join("\n");
        assert_eq!(detect(&text).unwrap().len(), 9);
        assert!(!mask(&text).contains("fixture"));
    }

    #[test]
    fn mask_mode_uses_credential_id_without_value() {
        let text = "password: fixture";
        assert_eq!(mask_with_ids(text, &detect(text).unwrap(), &["cred_test".into()]),
            "password: [已遮罩的密碼 cred_test]");
    }

    #[test]
    fn oversized_or_invalid_spans_fail_closed() {
        assert_eq!(mask(&"密".repeat(MAX_SCAN_BYTES)), UNAVAILABLE_MASK);
        let mut findings = detect("密碼：fixture").unwrap();
        findings[0].span = 1..2;
        assert_eq!(mask_with_ids("密碼：fixture", &findings, &[]), UNAVAILABLE_MASK);
    }
    #[test]
    fn detector_error_and_panic_mask_entire_message() {
        let text = "unstructured fixture";
        assert_eq!(mask_detected(text, guarded_detect(|| Err(DetectError))), UNAVAILABLE_MASK);
        assert_eq!(mask_detected(text, guarded_detect(|| panic!("injected detector failure"))), UNAVAILABLE_MASK);
    }

    #[test]
    fn overlapping_shape_and_context_masks_whole_value() {
        let secret = ["sk-", &"a".repeat(20), "測試"].concat();
        let text = format!("password: {secret}");
        assert_eq!(mask(&text), "password: [已遮罩的密碼]");
    }

}

#[cfg(test)]
mod entropy_precedence_tests {
    use super::*;

    #[test]
    fn password_context_never_inherits_high_entropy_shape_kind() {
        let value = ["sk-", &"x".repeat(40), "!suffix"].concat();
        for input in [format!("password: {value}"), format!("密碼是{value}")] {
            let findings = detect(&input).unwrap();
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].kind, "password");
            assert!(!mask(&input).contains("suffix"));
        }
    }
}

#[cfg(test)]
mod adjacent_shape_tests {
    use super::*;

    #[test]
    fn adjacent_aws_keys_are_detected_without_consuming_shared_boundary() {
        let first = ["AKIA", &"A".repeat(16)].concat();
        let second = ["ASIA", &"B".repeat(16)].concat();
        let text = format!("{first} {second}");
        assert_eq!(detect(&text).unwrap().len(), 2);
        assert!(!mask(&text).contains(&first));
        assert!(!mask(&text).contains(&second));
        assert!(detect(&format!("{first}EXTRA")).unwrap().is_empty());
    }
}

// Security review regressions: written but not run (owner prohibits builds).
#[cfg(test)]
mod fingerprint_evidence_tests {
    use super::*;

    #[test]
    fn context_and_incomplete_structures_are_never_fingerprint_eligible() {
        for text in [
            format!("api key: {}", "guessable".repeat(8)),
            format!("token: {}", "guessable".repeat(8)),
            format!("Bearer {}", "guessable".repeat(8)),
            format!("password: sk-{}", "x".repeat(40)),
            "-----BEGIN PRIVATE KEY-----\nfixture".into(),
            "-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----".into(),
            format!("api key: sk-{}!suffix", "x".repeat(40)),
        ] {
            let findings = detect(&text).unwrap();
            assert!(!findings.is_empty());
            assert!(findings.iter().all(|finding| !finding.fingerprint_eligible), "{text}");
        }
    }

    #[test]
    fn only_complete_provider_or_pem_structures_are_eligible() {
        use base64::Engine as _;
        let body = base64::engine::general_purpose::STANDARD.encode([42; 64]);
        for text in [format!("api key: sk-{}", "x".repeat(40)),
            format!("-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----"),
            format!("AIza{}", "x".repeat(35))] {
            let findings = detect(&text).unwrap();
            assert_eq!(findings.len(), 1);
            assert!(findings[0].fingerprint_eligible);
        }
    }

    #[test]
    fn json_sensitive_names_preserve_typed_containers_and_numbers() {
        let mut data = serde_json::json!({"token":{"value":"password: fixture","at":1},"nested":{"token":17}});
        mask_json(&mut data);
        assert!(data["token"].is_object());
        assert_eq!(data["token"]["at"], 1);
        assert_eq!(data["nested"]["token"], 17);
        assert!(!data.to_string().contains("fixture"));
    }

    #[test]
    fn serialized_json_credential_labels_keep_their_context() {
        for raw in [r#"{"password":"fixture secret"}"#, r#"{"api_key":"fixture"}"#, r#"{"token":"fixture"}"#] {
            assert!(!mask(raw).contains("fixture"));
        }
    }

    #[test]
    fn json_masks_context_fields_dynamic_keys_and_nested_values() {
        let mut data = serde_json::json!({"password":"fixture", "nested":[{"api_key":"fixture"}], "password: fixture":"ordinary"});
        mask_json(&mut data);
        assert!(!data.to_string().contains("fixture"));
        let mut collision = serde_json::json!({"password: one":1,"password: two":2});
        mask_json(&mut collision);
        assert_eq!(collision, serde_json::json!({}));
    }
}
