//! Credential redaction for operator-visible diagnostics.
//!
//! Moved here from `duduclaw-gateway::secret_redact` (2026-10, maintenance SSH
//! P-2) so that a binary that must not link the gateway can share the exact
//! same rules. `duduclaw-gateway::secret_redact` re-exports this module, so
//! every existing `crate::secret_redact::redact_secrets` call site keeps
//! compiling with unchanged behaviour. The callers today:
//!
//! - **Gateway channel diagnostics.** Channel bots surface raw transport
//!   errors to three places that a human (or a screenshot in a support
//!   ticket) can read: the dashboard channel roster (`channels.status` /
//!   `channels.status_changed`), `~/.duduclaw/channel_status.json` (the
//!   out-of-process snapshot the `channel_status` MCP tool reads), and the
//!   gateway log.
//! - **`duduclaw-maint-helper`** (crate `duduclaw-maint-tools`), the root
//!   service that hands the maintenance SSH account a filtered view of the
//!   gateway and local-model journal. Every `MESSAGE` it returns goes through
//!   [`redact_secrets`].
//!
//! Several channel APIs put the credential **in the URL** (Telegram's
//! `/bot<token>/getMe`, WeCom's `?corpsecret=`, DingTalk's `?appsecret=`), so a
//! bare `reqwest::Error::to_string()` prints a working bot token. Everything
//! that reaches those sinks must go through [`redact_secrets`] first.
//!
//! The redactor is deliberately **shape-driven and dependency-free** (no regex
//! crate in this crate's tree): it only rewrites spans that match a known
//! credential shape, so ordinary diagnostics (`dns error`, `connection
//! refused`, host names, CJK text) survive byte-identical.

use std::borrow::Cow;
use std::ops::Range;

