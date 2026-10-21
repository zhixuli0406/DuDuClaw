//! PowerPoint `.pptx`: one segment per slide, in presentation order, with the
//! slide's speaker notes appended under 「備忘稿：」.
//!
//! Order comes from `ppt/presentation.xml` (`p:sldIdLst`) through its
//! relationships; when that cannot be read the slide parts are taken in
//! numeric order. Notes are found through each slide's own relationships,
//! because `notesSlideN` is not numbered like the slides. Field text
//! (`a:fld`, e.g. slide numbers) is skipped.

use super::xml::{XEvent, events, open_zip, read_part, relationships, resolve_target};
use super::{Budget, ExtractError, RawSegment, SegmentKind};

type Zip<'a> = zip::ZipArchive<std::io::Cursor<&'a [u8]>>;

/// Paragraph texts of a slide or notes part.
fn paragraphs(xml: &str) -> Result<Vec<String>, ExtractError> {
    let mut out = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut para = String::new();
    for ev in events(xml)? {
        match ev {
            XEvent::Start { name, .. } => {
                if name == "p" {
                    para.clear();
                }
                stack.push(name);
            }
            XEvent::Empty { name, .. } => {
                if name == "br" {
                    para.push(' ');
                }
            }
            XEvent::End { name } => {
                stack.pop();
                if name == "p" {
                    let t = para.trim().to_string();
                    if !t.is_empty() {
                        out.push(t);
                    }
                    para.clear();
                }
            }
            XEvent::Text(t) => {
                let in_text = stack.last().map(String::as_str) == Some("t");
                let in_field = stack.iter().any(|s| s == "fld");
                if in_text && !in_field {
                    para.push_str(&t);
                }
            }
        }
    }
    Ok(out)
}

/// Slide part names in presentation order.
fn slide_order(zip: &mut Zip<'_>) -> Result<Vec<String>, ExtractError> {
    let from_presentation = (|| -> Result<Option<Vec<String>>, ExtractError> {
        let Some(pres) = read_part(zip, "ppt/presentation.xml")? else {
            return Ok(None);
        };
        let Some(rels) = read_part(zip, "ppt/_rels/presentation.xml.rels")? else {
            return Ok(None);
        };
        let rels = relationships(&rels)?;
        let mut order = Vec::new();
        for ev in events(&pres)? {
            if let XEvent::Start { name, attrs } | XEvent::Empty { name, attrs } = ev
                && name == "sldId"
            {
                // `id` (numeric) and `r:id` share a local name; the
                // relationship id is the non-numeric one.
                let rid = attrs
                    .iter()
                    .filter(|(k, _)| k == "id")
                    .map(|(_, v)| v.as_str())
                    .find(|v| !v.chars().all(|c| c.is_ascii_digit()));
                if let Some((_, _, target)) =
                    rid.and_then(|rid| rels.iter().find(|(id, _, _)| id == rid))
                {
                    order.push(resolve_target("ppt/presentation.xml", target));
                }
            }
        }
        Ok((!order.is_empty()).then_some(order))
    })()?;
    if let Some(order) = from_presentation {
        return Ok(order);
    }
    let mut numbered: Vec<(u32, String)> = zip
        .file_names()
        .filter_map(|n| {
            let n = n.ok()?;
            let num = n
                .strip_prefix("ppt/slides/slide")?
                .strip_suffix(".xml")?
                .parse::<u32>()
                .ok()?;
            Some((num, n.to_string()))
        })
        .collect();
    numbered.sort();
    Ok(numbered.into_iter().map(|(_, n)| n).collect())
}

fn notes_for(zip: &mut Zip<'_>, slide: &str) -> Result<Vec<String>, ExtractError> {
    let (dir, file) = slide.rsplit_once('/').unwrap_or(("", slide));
    let rels_name = format!("{dir}/_rels/{file}.rels");
    let Some(rels) = read_part(zip, &rels_name)? else {
        return Ok(Vec::new());
    };
    let target = relationships(&rels)?
        .into_iter()
        .find(|(_, ty, _)| ty.ends_with("/notesSlide"))
        .map(|(_, _, t)| resolve_target(slide, &t));
    match target {
        Some(part) => match read_part(zip, &part)? {
            Some(xml) => paragraphs(&xml),
            None => Ok(Vec::new()),
        },
        None => Ok(Vec::new()),
    }
}

/// Extract a `.pptx`.
pub fn extract(bytes: &[u8], budget: &mut Budget) -> Result<Vec<RawSegment>, ExtractError> {
    let mut zip = open_zip(bytes)?;
    let order = slide_order(&mut zip)?;
    let mut out = Vec::new();
    for (i, part) in order.iter().enumerate() {
        let Some(xml) = read_part(&mut zip, part)? else {
            continue;
        };
        let mut lines = paragraphs(&xml)?;
        let notes = notes_for(&mut zip, part)?;
        if !notes.is_empty() {
            lines.push(format!("備忘稿：{}", notes.join(" ")));
        }
        let text = lines.join("\n");
        budget.take(&text)?;
        let mut seg = RawSegment::new(SegmentKind::Slide, text);
        seg.page = Some((i + 1) as u32);
        out.push(seg);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::fixtures;

    #[test]
    fn slides_in_order_with_notes() {
        let bytes = fixtures::pptx(&[
            ("產品介紹", Some("講者提醒：先講價格")),
            ("第二張：售後服務", None),
        ]);
        let segs = extract(&bytes, &mut Budget::new(10_000)).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].page, Some(1));
        assert!(segs[0].text.contains("產品介紹"));
        assert!(segs[0].text.contains("備忘稿：講者提醒：先講價格"));
        assert_eq!(segs[1].text, "第二張：售後服務");
    }
}
