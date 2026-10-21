//! HTML (web pages added by URL): visible text only, split into sections at
//! `h1`–`h3`. `script`, `style`, `nav`, `footer`, `header`, `noscript`,
//! `template`, `svg`, `form` and `aside` subtrees are dropped. Nothing is
//! executed or fetched; the markup is only walked.

use ego_tree::iter::Edge;
use scraper::{Html, Node};

use super::encoding::decode_text;
use super::{Budget, ExtractError, RawSegment, SegmentKind};

const SKIP: &[&str] = &[
    "script", "style", "nav", "footer", "header", "noscript", "template", "svg", "form", "aside",
    "head",
];
const BLOCK: &[&str] = &[
    "p",
    "div",
    "li",
    "tr",
    "br",
    "pre",
    "blockquote",
    "section",
    "article",
    "main",
    "table",
    "ul",
    "ol",
    "dd",
    "dt",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "td",
    "th",
];

fn heading_level(name: &str) -> Option<usize> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        _ => None,
    }
}

struct Builder {
    segments: Vec<RawSegment>,
    path: Vec<(usize, String)>,
    body: String,
    heading: Option<(usize, String)>,
}

impl Builder {
    fn newline(&mut self) {
        if !self.body.ends_with('\n') && !self.body.is_empty() {
            self.body.push('\n');
        }
    }

    fn flush(&mut self) {
        if self.body.trim().is_empty() {
            self.body.clear();
            return;
        }
        let mut seg = RawSegment::new(SegmentKind::Section, std::mem::take(&mut self.body));
        let label = self
            .path
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join(" > ");
        seg.label = (!label.is_empty()).then_some(label);
        self.segments.push(seg);
    }
}

/// Extract an HTML page.
pub fn extract(bytes: &[u8], budget: &mut Budget) -> Result<Vec<RawSegment>, ExtractError> {
    let source = decode_text(bytes)?;
    let doc = Html::parse_document(&source);
    let mut b = Builder {
        segments: Vec::new(),
        path: Vec::new(),
        body: String::new(),
        heading: None,
    };
    let mut skip_depth = 0usize;
    for edge in doc.root_element().traverse() {
        match edge {
            Edge::Open(node) => match node.value() {
                Node::Element(el) => {
                    let name = el.name();
                    if skip_depth > 0 || SKIP.contains(&name) {
                        skip_depth += 1;
                        continue;
                    }
                    if let Some(level) = heading_level(name) {
                        b.flush();
                        b.heading = Some((level, String::new()));
                    }
                    if BLOCK.contains(&name) {
                        b.newline();
                    }
                }
                Node::Text(t) if skip_depth == 0 => {
                    let text = t.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !text.is_empty() {
                        if !b.body.is_empty() && !b.body.ends_with('\n') && !b.body.ends_with(' ') {
                            b.body.push(' ');
                        }
                        b.body.push_str(&text);
                        if let Some((_, h)) = b.heading.as_mut() {
                            if !h.is_empty() {
                                h.push(' ');
                            }
                            h.push_str(&text);
                        }
                    }
                }
                _ => {}
            },
            Edge::Close(node) => {
                if let Node::Element(el) = node.value() {
                    if skip_depth > 0 {
                        skip_depth -= 1;
                        continue;
                    }
                    let name = el.name();
                    if heading_level(name).is_some()
                        && let Some((level, title)) = b.heading.take()
                    {
                        b.path.retain(|(l, _)| *l < level);
                        b.path
                            .push((level, duduclaw_core::truncate_chars(title.trim(), 80)));
                    }
                    if BLOCK.contains(&name) {
                        b.newline();
                    }
                }
            }
        }
    }
    b.flush();
    for s in &b.segments {
        budget.take(&s.text)?;
    }
    Ok(b.segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_text_and_sections() {
        let html = r#"<html><head><title>t</title><script>alert('x')</script></head><body>
            <nav>選單</nav><h1>退換貨政策</h1><p>七天內可退貨。</p><style>p{}</style>
            <h2>運費</h2><p>滿 <b>1000</b> 元免運。</p><footer>版權</footer></body></html>"#;
        let segs = extract(html.as_bytes(), &mut Budget::new(10_000)).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].label.as_deref(), Some("退換貨政策"));
        assert!(segs[0].text.contains("七天內可退貨。"));
        assert_eq!(segs[1].label.as_deref(), Some("退換貨政策 > 運費"));
        assert!(segs[1].text.contains("滿 1000 元免運。"));
        let all: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert!(!all.contains("alert") && !all.contains("選單") && !all.contains("版權"));
    }
}