/// Complete, value-free display markers shared by the masking layers.
/// Match the marker grammar, never arbitrary bracketed text containing secrets.
pub fn redaction_placeholder_ranges(input: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut cursor = 0;
    for (start, c) in input.char_indices() {
        if start < cursor || !matches!(c, '[' | '〔') { continue; }
        let tail = &input[start..];
        let len = if tail.starts_with("[REDACTED]") {
            Some("[REDACTED]".len())
        } else if tail.starts_with("[憑證偵測不可用，已遮罩此訊息]") {
            Some("[憑證偵測不可用，已遮罩此訊息]".len())
        } else if let Some(body) = tail.strip_prefix("[已遮罩的") {
            body.find(']').and_then(|end| {
                let body = &body[..end];
                let valid = ["密碼", " API key", "權杖", "私密金鑰", "憑證", "連線字串", "Cookie"].iter().any(|label| {
                    body.strip_prefix(label).is_some_and(|suffix| suffix.is_empty()
                        || suffix.strip_prefix(" cred_").is_some_and(|id| !id.is_empty()
                            && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')))
                });
                valid.then_some("[已遮罩的".len() + end + 1)
            })
        } else if let Some(body) = tail.strip_prefix("〔已遮蔽·") {
            body.find('〕').and_then(|end| {
                let category = &body[..end];
                (matches!(category, "secret" | "text") || (!category.is_empty() && category.len() <= 32
                    && category.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
                    && !has_secret_prefix(category) && !category.starts_with("AKIA") && !category.starts_with("ASIA")))
                    .then_some("〔已遮蔽·".len() + end + '〕'.len_utf8())
            })
        } else { None };
        if let Some(len) = len {
            cursor = start + len;
            ranges.push(start..cursor);
        }
    }
    ranges
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Mask an opaque secret: `first4***last4`, or `***` when the value is too
/// short to reveal anything safely.
///
/// Codepoint-based (never slices by raw byte index) so CJK/emoji input cannot
/// panic — see the project convention on string slicing.
pub fn mask_secret(s: &str) -> String {
    let n = s.chars().count();
    if n <= 8 {
        return "***".to_string();
    }
    format!("{}***{}", first_chars(s, 4), last_chars(s, 4))
}

/// Redact every credential-shaped span in a free-form diagnostic string.
///
/// Returns `Cow::Borrowed` when nothing matched, so the overwhelmingly common
/// "no secret in this message" case allocates nothing.
pub fn redact_secrets(input: &str) -> Cow<'_, str> {
    redact_secrets_with_count(input).0
}

/// Return the redacted text and the number of credential values replaced.
pub fn redact_secrets_with_count(input: &str) -> (Cow<'_, str>, usize) {
    if input.is_empty() {
        return (Cow::Borrowed(input), 0);
    }

    let mut count = 0;
    let mut out = String::new();
    let mut changed = false;
    let mut word_start: Option<usize> = None;
    let mut prev_word: Option<&str> = None;

    let mut placeholders = redaction_placeholder_ranges(input).into_iter().peekable();

    // Walk the string word-by-word, keeping the delimiters verbatim. A "word"
    // is any run of non-delimiter characters; URLs, bare tokens and
    // `Bearer <tok>` pairs are all recognised at that granularity.
    for (i, c) in input.char_indices() {
        if placeholders.peek().is_some_and(|span| i >= span.end) { placeholders.next(); }
        if let Some(span) = placeholders.peek().filter(|span| span.contains(&i)) {
            if i == span.start {
                if let Some(s) = word_start.take() {
                    push_word(&mut out, input, s, &input[s..i], prev_word, &mut changed, &mut count);
                }
                if changed { out.push_str(&input[span.clone()]); }
                prev_word = None;
            }
            continue;
        }
        if is_delim(c) {
            if let Some(s) = word_start.take() {
                let w = &input[s..i];
                push_word(&mut out, input, s, w, prev_word, &mut changed, &mut count);
                prev_word = Some(w);
            }
            if changed {
                out.push(c);
            }
        } else if word_start.is_none() {
            word_start = Some(i);
        }
    }
    if let Some(s) = word_start {
        let w = &input[s..];
        push_word(&mut out, input, s, w, prev_word, &mut changed, &mut count);
    }

    if changed {
        (Cow::Owned(out), count)
    } else {
        (Cow::Borrowed(input), 0)
    }
}

/// Convenience wrapper for the `Option<String>` error field carried by
/// `set_channel_connected`.
pub fn redact_opt(error: Option<String>) -> Option<String> {
    error.map(|e| redact_secrets(&e).into_owned())
}

// ── Word-level dispatch ─────────────────────────────────────────────────────

/// Append `word` (redacted if needed). Materialises the untouched prefix
/// lazily: `out` only starts accumulating once the first redaction happens.
fn push_word(
    out: &mut String,
    input: &str,
    word_start: usize,
    word: &str,
    prev_word: Option<&str>,
    changed: &mut bool,
    count: &mut usize,
) {
    let redacted = redact_word(word, prev_word, count);
    match redacted {
        Cow::Borrowed(_) if !*changed => {}
        Cow::Borrowed(w) => out.push_str(w),
        Cow::Owned(w) => {
            if !*changed {
                // First hit: replay everything before this word verbatim.
                out.push_str(&input[..word_start]);
                *changed = true;
            }
            out.push_str(&w);
        }
    }
}

/// Keywords that mark the *next* word as a **candidate** credential
/// (`Authorization: Bearer <tok>`, `token <tok>`).
///
/// `bot` and `basic` are deliberately NOT here. They are ordinary English words
/// in our own log lines ("Telegram bot connected", "Bot Token 無效"), and the one
/// credential they used to catch — Discord's `Bot <token>` header — is already
/// caught by [`is_dotted_token_shape`] on the token itself.
const AUTH_KEYWORDS: &[&str] = &["bearer", "token", "authorization"];

/// Whether a keyword-flagged word actually *looks* like an opaque credential.
///
/// A keyword alone is far too weak a signal: `token invalidated by user` and
/// `Telegram bot connected:` both put a plain English word after a keyword. A
/// real credential is a long, unbroken run of ASCII base64/base64url/JWT
/// characters — prose, CJK, and punctuation-bearing words all fail this.
fn looks_like_opaque_credential(w: &str) -> bool {
    w.is_ascii()
        && w.len() >= 20
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '/' | '='))
}

