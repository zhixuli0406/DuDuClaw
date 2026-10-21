//! Distributor-notice HTML sanitizer: the single source of truth.
//!
//! This file is compiled twice: as `crate::distributor_notice::sanitize` in the
//! gateway library (runtime re-sanitization, defence in depth) and by the
//! gateway `build.rs` through `#[path]` (build-time sanitization of the
//! distributor build input). It must therefore depend only on `ammonia`, `url`
//! and `crate::image_data`, which both compilation units provide.
//!
//! Policy (the About-page notice is rendered inside a script-less sandboxed
//! iframe as a second layer):
//! - Raw input over [`MAX_NOTICE_BYTES`] is refused, never truncated. The
//!   sanitized output must also fit (sanitizing adds `rel`/`target` and may
//!   grow), so the runtime pass over build-time output can never refuse it.
//! - Only [`ALLOWED_TAGS`] survive; `<script>` / `<style>` lose their content.
//! - No generic attributes (`style`, `class`, `id`, `on*` are all dropped).
//! - `a[href]` must parse as an absolute `http`, `https` or `mailto` URL and
//!   every link is stamped `rel="nofollow noopener noreferrer"
//!   target="_blank"`.
//! - `img[src]` must be a PNG / JPEG / WebP base64 data URI whose bytes match
//!   the declared type, at most [`MAX_NOTICE_IMAGE_BYTES`] decoded.
//! - Comments are stripped and relative URLs are denied.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;

/// Raw (pre-sanitization) size ceiling for the whole notice document.
pub const MAX_NOTICE_BYTES: usize = 64 * 1024;
/// Decoded size ceiling for one embedded image.
pub const MAX_NOTICE_IMAGE_BYTES: usize = 512 * 1024;
/// `rel` value forced onto every surviving link.
pub const NOTICE_LINK_REL: &str = "nofollow noopener noreferrer";

/// Elements that survive sanitization.
pub const ALLOWED_TAGS: &[&str] = &[
    "p", "br", "h1", "h2", "h3", "h4", "ul", "ol", "li", "strong", "em", "u", "s", "a",
    "blockquote", "code", "pre", "hr", "img", "div", "span", "table", "thead", "tbody", "tr",
    "th", "td",
];

/// Per-element attributes that survive (values are further filtered).
const ALLOWED_ATTRIBUTES: &[(&str, &[&str])] = &[("a", &["href"]), ("img", &["src", "alt"])];

/// Why a notice was refused outright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeError {
    /// The raw document exceeds [`MAX_NOTICE_BYTES`].
    TooLarge { bytes: usize },
    /// The sanitized output exceeds [`MAX_NOTICE_BYTES`] (links gain `rel`
    /// and `target`, so output can be larger than the input).
    TooLargeAfterSanitizing { bytes: usize },
    /// Nothing displayable remains after sanitization.
    Empty,
}

impl fmt::Display for NoticeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { bytes } => write!(
                f,
                "distributor notice is {bytes} bytes; the limit is {} KB",
                MAX_NOTICE_BYTES / 1024
            ),
            Self::TooLargeAfterSanitizing { bytes } => write!(
                f,
                "distributor notice grows to {bytes} bytes after sanitization; the limit is {} KB",
                MAX_NOTICE_BYTES / 1024
            ),
            Self::Empty => write!(f, "distributor notice has no content left after sanitization"),
        }
    }
}

/// Sanitized output plus an informational list of what was dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoticeReport {
    /// The sanitized HTML (trimmed, never empty).
    pub html: String,
    /// Element names that are not on the allowlist (e.g. `script`).
    pub removed_elements: BTreeSet<String>,
    /// `element[attribute]` pairs dropped from allowed elements.
    pub removed_attributes: BTreeSet<String>,
    /// Link or image URLs refused by the URL policy (shortened).
    pub rejected_urls: Vec<String>,
}

impl NoticeReport {
    /// True when sanitization dropped anything worth telling the operator.
    pub fn has_removals(&self) -> bool {
        !self.removed_elements.is_empty()
            || !self.removed_attributes.is_empty()
            || !self.rejected_urls.is_empty()
    }
}

/// Sanitize a notice and return only the HTML.
pub fn sanitize_notice_html(raw: &str) -> Result<String, NoticeError> {
    sanitize_notice(raw).map(|report| report.html)
}

