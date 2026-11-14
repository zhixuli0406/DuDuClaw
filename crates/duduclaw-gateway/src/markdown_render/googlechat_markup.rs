//! Google Chat text markup is live: `<users/123>` / `<users/all>` mention
//! and notify people, `<url|text>` is a link. Only the renderer itself may
//! emit markup; every piece of model / user text (paragraphs, list items,
//! headings, table titles and cells, link text) passes through here first.

/// Untrusted text made inert: angle brackets become their full-width
/// look-alikes, so no `<…>` construct (mention, link) can start or end in it.
/// `|` outside a link has no meaning in Chat and is kept as is.
pub(super) fn neutralize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '<' => '＜',
            '>' => '＞',
            other => other,
        })
        .collect()
}

/// Link text additionally may not contain the `|` delimiter.
fn neutralize_link_text(s: &str) -> String {
    neutralize(s).replace('|', "｜")
}

/// The URL as given, when it is an absolute `http` / `https` URL that cannot
/// break out of the `<url|text>` construct; `None` otherwise.
fn safe_link_url(raw: &str) -> Option<&str> {
    if raw.is_empty()
        || raw
            .chars()
            .any(|c| matches!(c, '<' | '>' | '|') || c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let parsed = reqwest::Url::parse(raw).ok()?;
    matches!(parsed.scheme(), "http" | "https").then_some(raw)
}

/// A markdown link `[text](url)` for Google Chat. Only a safe http(s) URL
/// becomes renderer-emitted `<url|text>` markup; anything else (another
/// scheme, a URL with delimiters, empty link text) is plain, inert text.
pub(super) fn link(text: &str, url: &str) -> String {
    match safe_link_url(url) {
        Some(url) if !text.trim().is_empty() => {
            format!("<{url}|{}>", neutralize_link_text(text))
        }
        // A bare URL is auto-linked by Chat itself; no markup needed.
        Some(url) => url.to_string(),
        None if text == url || text.trim().is_empty() => neutralize(url),
        None => format!("{} ({})", neutralize(text), neutralize(url)),
    }
}