fn redact_word<'a>(word: &'a str, prev_word: Option<&str>, count: &mut usize) -> Cow<'a, str> {
    // 1. Anything URL-shaped: mask secret path segments + sensitive query params.
    if word.contains("://") {
        return mask_url(word, count);
    }

    // 2. `Bearer <tok>` / `token <tok>` pairs — keyword AND credential shape.
    if let Some(p) = prev_word {
        let key = p
            .trim_end_matches(':')
            .trim_end_matches('=')
            .to_ascii_lowercase();
        if AUTH_KEYWORDS.contains(&key.as_str()) && looks_like_opaque_credential(word) {
            let masked = mask_secret(word);
            *count += usize::from(masked != word);
            return Cow::Owned(masked);
        }
    }

    // 3. A bare Telegram bot token pasted into a message.
    if split_telegram_token(word).is_some() {
        let masked = mask_telegram_token(word);
        *count += usize::from(masked != word);
        return Cow::Owned(masked);
    }

    // 4. Vendor-prefixed credentials (Slack / GitHub / Anthropic / Meta / …).
    if has_secret_prefix(word) {
        let masked = mask_secret(word);
        *count += usize::from(masked != word);
        return Cow::Owned(masked);
    }

    // 5. Three-segment base64 credentials (Discord bot tokens, JWTs).
    if is_dotted_token_shape(word) {
        let masked = mask_secret(word);
        *count += usize::from(masked != word);
        return Cow::Owned(masked);
    }

    Cow::Borrowed(word)
}

// ── URL handling ────────────────────────────────────────────────────────────

/// Words that mark a query parameter as carrying a credential. Matched on
/// **whole words** after splitting the name on `_`, `-` and `.` — a substring
/// match would fire on `monkey` (key), `design` / `assign` (sign) and
/// `authority` (auth). Ambiguous words also require a credential-shaped value.
const SENSITIVE_PARAM_WORDS: &[&str] = &[
    "secret",
    "secrets",
    "token",
    "tokens",
    "password",
    "passwd",
    "pwd",
    "key",
    "keys",
    "sign",
    "signature",
    "credential",
    "credentials",
    "auth",
    "session",
];

/// Vendor parameter names that are a single unsplittable word, so the
/// word-equality rule above cannot see the marker inside them.
const SENSITIVE_PARAM_EXACT: &[&str] = &[
    "corpsecret",  // WeCom /cgi-bin/gettoken
    "appsecret",   // DingTalk /gettoken
    "appkey",      // DingTalk
    "accesskey",   // generic cloud vendors
    "secretkey",   //
    "apikey",      //
    "sessionkey",  //
    "accesstoken", //
    "authtoken",   //
];

fn is_sensitive_param(name: &str, value: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    if SENSITIVE_PARAM_EXACT.contains(&lower.as_str()) {
        return true;
    }
    lower.split(['_', '-', '.']).any(|word| {
        SENSITIVE_PARAM_WORDS.contains(&word)
            && (!matches!(word, "key" | "keys" | "sign" | "signature" | "auth" | "session")
                || looks_like_query_credential(value))
    }) || matches!(lower.as_str(), "api_key" | "api-key" | "api.key")
}

/// Ambiguous navigation parameters need more evidence than their name.
fn looks_like_query_credential(value: &str) -> bool {
    if has_secret_prefix(value) || is_dotted_token_shape(value) {
        return true;
    }
    let lower = value.chars().any(|c| c.is_ascii_lowercase());
    let upper = value.chars().any(|c| c.is_ascii_uppercase());
    let digit = value.chars().any(|c| c.is_ascii_digit());
    let symbol = value.chars().any(|c| matches!(c, '_' | '-' | '+' | '/' | '=' | '%'));
    value.len() >= 20
        && value.chars().all(|c| c.is_ascii_alphanumeric()
            || matches!(c, '_' | '-' | '+' | '/' | '=' | '%' | '.'))
        && [lower, upper, digit, symbol].into_iter().filter(|mixed| *mixed).count() >= 2
}

