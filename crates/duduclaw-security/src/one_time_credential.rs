//! Deterministic masking for login links and one-time codes in mail text.
use regex::Regex;
use std::sync::OnceLock;

const LINK_NOTICE: &str = "[已移除 1 個登入連結]";
const CODE_NOTICE: &str = "[已移除 1 組驗證碼]";
const FORCED_PARAMETER: &str = r"(?i)(?:[?&#])(?:code|otp|key|sig|[^?&#=]*token[^?&#=]*)=";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stripped {
    pub text: String,
    pub links: usize,
    pub codes: usize,
}

fn link_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)https?://[^\s<>"']+"#).expect("valid URL regex"))
}

fn code_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(?P<code>[0-9]{3}[ -][0-9]{3}|[0-9]{4,8}|[A-Z0-9]{6,8})").expect("valid code regex"))
}

fn keyword_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)驗證碼|認證碼|確認碼|動態密碼|一次性|安全碼|\bOTP\b|passcode|verification\s+code|security\s+code|login\s+code|your\s+code|one-time").expect("valid keyword regex"))
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '+' | '/')
}

fn long_mixed_token(value: &str) -> bool {
    value.chars().count() >= 16
        && value.chars().any(|c| c.is_ascii_alphabetic())
        && value.chars().any(|c| c.is_ascii_digit())
        && value.chars().all(is_token_char)
}

fn suspicious_url(url: &str) -> bool {
    static PARAMETER: OnceLock<Regex> = OnceLock::new();
    let Some(scheme_end) = url.find("://") else { return false };
    let rest = &url[scheme_end + 3..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let path_query_fragment = &rest[authority_end..];
    let lower = path_query_fragment.to_ascii_lowercase();
    let marker = ["reset", "verify", "verification", "confirm", "magic", "token", "login", "signin", "sign-in", "activate", "unsubscribe", "otp", "auth"]
        .iter().any(|word| lower.contains(word));
    let mut long_value = false;
    for part in path_query_fragment.split(['/', '?', '#', '&', '=']) {
        if long_mixed_token(part) { long_value = true; }
    }
    let forced_parameter = PARAMETER.get_or_init(|| Regex::new(FORCED_PARAMETER).expect("valid parameter regex"))
        .is_match(path_query_fragment);
    forced_parameter || (marker && long_value)
}

/// Remove login links and likely one-time codes without performing any I/O.
pub fn strip_one_time_credentials(text: &str) -> Stripped {
    let mut links = 0;
    let linked = link_regex().replace_all(text, |caps: &regex::Captures<'_>| {
        if suspicious_url(&caps[0]) { links += 1; LINK_NOTICE.to_string() } else { caps[0].to_string() }
    }).into_owned();

    let lines: Vec<&str> = linked.split_inclusive('\n').collect();
    let mut offsets = Vec::with_capacity(lines.len());
    let mut offset = 0;
    for line in &lines { offsets.push(offset); offset += line.len(); }
    let mut keyword_lines = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if keyword_regex().is_match(line) {
            keyword_lines.push(i..=(i + 3).min(lines.len().saturating_sub(1)));
            keyword_lines.push(i.saturating_sub(3)..=i);
        }
    }
    let mut code_spans = Vec::new();
    for captures in code_regex().captures_iter(&linked) {
        let Some(m) = captures.name("code") else { continue };
        if m.as_str().len() >= 6 && m.as_str().len() <= 8
            && m.as_str().bytes().all(|b| b.is_ascii_alphanumeric())
            && (!m.as_str().bytes().any(|b| b.is_ascii_digit())
                || !m.as_str().bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())) {
            continue;
        }
        let before = &linked[..m.start()];
        let after = &linked[m.end()..];
        if before.chars().last().is_some_and(|c| c.is_ascii_alphanumeric())
            || after.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        {
            continue;
        }
        let line_index = offsets.partition_point(|start| *start <= m.start()).saturating_sub(1);
        if keyword_lines.iter().any(|range| range.contains(&line_index)) {
            code_spans.push(m.start()..m.end());
        }
    }
    let mut output = String::with_capacity(linked.len());
    let mut cursor = 0;
    for span in code_spans.iter() {
        output.push_str(&linked[cursor..span.start]);
        output.push_str(CODE_NOTICE);
        cursor = span.end;
    }
    output.push_str(&linked[cursor..]);
    Stripped { text: output, links, codes: code_spans.len() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_code_near_chinese_keyword_same_line_and_three_lines_above_blank_line() {
        let same = strip_one_time_credentials("您的驗證碼是 1234");
        assert_eq!(same.codes, 1);
        assert!(!same.text.contains("1234"));
        let nearby = strip_one_time_credentials("安全碼\n說明\n\n請輸入 987654");
        assert_eq!(nearby.codes, 1);
        assert!(!nearby.text.contains("987654"));
    }

    #[test]
    fn masks_supported_code_shapes_and_counts_two() {
        let result = strip_one_time_credentials("OTP: 123 456 and login code ABC12Z");
        assert_eq!(result.codes, 2);
        assert!(!result.text.contains("123 456"));
        assert!(!result.text.contains("ABC12Z"));
    }

    #[test]
    fn masks_query_and_fragment_credentials_regardless_of_value_length() {
        for separator in ["?", "#", "?article=yes&", "#article=yes&"] {
            for name in ["code", "token", "otp", "key", "sig", "access_token", "refresh_TOKEN", "mytokenvalue"] {
                for value in ["", "a", "abc"] {
                    let url = format!("https://example.test/path{separator}{name}={value}");
                    assert_eq!(strip_one_time_credentials(&url).links, 1, "{url}");
                }
            }
        }
        let article = "https://example.test/article?topic=rust#section=introduction";
        assert_eq!(strip_one_time_credentials(article).text, article);
    }

    #[test]
    fn masks_sensitive_links_but_keeps_article_url() {
        for url in [
            "https://example.test/reset/AbCdEf0123456789",
            "https://example.test/path?token=abc",
            "https://example.test/path#access_token=abc",
            "https://example.test/magic/AbCdEf0123456789",
        ] {
            let result = strip_one_time_credentials(url);
            assert_eq!(result.links, 1, "{url}");
            assert!(!result.text.contains(url));
        }
        let article = strip_one_time_credentials("https://example.test/article/interesting-story");
        assert_eq!(article.links, 0);
        assert!(article.text.contains("https://example.test/article/interesting-story"));
    }

    #[test]
    fn leaves_unlabelled_order_number_and_handles_unicode() {
        let result = strip_one_time_credentials("訂單 12345678 😀 中文內容");
        assert_eq!(result.codes, 0);
        assert!(result.text.contains("12345678"));
        assert!(result.text.contains("😀 中文內容"));
    }
}