/// Sanitize a notice and report what the allowlist removed.
pub fn sanitize_notice(raw: &str) -> Result<NoticeReport, NoticeError> {
    if raw.len() > MAX_NOTICE_BYTES {
        return Err(NoticeError::TooLarge { bytes: raw.len() });
    }
    let cleaned = notice_builder().clean(raw).to_string();
    let html = cleaned.trim();
    if html.is_empty() {
        return Err(NoticeError::Empty);
    }
    if html.len() > MAX_NOTICE_BYTES {
        return Err(NoticeError::TooLargeAfterSanitizing { bytes: html.len() });
    }
    let (removed_elements, removed_attributes, rejected_urls) = removal_summary(raw);
    Ok(NoticeReport {
        html: html.to_string(),
        removed_elements,
        removed_attributes,
        rejected_urls,
    })
}

/// Accept an `a[href]`: an absolute http(s) URL with a host and no embedded
/// credentials, or `mailto:`. Returns the normalized URL so browser-side
/// parsing cannot differ.
pub fn notice_link(value: &str) -> Option<String> {
    let url = url::Url::parse(value.trim()).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let allowed = match url.scheme() {
        "http" | "https" => url.host_str().is_some_and(|host| !host.is_empty()),
        "mailto" => !url.path().is_empty(),
        _ => false,
    };
    allowed.then(|| url.to_string())
}

/// Accept an `img[src]`: a bounded, magic-byte-checked raster data URI.
pub fn notice_image_ok(value: &str) -> bool {
    crate::image_data::validate_image_data_uri(value, MAX_NOTICE_IMAGE_BYTES, "Image").is_ok()
}

fn notice_builder() -> ammonia::Builder<'static> {
    let tags: HashSet<&'static str> = ALLOWED_TAGS.iter().copied().collect();
    let tag_attributes: HashMap<&'static str, HashSet<&'static str>> = ALLOWED_ATTRIBUTES
        .iter()
        .map(|(tag, attrs)| (*tag, attrs.iter().copied().collect()))
        .collect();
    let forced_link_values: HashMap<&'static str, HashMap<&'static str, &'static str>> =
        HashMap::from([("a", HashMap::from([("target", "_blank")]))]);

    let mut builder = ammonia::Builder::default();
    builder
        .tags(tags)
        .tag_attributes(tag_attributes)
        .set_tag_attribute_values(forced_link_values)
        .generic_attributes(HashSet::new())
        .url_schemes(HashSet::from(["http", "https", "mailto", "data"]))
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(Some(NOTICE_LINK_REL))
        .strip_comments(true)
        .attribute_filter(|element, attribute, value| match (element, attribute) {
            ("a", "href") => notice_link(value).map(Cow::Owned),
            ("img", "src") => notice_image_ok(value).then_some(Cow::Borrowed(value)),
            _ => Some(Cow::Borrowed(value)),
        });
    builder
}

/// Short, log-safe description of a refused URL (data URIs are not echoed).
fn describe_rejected(tag: &str, attr: &str, value: &str) -> String {
    let lowered = value.trim_start().to_ascii_lowercase();
    let shown: String = if lowered.starts_with("data:") {
        "data URI".to_string()
    } else {
        value.chars().take(80).collect()
    };
    format!("{tag}[{attr}] {shown}")
}

fn allowed_attribute(tag: &str, attribute: &str) -> bool {
    // `a[rel]` / `a[target]` are always rewritten to the forced values, so an
    // already-sanitized notice (e.g. a console export) is not reported as lossy.
    if tag == "a" && (attribute == "rel" || attribute == "target") {
        return true;
    }
    ALLOWED_ATTRIBUTES
        .iter()
        .any(|(t, attrs)| *t == tag && attrs.contains(&attribute))
}

type Removals = (BTreeSet<String>, BTreeSet<String>, Vec<String>);