fn mask_url<'a>(url: &'a str, count: &mut usize) -> Cow<'a, str> {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };

    let mut out = String::with_capacity(url.len());
    let mut changed = false;

    for (i, seg) in base.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        match mask_path_segment(seg) {
            Cow::Borrowed(s) => out.push_str(s),
            Cow::Owned(s) => {
                *count += usize::from(s != seg);
                out.push_str(&s);
                changed = true;
            }
        }
    }

    if let Some(q) = query {
        out.push('?');
        for (i, pair) in q.split('&').enumerate() {
            if i > 0 {
                out.push('&');
            }
            match pair.split_once('=') {
                Some((k, v)) if !v.is_empty() && is_sensitive_param(k, v) => {
                    out.push_str(k);
                    out.push('=');
                    let masked = mask_secret(v);
                    *count += usize::from(masked != v);
                    out.push_str(&masked);
                    changed = true;
                }
                _ => out.push_str(pair),
            }
        }
    }

    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(url)
    }
}

fn mask_path_segment(seg: &str) -> Cow<'_, str> {
    // Telegram: `/bot<id>:<secret>/getMe`.
    if let Some(rest) = seg.strip_prefix("bot") {
        if split_telegram_token(rest).is_some() {
            return Cow::Owned(format!("bot{}", mask_telegram_token(rest)));
        }
    }
    if split_telegram_token(seg).is_some() {
        return Cow::Owned(mask_telegram_token(seg));
    }
    if has_secret_prefix(seg) || is_dotted_token_shape(seg) {
        return Cow::Owned(mask_secret(seg));
    }
    Cow::Borrowed(seg)
}

// ── Shape detectors ─────────────────────────────────────────────────────────

/// Split a Telegram bot token into `(bot_id, separator, secret)`.
///
/// Accepts BOTH the canonical `<digits>:<secret>` form and the corrupted
/// `<digits>-<secret>` form, so a malformed token is still recognised as a
/// credential and gets masked rather than printed in full.
pub(crate) fn split_telegram_token(s: &str) -> Option<(&str, char, &str)> {
    let idx = s.find([':', '-'])?;
    let (id, rest) = s.split_at(idx);
    let mut chars = rest.chars();
    let sep = chars.next()?;
    let secret = chars.as_str();
    let id_ok = (5..=16).contains(&id.len()) && id.chars().all(|c| c.is_ascii_digit());
    // A hyphen is also a UUID separator. Require Telegram's complete
    // secret length for this corrupted-token compatibility form.
    let minimum_secret_len = if sep == '-' { 35 } else { 20 };
    let secret_ok = secret.len() >= minimum_secret_len
        && secret
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if id_ok && secret_ok {
        Some((id, sep, secret))
    } else {
        None
    }
}

/// `7000***:***YZ12` — keeps the separator visible on purpose: a `-` instead
/// of `:` is itself the diagnostic signal for a corrupted token.
fn mask_telegram_token(s: &str) -> String {
    match split_telegram_token(s) {
        Some((id, sep, secret)) => {
            format!(
                "{}***{}***{}",
                first_chars(id, 4),
                sep,
                last_chars(secret, 4)
            )
        }
        None => mask_secret(s),
    }
}

/// Vendor prefixes that unambiguously mark a credential.
const SECRET_PREFIXES: &[&str] = &[
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxe-",
    "xoxs-",
    "xapp-", // Slack
    "sk-",   // OpenAI / Anthropic
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_", // GitHub
    "ddc_",        // DuDuClaw MCP API keys
    "EAA",         // Meta / WhatsApp Cloud access tokens
    "AIza",        // Google API keys
    "ya29.",       // Google OAuth access tokens
];

fn has_secret_prefix(w: &str) -> bool {
    w.chars().count() >= 12 && SECRET_PREFIXES.iter().any(|p| w.starts_with(p))
}

