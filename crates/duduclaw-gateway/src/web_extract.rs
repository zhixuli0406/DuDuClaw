//! CSS selector-based content extraction for the browser automation L2 layer.
//!
//! Provides structured extraction of HTML elements using CSS selectors,
//! with multiple output formats (plain text, inner HTML, structured JSON).

use std::collections::HashMap;

use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use tracing::warn;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors that can occur during extraction.
#[derive(Debug)]
pub enum ExtractError {
    InvalidSelector(String),
    ParseError(String),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSelector(s) => write!(f, "invalid CSS selector: {s}"),
            Self::ParseError(s) => write!(f, "parse error: {s}"),
        }
    }
}

impl std::error::Error for ExtractError {}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Desired output format for a selector query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// Inner text only.
    Text,
    /// Inner HTML.
    Html,
    /// Structured JSON with tag, text, attributes, children.
    Json,
}

/// A single extracted element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedElement {
    pub tag: String,
    pub text: String,
    pub html: String,
    pub attributes: HashMap<String, String>,
}

/// A named selector query with an associated output format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectorQuery {
    pub name: String,
    pub selector: String,
    pub format: OutputFormat,
}

/// Aggregated extraction results keyed by query name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub results: HashMap<String, Vec<serde_json::Value>>,
}

// ---------------------------------------------------------------------------
// Core extraction
// ---------------------------------------------------------------------------

/// Extract all elements matching a single CSS `selector` from `html`.
pub fn extract_css(html: &str, selector: &str) -> Result<Vec<ExtractedElement>, ExtractError> {
    let sel = Selector::parse(selector)
        .map_err(|e| ExtractError::InvalidSelector(format!("{selector}: {e}")))?;

    let document = Html::parse_document(html);

    let elements = document
        .select(&sel)
        .map(|el| {
            let tag = el.value().name().to_string();
            let text: String = el.text().collect::<Vec<_>>().join(" ").trim().to_string();
            let inner_html = el.inner_html();
            let attributes: HashMap<String, String> = el
                .value()
                .attrs()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();

            ExtractedElement {
                tag,
                text,
                html: inner_html,
                attributes,
            }
        })
        .collect();

    Ok(elements)
}

/// Run multiple named selector queries against the same HTML document.
pub fn extract_multiple(
    html: &str,
    selectors: &[SelectorQuery],
) -> Result<ExtractionResult, ExtractError> {
    let document = Html::parse_document(html);
    let mut results: HashMap<String, Vec<serde_json::Value>> = HashMap::new();

    for query in selectors {
        let sel = Selector::parse(&query.selector).map_err(|e| {
            ExtractError::InvalidSelector(format!("{}: {e}", query.selector))
        })?;

        let values: Vec<serde_json::Value> = document
            .select(&sel)
            .map(|el| format_element(&el, query.format))
            .collect();

        if values.is_empty() {
            warn!(selector = %query.selector, name = %query.name, "no elements matched");
        }

        results.insert(query.name.clone(), values);
    }

    Ok(ExtractionResult { results })
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn format_element(el: &scraper::ElementRef<'_>, format: OutputFormat) -> serde_json::Value {
    match format {
        OutputFormat::Text => {
            let text: String = el.text().collect::<Vec<_>>().join(" ").trim().to_string();
            serde_json::Value::String(text)
        }
        OutputFormat::Html => {
            serde_json::Value::String(el.inner_html())
        }
        OutputFormat::Json => {
            let tag = el.value().name().to_string();
            let text: String = el.text().collect::<Vec<_>>().join(" ").trim().to_string();
            let attrs: HashMap<String, String> = el
                .value()
                .attrs()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let children: Vec<serde_json::Value> = el
                .children()
                .filter_map(|child| {
                    let el_ref = scraper::ElementRef::wrap(child)?;
                    Some(format_element(&el_ref, OutputFormat::Json))
                })
                .collect();

            serde_json::json!({
                "tag": tag,
                "text": text,
                "attributes": attrs,
                "children": children,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_HTML: &str = r#"
    <html>
    <body>
        <h1 class="title">Hello World</h1>
        <p>First paragraph</p>
        <p>Second paragraph</p>
        <a href="https://example.com" target="_blank">Example Link</a>
        <div class="card">
            <span>Nested text</span>
        </div>
    </body>
    </html>
    "#;

    #[test]
    fn extract_h1() {
        let results = extract_css(SAMPLE_HTML, "h1").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tag, "h1");
        assert_eq!(results[0].text, "Hello World");
        assert_eq!(results[0].attributes.get("class").unwrap(), "title");
    }

    #[test]
    fn extract_paragraphs() {
        let results = extract_css(SAMPLE_HTML, "p").unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].text, "First paragraph");
        assert_eq!(results[1].text, "Second paragraph");
    }

    #[test]
    fn extract_anchor_with_href() {
        let results = extract_css(SAMPLE_HTML, "a[href]").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].attributes.get("href").unwrap(),
            "https://example.com"
        );
        assert_eq!(results[0].text, "Example Link");
    }

    #[test]
    fn extract_by_class() {
        let results = extract_css(SAMPLE_HTML, ".card").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].text, "Nested text");
    }

    #[test]
    fn invalid_selector_returns_error() {
        let result = extract_css(SAMPLE_HTML, "[[[invalid");
        assert!(result.is_err());
        match result.unwrap_err() {
            ExtractError::InvalidSelector(msg) => {
                assert!(msg.contains("[[[invalid"));
            }
            other => panic!("expected InvalidSelector, got: {other:?}"),
        }
    }

    #[test]
    fn multi_selector_text_format() {
        let queries = vec![
            SelectorQuery {
                name: "headings".to_string(),
                selector: "h1".to_string(),
                format: OutputFormat::Text,
            },
            SelectorQuery {
                name: "links".to_string(),
                selector: "a".to_string(),
                format: OutputFormat::Text,
            },
        ];

        let result = extract_multiple(SAMPLE_HTML, &queries).unwrap();
        assert_eq!(result.results.len(), 2);

        let headings = &result.results["headings"];
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].as_str().unwrap(), "Hello World");

        let links = &result.results["links"];
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].as_str().unwrap(), "Example Link");
    }

    #[test]
    fn multi_selector_html_format() {
        let queries = vec![SelectorQuery {
            name: "card".to_string(),
            selector: ".card".to_string(),
            format: OutputFormat::Html,
        }];

        let result = extract_multiple(SAMPLE_HTML, &queries).unwrap();
        let card_html = result.results["card"][0].as_str().unwrap();
        assert!(card_html.contains("<span>"));
        assert!(card_html.contains("Nested text"));
    }

    #[test]
    fn multi_selector_json_format() {
        let queries = vec![SelectorQuery {
            name: "card".to_string(),
            selector: ".card".to_string(),
            format: OutputFormat::Json,
        }];

        let result = extract_multiple(SAMPLE_HTML, &queries).unwrap();
        let card = &result.results["card"][0];
        assert_eq!(card["tag"], "div");
        assert!(card["text"].as_str().unwrap().contains("Nested text"));
        assert!(card["children"].as_array().unwrap().len() > 0);
        assert_eq!(card["children"][0]["tag"], "span");
    }

    #[test]
    fn no_matches_returns_empty_vec() {
        let results = extract_css(SAMPLE_HTML, ".nonexistent").unwrap();
        assert!(results.is_empty());
    }
}