/// Informational only (the ammonia pass above is the security boundary):
/// list disallowed elements, dropped attributes and refused URLs in `raw`.
/// Values are read verbatim (entities are not decoded), so the report can
/// differ from ammonia on entity-encoded URLs.
fn removal_summary(raw: &str) -> Removals {
    let mut elements = BTreeSet::new();
    let mut attributes = BTreeSet::new();
    let mut urls = Vec::new();
    for (tag, attrs) in scan_start_tags(raw) {
        if !ALLOWED_TAGS.contains(&tag.as_str()) {
            elements.insert(tag);
            continue;
        }
        for (attr, value) in attrs {
            if !allowed_attribute(&tag, &attr) {
                attributes.insert(format!("{tag}[{attr}]"));
                continue;
            }
            let value = value.unwrap_or_default();
            let refused = match (tag.as_str(), attr.as_str()) {
                ("a", "href") => notice_link(&value).is_none(),
                ("img", "src") => !notice_image_ok(&value),
                _ => false,
            };
            if refused {
                urls.push(describe_rejected(&tag, &attr, &value));
            }
        }
    }
    (elements, attributes, urls)
}

type ScannedTag = (String, Vec<(String, Option<String>)>);

/// Minimal start-tag lexer: `(lowercase tag, [(lowercase attribute, value)])`.
/// Skips comments, end tags and the bodies of `<script>` / `<style>`.
fn scan_start_tags(raw: &str) -> Vec<ScannedTag> {
    let cs: Vec<char> = raw.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] != '<' {
            i += 1;
            continue;
        }
        if starts_at(&cs, i, "<!--") {
            i = find_from(&cs, i + 4, "-->").map_or(cs.len(), |end| end + 3);
            continue;
        }
        if !cs.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic()) {
            i += 1;
            continue;
        }
        let (tag, attrs, next) = read_start_tag(&cs, i + 1);
        i = next;
        if tag == "script" || tag == "style" {
            let close = format!("</{tag}");
            i = find_from_ci(&cs, i, &close).unwrap_or(cs.len());
        }
        out.push((tag, attrs));
    }
    out
}

fn read_start_tag(cs: &[char], mut j: usize) -> (String, Vec<(String, Option<String>)>, usize) {
    let start = j;
    while j < cs.len() && (cs[j].is_ascii_alphanumeric() || cs[j] == '-') {
        j += 1;
    }
    let tag = cs[start..j].iter().collect::<String>().to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while j < cs.len() && (cs[j].is_whitespace() || cs[j] == '/') {
            j += 1;
        }
        if j >= cs.len() {
            break;
        }
        if cs[j] == '>' {
            j += 1;
            break;
        }
        let name_start = j;
        while j < cs.len() && !cs[j].is_whitespace() && !matches!(cs[j], '=' | '>' | '/') {
            j += 1;
        }
        if j == name_start {
            j += 1;
            continue;
        }
        let name = cs[name_start..j].iter().collect::<String>().to_ascii_lowercase();
        let (value, next) = read_attribute_value(cs, j);
        attrs.push((name, value));
        j = next;
    }
    (tag, attrs, j)
}

fn read_attribute_value(cs: &[char], mut j: usize) -> (Option<String>, usize) {
    while j < cs.len() && cs[j].is_whitespace() {
        j += 1;
    }
    if j >= cs.len() || cs[j] != '=' {
        return (None, j);
    }
    j += 1;
    while j < cs.len() && cs[j].is_whitespace() {
        j += 1;
    }
    let (start, end, next) = match cs.get(j) {
        Some(&quote) if quote == '"' || quote == '\'' => {
            let start = j + 1;
            let mut end = start;
            while end < cs.len() && cs[end] != quote {
                end += 1;
            }
            (start, end, (end + 1).min(cs.len()))
        }
        _ => {
            let start = j;
            let mut end = start;
            while end < cs.len() && !cs[end].is_whitespace() && cs[end] != '>' {
                end += 1;
            }
            (start, end, end)
        }
    };
    (Some(cs[start..end].iter().collect()), next)
}

fn starts_at(cs: &[char], at: usize, needle: &str) -> bool {
    let needle: Vec<char> = needle.chars().collect();
    cs.get(at..at + needle.len()) == Some(needle.as_slice())
}

fn find_from(cs: &[char], from: usize, needle: &str) -> Option<usize> {
    (from..cs.len()).find(|&k| starts_at(cs, k, needle))
}

fn find_from_ci(cs: &[char], from: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    (from..cs.len()).find(|&k| {
        cs.get(k..k + needle.len()).is_some_and(|window| {
            window
                .iter()
                .zip(&needle)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        })
    })
}