/// `<base64>.<base64>.<base64>` — Discord bot tokens and JWTs.
fn is_dotted_token_shape(w: &str) -> bool {
    let parts: Vec<&str> = w.split('.').collect();
    parts.len() == 3
        && parts[0].len() >= 20
        && parts[1].len() >= 5
        && parts[2].len() >= 20
        && parts.iter().all(|p| {
            p.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

// ── Small helpers ───────────────────────────────────────────────────────────

fn is_delim(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '"' | '\'' | ',' | ';'
        )
}

fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn last_chars(s: &str, n: usize) -> String {
    let total = s.chars().count();
    s.chars().skip(total.saturating_sub(n)).collect()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layered_placeholders_are_preserved_and_neighboring_secrets_are_masked() {
        for marker in ["[REDACTED]", "[已遮罩的 API key cred_test]", "[已遮罩的密碼]", "〔已遮蔽·secret〕"] {
            let text = format!("Bearer {marker} https://example.test/?api_key={marker}&offset=1");
            assert_eq!(redact_secrets(&text), text);
            let text = format!("{marker} {}", anthropic_key());
            let once = redact_secrets(&text);
            assert!(once.starts_with(marker));
            assert!(!once.contains("abcdefghijklmnopqrstuvwx"));
            assert_eq!(redact_secrets(&once), once);
        }
    }

    #[test]
    fn credential_shaped_categories_are_not_safe_placeholders() {
        let marker = format!("〔已遮蔽·AKIA{}〕", "A".repeat(16));
        assert!(redaction_placeholder_ranges(&marker).is_empty());
    }

    // ── Synthetic fixtures, assembled at run time ────────────────────────────
    //
    // Every credential below is fake, but a *fake* token with a real vendor
    // shape still trips GitHub push protection and every other source scanner —
    // and a blocked push is indistinguishable from a real leak until someone
    // reads the diff. So no contiguous vendor-shaped literal is allowed in this
    // file: each fixture is concatenated from fragments at run time. The values
    // the assertions see are identical, so the tests lose nothing.
    //
    // (The `SECRET_PREFIXES` table above is the detector's own definition, not a
    // fixture — those are bare prefixes with no secret body and stay as-is.)

    /// Telegram bot id.
    const TG_ID: &str = "7000000001";

    /// Telegram's 35-character URL-safe secret.
    fn tg_secret() -> String {
        ["AAExample", "Example", "Example", "Example", "XYZ12"].concat()
    }

    /// A full Telegram token. `sep` is `:` for the canonical form, `-` for the
    /// corrupted form WP12 was reported with.
    fn tg_token(sep: char) -> String {
        format!("{TG_ID}{sep}{}", tg_secret())
    }

    fn slack_token() -> String {
        ["xoxb", "-1234567890-", "abcdefghijklmnop"].concat()
    }

    fn anthropic_key() -> String {
        ["sk-", "ant-", "api03-", "abcdefghijklmnopqrstuvwx"].concat()
    }

    fn discord_token() -> String {
        [
            "MTIzNDU2Nzg5MDEyMzQ1Njc4",
            ".GhIjKl",
            ".abcdefghijklmnopqrstuvwxyz12",
        ]
        .concat()
    }

    fn jwt() -> String {
        [
            "eyJhbGciOiJIUzI1NiJ9",
            ".eyJzdWIiOiIxIn0",
            ".dBjftJeZ4CVPmB92K27uhbUJU1p1r",
        ]
        .concat()
    }

    /// The dashboard error shape reported in WP12. Note the corrupted `-`
    /// separator — it must still be recognised and masked.
    fn reported() -> String {
        format!(
            "error sending request for url (https://api.telegram.org/bot{}/getMe)",
            tg_token('-')
        )
    }

    #[test]
    fn reported_telegram_error_no_longer_leaks_the_token() {
        let reported = reported();
        let out = redact_secrets(&reported);
        assert!(
            !out.contains(&tg_secret()),
            "secret must not survive: {out}"
        );
        assert!(!out.contains(TG_ID), "full bot id must not survive: {out}");
        assert!(out.contains("bot7000***-***YZ12"), "unexpected mask: {out}");
        // Everything that is NOT the credential stays readable.
        assert!(out.starts_with("error sending request for url ("));
        assert!(out.contains("api.telegram.org"));
        assert!(out.ends_with("/getMe)"));
    }

    #[test]
    fn canonical_telegram_token_is_masked_and_keeps_the_colon() {
        let s = format!(
            "https://api.telegram.org/bot{}/getUpdates?offset=0",
            tg_token(':')
        );
        let out = redact_secrets(&s);
        assert!(out.contains("bot7000***:***YZ12"), "{out}");
        assert!(
            out.contains("getUpdates?offset=0"),
            "non-secret query kept: {out}"
        );
    }

    #[test]
    fn digit_led_uuid_is_preserved_but_complete_corrupted_telegram_tokens_are_masked() {
        let uuid = "12345678-abcd-4abc-abcd-123456789abc";
        assert!(split_telegram_token(uuid).is_none());
        for text in [uuid.to_string(), format!("task id {uuid}"),
            format!("https://example.test/tasks/{uuid}")] {
            assert_eq!(redact_secrets(&text), text);
        }
        assert_eq!(tg_secret().len(), 35);
        for separator in [':', '-'] {
            let token = tg_token(separator);
            assert!(split_telegram_token(&token).is_some());
            assert!(!redact_secrets(&token).contains(&tg_secret()));
        }
    }

    #[test]
    fn bare_telegram_token_in_prose_is_masked() {
        let input = format!("token {} is invalid", tg_token(':'));
        let out = redact_secrets(&input);
        assert!(!out.contains(&tg_secret()), "{out}");
    }

    #[test]
    fn wecom_and_dingtalk_query_secrets_are_masked() {
        let wecom = "https://qyapi.weixin.qq.com/cgi-bin/gettoken?corpid=ww1234567890&corpsecret=SsuperSecretValue1234567890abcdef";
        let out = redact_secrets(wecom);
        assert!(!out.contains("SsuperSecretValue1234567890abcdef"), "{out}");
        assert!(
            out.contains("corpid=ww1234567890"),
            "corpid is an identifier, kept: {out}"
        );

        let ding =
            "https://oapi.dingtalk.com/gettoken?appkey=abc&appsecret=zzzzzzzzzzzzzzzzzzzzzzzzz";
        let out = redact_secrets(ding);
        assert!(!out.contains("zzzzzzzzzzzzzzzzzzzzzzzzz"), "{out}");
    }

    #[test]
    fn slack_github_and_bearer_credentials_are_masked() {
        let input = format!("auth failed for {}", slack_token());
        let out = redact_secrets(&input);
        assert!(!out.contains("abcdefghijklmnop"), "{out}");

        let input = format!("header Authorization: Bearer {}", anthropic_key());
        let out = redact_secrets(&input);
        assert!(!out.contains("abcdefghijklmnopqrstuvwx"), "{out}");

        let input = format!("Bot {}", discord_token());
        let out = redact_secrets(&input);
        assert!(!out.contains("abcdefghijklmnopqrstuvwxyz12"), "{out}");
    }

    #[test]
    fn discord_token_shape_is_masked_without_a_keyword() {
        let input = format!("gateway closed: {}", discord_token());
        let out = redact_secrets(&input);
        assert!(!out.contains("abcdefghijklmnopqrstuvwxyz12"), "{out}");
    }

    #[test]
    fn ordinary_diagnostics_are_returned_byte_identical() {
        for s in [
            "dns error: failed to lookup address information: nodename nor servname provided",
            "connection refused (os error 61)",
            "not configured",
            "Telegram 通道尚未設定，請於儀表板填入 Bot Token",
            "https://api.telegram.org/健康檢查/getMe",
            "operation timed out after 35s",
        ] {
            assert!(
                matches!(redact_secrets(s), Cow::Borrowed(_)),
                "must not rewrite: {s}"
            );
            assert_eq!(redact_secrets(s), s);
        }
    }

    /// H1 regression — a keyword (`token`, `Bearer`, `Authorization`) must not
    /// be enough on its own to mangle the word after it. Every string here is
    /// real operator-facing text this crate emits.
    #[test]
    fn keyword_adjacent_prose_is_returned_byte_identical() {
        for s in [
            // Our own zh-TW / English channel diagnostics.
            "Bot Token 無效（Unauthorized）— 請在儀表板重新填入",
            "Telegram bot connected: @dudu_assistant (label: telegram)",
            "token invalidated by user",
            "Telegram getMe rejected for telegram:ceo: Unauthorized",
            "telegram bot_token not configured",
            "Bot Token is configured",
            "slack_bot_token missing — check the Dashboard",
            "Authorization required",
            "Bearer authentication failed",
            "token expired, refreshing",
            "channels.telegram removed",
            // Non-ASCII right after a keyword.
            "token 無效",
            "Bearer 憑證已過期，請重新授權一次",
        ] {
            assert!(
                matches!(redact_secrets(s), Cow::Borrowed(_)),
                "must not rewrite: {s}"
            );
            assert_eq!(redact_secrets(s), s);
        }
    }

    /// H1 counterpart — the shape gate must not weaken real credential capture.
    #[test]
    fn keyword_plus_credential_shape_still_masks() {
        let cases = [
            (
                format!("Authorization: Bearer {}", anthropic_key()),
                "abcdefghijklmnopqrstuvwx".to_string(),
            ),
            (format!("token {} rejected", tg_secret()), tg_secret()),
            (
                format!("Bearer {}", jwt()),
                "dBjftJeZ4CVPmB92K27uhbUJU1p1r".to_string(),
            ),
        ];
        for (input, secret) in cases {
            let out = redact_secrets(&input);
            assert!(!out.contains(&secret), "credential survived: {out}");
        }
    }

    /// M5 regression — query-parameter names are matched on whole words.
    #[test]
    fn innocuous_query_params_containing_marker_substrings_are_kept() {
        let url = "https://example.com/api?monkey=curious&design=flat&assign=bob&authority=local&offset=42&keyboard=qwerty";
        let out = redact_secrets(url);
        assert_eq!(out, url, "no value should have been masked: {out}");
    }

    #[test]
    fn ambiguous_query_params_preserve_navigation_values() {
        for query in ["session=overview", "key=value", "sign=up"] {
            let url = format!("https://example.com/?{query}");
            assert_eq!(redact_secrets(&url), url);
        }
        for name in ["session", "key", "sign", "signature", "auth"] {
            let url = format!("https://example.com/?{name}=AbCdEf0123456789abcdef");
            assert!(!redact_secrets(&url).contains("AbCdEf0123456789abcdef"));
        }
        for name in ["password", "token", "api_key", "access_token", "client_secret"] {
            let url = format!("https://example.com/?{name}=short");
            assert!(!redact_secrets(&url).contains("short"));
        }
    }

    #[test]
    fn vendor_credential_params_are_still_masked() {
        for (url, secret) in [
            (
                "https://x/y?corpsecret=SsuperSecretValue1234567890",
                "SsuperSecretValue1234567890",
            ),
            (
                "https://x/y?appsecret=SsuperSecretValue1234567890",
                "SsuperSecretValue1234567890",
            ),
            (
                "https://x/y?access_token=SsuperSecretValue1234567890",
                "SsuperSecretValue1234567890",
            ),
            (
                "https://x/y?api-key=SsuperSecretValue1234567890",
                "SsuperSecretValue1234567890",
            ),
            (
                "https://x/y?client.secret=SsuperSecretValue1234567890",
                "SsuperSecretValue1234567890",
            ),
        ] {
            let out = redact_secrets(url);
            assert!(!out.contains(secret), "credential survived: {out}");
        }
    }

    #[test]
    fn mask_secret_is_cjk_safe_and_never_panics() {
        assert_eq!(mask_secret(""), "***");
        assert_eq!(mask_secret("abc"), "***");
        assert_eq!(mask_secret("12345678"), "***");
        assert_eq!(mask_secret("123456789"), "1234***6789");
        // Multi-byte input must not panic on a mid-char byte index.
        let cjk = "密碼密碼密碼密碼密碼密碼";
        assert_eq!(mask_secret(cjk), "密碼密碼***密碼密碼");
    }

    #[test]
    fn redaction_is_idempotent() {
        let once = redact_secrets(&reported()).into_owned();
        let twice = redact_secrets(&once).into_owned();
        assert_eq!(once, twice);
    }

    #[test]
    fn short_hyphenated_numbers_are_not_mistaken_for_tokens() {
        // A date or an id must not be mangled.
        assert_eq!(redact_secrets("2026-08-04"), "2026-08-04");
        assert_eq!(redact_secrets("12345-abc"), "12345-abc");
    }
}
