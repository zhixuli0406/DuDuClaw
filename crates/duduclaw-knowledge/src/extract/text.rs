//! Plain text and Markdown.
//!
//! Markdown is split into one section per ATX heading (`#` … `######`), the
//! label being the heading path (「第二章 > 2.1 請假」). `#` lines inside a
//! fenced code block are not headings. Markdown is never rendered to HTML.

use super::encoding::decode_text;
use super::{Budget, ExtractError, RawSegment, SegmentKind};

/// A `.txt` file: one text segment.
pub fn extract_plain(bytes: &[u8], budget: &mut Budget) -> Result<Vec<RawSegment>, ExtractError> {
    let text = decode_text(bytes)?;
    budget.take(&text)?;
    Ok(vec![RawSegment::new(SegmentKind::Text, text)])
}

fn heading_level(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    // Up to three leading spaces, like CommonMark.
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let title = rest.trim().trim_end_matches('#').trim();
    Some((hashes, title))
}

/// A Markdown file: sections by heading.
pub fn extract_markdown(bytes: &[u8], budget: &mut Budget) -> Result<Vec<RawSegment>, ExtractError> {
    let text = decode_text(bytes)?;
    budget.take(&text)?;
    let mut segments = Vec::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;
    let flush = |segments: &mut Vec<RawSegment>, path: &[(usize, String)], body: &mut String| {
        if body.trim().is_empty() {
            body.clear();
            return;
        }
        let mut seg = RawSegment::new(SegmentKind::Section, std::mem::take(body));
        let label = path.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" > ");
        seg.label = (!label.is_empty()).then_some(label);
        segments.push(seg);
    };
    for line in text.lines() {
        let fence = line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~");
        if fence {
            in_fence = !in_fence;
        }
        if !in_fence
            && !fence
            && let Some((level, title)) = heading_level(line)
        {
            flush(&mut segments, &path, &mut current);
            path.retain(|(l, _)| *l < level);
            path.push((level, title.to_string()));
            // The heading line stays in the body so it is searchable and
            // visible in the source viewer.
            current.push_str(title);
            current.push('\n');
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    flush(&mut segments, &path, &mut current);
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_sections_follow_heading_path() {
        let md = "前言\n# 第一章\n內容一\n## 1.1 請假\n請假規定\n```\n# 不是標題\n```\n# 第二章\n內容二\n";
        let segs = extract_markdown(md.as_bytes(), &mut Budget::new(10_000)).unwrap();
        let labels: Vec<_> = segs.iter().map(|s| s.label.clone()).collect();
        assert_eq!(
            labels,
            vec![
                None,
                Some("第一章".into()),
                Some("第一章 > 1.1 請假".into()),
                Some("第二章".into())
            ]
        );
        assert!(segs[2].text.contains("# 不是標題"));
    }
}
