//! Client adapters and disposable image/HTML processing. No host paths cross RPC.
use duduclaw_isolation::{bounded::{List, Text}, worker::*};

fn image_request(options: ParseOptions) -> ParseRequest {
    let mut request = crate::client::request(ParseOperation::TransformImage, ContentFormat::Image, 0);
    request.options = Some(options);
    request.outputs = List::new(vec![OutputFd { slot: 1, max_bytes: MAX_OUTPUT_BYTES.try_into().unwrap() }]).unwrap();
    request
}

fn image_output(bytes: &[u8], options: ParseOptions) -> Result<Vec<u8>, ParseError> {
    let request = image_request(options);
    let (response, mut outputs) = crate::client::invoke_bytes_with_output(&request, bytes)?;
    let ParseResult::Complete { items } = response.result else { return Err(ParseError::InvalidContent); };
    if !matches!(items.as_slice(), [ParsedItem::OutputArtifact { output_slot: 1, format: ContentFormat::Image, .. }])
        || outputs.len() != 1 { return Err(ParseError::InvalidContent); }
    Ok(outputs.remove(0))
}

pub fn resize_image(bytes: &[u8], max_dimension: u32) -> Result<Vec<u8>, ParseError> {
    image_output(bytes, ParseOptions::ImageResize { max_dimension })
}

pub fn mask_image(bytes: &[u8], regions: &[[u32; 4]], fill: [u8; 3]) -> Result<Vec<u8>, ParseError> {
    image_output(bytes, ParseOptions::ImageMask {
        regions: List::new(regions.to_vec()).map_err(|_| ParseError::LimitExceeded)?, fill,
    })
}

/// Values are bounded JSON content, decoded by the caller into its existing
/// presentation types. The query names come from this request, never authority.
pub fn html_query(name: &str, selector: &str, format: HtmlOutputFormat) -> Result<HtmlQuery, ParseError> {
    Ok(HtmlQuery {
        name: Text::new(name.to_owned()).map_err(|_| ParseError::LimitExceeded)?,
        selector: Text::new(selector.to_owned()).map_err(|_| ParseError::LimitExceeded)?,
        format, max_matches: 0,
    })
}

pub fn extract_html(bytes: &[u8], queries: Vec<HtmlQuery>) -> Result<Vec<(String, serde_json::Value)>, ParseError> {
    let mut request = crate::client::request(ParseOperation::ExtractHtml, ContentFormat::Text, 0);
    request.options = Some(ParseOptions::Html {
        queries: List::new(queries).map_err(|_| ParseError::LimitExceeded)?,
    });
    html_response(crate::client::invoke_reader(&request, &mut std::io::Cursor::new(bytes))?)
}

fn html_response(response: ParseResponse) -> Result<Vec<(String, serde_json::Value)>, ParseError> {
    let ParseResult::Complete { items } = response.result else { return Err(ParseError::InvalidContent); };
    items.as_slice().iter().map(|item| {
        let ParsedItem::Html { input_slot: 0, query, value } = item else { return Err(ParseError::InvalidContent); };
        let value = serde_json::from_str(value.as_str()).map_err(|_| ParseError::InvalidContent)?;
        Ok((query.as_str().to_owned(), value))
    }).collect()
}

pub fn search_html(bytes: &[u8]) -> Result<String, ParseError> {
    let mut request = crate::client::request(ParseOperation::ExtractHtml, ContentFormat::Text, 0);
    request.options = Some(ParseOptions::SearchHtml);
    let results = html_response(crate::client::invoke_reader(&request, &mut std::io::Cursor::new(bytes))?)?;
    if results.is_empty() { return Ok("No results found.".into()); }
    let mut lines = Vec::new();
    for (index, (name, value)) in results.into_iter().enumerate() {
        if name != "search" || index >= 5 { return Err(ParseError::InvalidContent); }
        let text = value.as_str().ok_or(ParseError::InvalidContent)?;
        lines.push(format!("{}. {}", index + 1, text));
    }
    Ok(lines.join("\n\n"))
}

#[cfg(feature = "engine")]
mod engine {
    use super::*;
    use std::fs::File;
    use std::io::{Cursor, Read, Seek, SeekFrom, Write};
    use sha2::{Digest, Sha256};
    use scraper::{ElementRef, Html, Selector};

    pub fn parse(request: &ParseRequest, inputs: &mut [File], outputs: &mut [File])
        -> Option<Result<Vec<ParsedItem>, ParseError>>
    {
        if !matches!(request.operation, ParseOperation::TransformImage | ParseOperation::ExtractHtml) { return None; }
        Some((|| {
            request.validate().map_err(|_| ParseError::InvalidContent)?;
            if inputs.len() != 1 || request.inputs.as_slice().len() != 1 { return Err(ParseError::InvalidContent); }
            let expected = if request.operation == ParseOperation::TransformImage { ContentFormat::Image } else { ContentFormat::Text };
            if request.inputs.as_slice()[0].format != expected { return Err(ParseError::InvalidContent); }
            let input = &mut inputs[0];
            input.seek(SeekFrom::Start(0)).map_err(|_| ParseError::InvalidContent)?;
            let mut bytes = Vec::new();
            input.take(crate::policy::MAX_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(|_| ParseError::InvalidContent)?;
            if bytes.len() as u64 > crate::policy::MAX_FILE_BYTES { return Err(ParseError::LimitExceeded); }
            if bytes.len() as u64 != request.inputs.as_slice()[0].size_bytes { return Err(ParseError::InvalidContent); }
            match request.operation {
                ParseOperation::TransformImage => {
                    if outputs.len() != 1 || request.outputs.as_slice().len() != 1 { return Err(ParseError::InvalidContent); }
                    image(request, &bytes, &mut outputs[0]).map(|item| vec![item])
                }
                ParseOperation::ExtractHtml => {
                    if !outputs.is_empty() || !request.outputs.as_slice().is_empty() { return Err(ParseError::InvalidContent); }
                    html(request, &bytes)
                }
                _ => unreachable!(),
            }
        })())
    }

    fn image_error(error: image::ImageError) -> ParseError {
        match error {
            image::ImageError::Limits(_) => ParseError::LimitExceeded,
            image::ImageError::Unsupported(_) => ParseError::UnsupportedFormat,
            _ => ParseError::InvalidContent,
        }
    }

    fn decode(bytes: &[u8]) -> Result<image::DynamicImage, ParseError> {
        use image::ImageDecoder;
        let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()
            .map_err(|_| ParseError::InvalidContent)?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(crate::parsers::MAX_IMAGE_DIMENSION);
        limits.max_image_height = Some(crate::parsers::MAX_IMAGE_DIMENSION);
        limits.max_alloc = Some(crate::parsers::MAX_IMAGE_BYTES);
        reader.limits(limits);
        let decoder = reader.into_decoder().map_err(image_error)?;
        let (width, height) = decoder.dimensions();
        if width == 0 || height == 0 { return Err(ParseError::InvalidContent); }
        if width > crate::parsers::MAX_IMAGE_DIMENSION || height > crate::parsers::MAX_IMAGE_DIMENSION
            || decoder.total_bytes() > crate::parsers::MAX_IMAGE_BYTES
            || u64::from(width) * u64::from(height) * 4 > crate::parsers::MAX_IMAGE_BYTES {
            return Err(ParseError::LimitExceeded);
        }
        image::DynamicImage::from_decoder(decoder).map_err(image_error)
    }

    /// Encoders may seek; refuse both sparse growth and writes past the budget.
    struct BoundedWriter { cursor: Cursor<Vec<u8>>, max: u64 }
    impl Write for BoundedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.cursor.position().checked_add(bytes.len() as u64).is_none_or(|end| end > self.max) {
                return Err(std::io::Error::other("output limit"));
            }
            self.cursor.write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    impl Seek for BoundedWriter {
        fn seek(&mut self, seek: SeekFrom) -> std::io::Result<u64> {
            let position = self.cursor.seek(seek)?;
            if position > self.max { return Err(std::io::Error::other("output limit")); }
            Ok(position)
        }
    }

    fn image(request: &ParseRequest, bytes: &[u8], output: &mut File) -> Result<ParsedItem, ParseError> {
        let img = decode(bytes)?;
        let (image, format) = match request.options.as_ref() {
            Some(ParseOptions::ImageResize { max_dimension }) => {
                if *max_dimension == 0 || *max_dimension > crate::parsers::MAX_IMAGE_DIMENSION { return Err(ParseError::LimitExceeded); }
                let img = if img.width().max(img.height()) > *max_dimension {
                    img.resize(*max_dimension, *max_dimension, image::imageops::FilterType::Lanczos3)
                } else { img };
                (img, image::ImageFormat::Jpeg)
            }
            Some(ParseOptions::ImageMask { regions, fill }) => {
                let mut img = img.to_rgba8();
                let color = image::Rgba([fill[0], fill[1], fill[2], 255]);
                for &[x, y, width, height] in regions.as_slice() {
                    for py in y.min(img.height())..y.saturating_add(height).min(img.height()) {
                        for px in x.min(img.width())..x.saturating_add(width).min(img.width()) {
                            img.put_pixel(px, py, color);
                        }
                    }
                }
                (image::DynamicImage::ImageRgba8(img), image::ImageFormat::Png)
            }
            _ => return Err(ParseError::InvalidContent),
        };
        let plan = &request.outputs.as_slice()[0];
        let mut writer = BoundedWriter { cursor: Cursor::new(Vec::new()), max: plan.max_bytes.get() };
        image.write_to(&mut writer, format).map_err(|_| ParseError::LimitExceeded)?;
        let bytes = writer.cursor.into_inner();
        output.write_all(&bytes).map_err(|_| ParseError::LimitExceeded)?;
        Ok(ParsedItem::OutputArtifact { output_slot: plan.slot, format: ContentFormat::Image,
            size_bytes: bytes.len() as u64, sha256: duduclaw_isolation::ids::Sha256Digest(Sha256::digest(&bytes).into()) })
    }

    fn element_text(el: &ElementRef<'_>) -> String {
        el.text().collect::<Vec<_>>().join(" ").trim().to_owned()
    }

    fn formatted(el: &ElementRef<'_>, format: HtmlOutputFormat, depth: usize, nodes: &mut usize)
        -> Result<serde_json::Value, ParseError>
    {
        if depth > 32 || *nodes >= 2048 { return Err(ParseError::LimitExceeded); }
        *nodes += 1;
        Ok(match format {
            HtmlOutputFormat::Text => serde_json::Value::String(element_text(el)),
            HtmlOutputFormat::Html => serde_json::Value::String(el.inner_html()),
            HtmlOutputFormat::Element | HtmlOutputFormat::Json => {
                let attrs: std::collections::BTreeMap<_, _> = el.value().attrs().collect();
                if format == HtmlOutputFormat::Element {
                    serde_json::json!({ "tag": el.value().name(), "text": element_text(el), "html": el.inner_html(), "attributes": attrs })
                } else {
                    let children = el.children().filter_map(ElementRef::wrap)
                        .map(|child| formatted(&child, format, depth + 1, nodes)).collect::<Result<Vec<_>, _>>()?;
                    serde_json::json!({ "tag": el.value().name(), "text": element_text(el), "attributes": attrs, "children": children })
                }
            }
        })
    }

    fn push(items: &mut Vec<ParsedItem>, remaining: &mut usize, query: &str, value: serde_json::Value)
        -> Result<(), ParseError>
    {
        let value = serde_json::to_string(&value).map_err(|_| ParseError::InvalidContent)?;
        let used = value.len().saturating_add(query.len());
        if items.len() >= 128 || used > *remaining { return Err(ParseError::LimitExceeded); }
        *remaining -= used;
        items.push(ParsedItem::Html { input_slot: 0,
            query: Text::new(query.to_owned()).map_err(|_| ParseError::LimitExceeded)?,
            value: Text::new(value).map_err(|_| ParseError::LimitExceeded)? });
        Ok(())
    }

    fn html(request: &ParseRequest, bytes: &[u8]) -> Result<Vec<ParsedItem>, ParseError> {
        let html = std::str::from_utf8(bytes).map_err(|_| ParseError::InvalidContent)?;
        let document = Html::parse_document(html);
        let mut items = Vec::new();
        let mut remaining = request.limits.max_result_text_bytes.get() as usize;
        match request.options.as_ref() {
            Some(ParseOptions::Html { queries }) => {
                let mut nodes = 0;
                for query in queries.as_slice() {
                    let selector = Selector::parse(query.selector.as_str()).map_err(|_| ParseError::InvalidContent)?;
                    for (index, el) in document.select(&selector).enumerate() {
                        if query.max_matches > 0 && index >= query.max_matches as usize { break; }
                        push(&mut items, &mut remaining, query.name.as_str(), formatted(&el, query.format, 0, &mut nodes)?)?;
                    }
                }
            }
            Some(ParseOptions::SearchHtml) => {
                for selector in [".result__snippet", ".result__a", ".links_main a"] {
                    let selector = Selector::parse(selector).map_err(|_| ParseError::InvalidContent)?;
                    for el in document.select(&selector) {
                        let text = element_text(&el);
                        if text.len() > 10 { push(&mut items, &mut remaining, "search", text.into())?; }
                        if items.len() == 5 { break; }
                    }
                    if !items.is_empty() { break; }
                }
            }
            _ => return Err(ParseError::InvalidContent),
        }
        Ok(items)
    }
}

#[cfg(feature = "engine")]
pub use engine::parse;

#[cfg(all(test, feature = "engine"))]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Seek, SeekFrom, Write};

    fn run(mut request: ParseRequest, bytes: &[u8]) -> Result<(Vec<ParsedItem>, Vec<u8>), ParseError> {
        request.inputs = List::new(vec![InputFd { slot: 0,
            format: request.inputs.as_slice()[0].format, size_bytes: bytes.len() as u64 }]).unwrap();
        let mut input = tempfile::tempfile().unwrap();
        input.write_all(bytes).unwrap();
        let mut outputs = if request.outputs.as_slice().is_empty() { vec![] } else { vec![tempfile::tempfile().unwrap()] };
        let items = parse(&request, &mut [input], &mut outputs).unwrap()?;
        let mut bytes = Vec::new();
        if let Some(output) = outputs.first_mut() {
            output.seek(SeekFrom::Start(0)).unwrap();
            output.read_to_end(&mut bytes).unwrap();
        }
        Ok((items, bytes))
    }

    #[test]
    fn isolated_media_resize_mask_and_digest_match_legacy() {
        use sha2::{Digest, Sha256};
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(4, 4, image::Rgba([255,255,255,255])));
        let mut input = Cursor::new(Vec::new());
        image.write_to(&mut input, image::ImageFormat::Png).unwrap();
        let (_, jpeg) = run(image_request(ParseOptions::ImageResize { max_dimension: 2 }), input.get_ref()).unwrap();
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2, 2));
        let (items, png) = run(image_request(ParseOptions::ImageMask {
            regions: List::new(vec![[2, 2, u32::MAX, u32::MAX]]).unwrap(), fill: [0, 0, 0],
        }), input.get_ref()).unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(3, 3).0, [0,0,0,255]);
        assert_eq!(decoded.get_pixel(1, 1).0, [255,255,255,255]);
        let ParsedItem::OutputArtifact { size_bytes, sha256, .. } = &items[0] else { panic!("artifact"); };
        assert_eq!(*size_bytes, png.len() as u64);
        assert_eq!(sha256.0, <[u8;32]>::from(Sha256::digest(&png)));
    }

    #[test]
    fn isolated_media_rejects_malformed_images_and_output_overflow() {
        assert_eq!(run(image_request(ParseOptions::ImageResize { max_dimension: 2 }), b"not image"), Err(ParseError::UnsupportedFormat));
        let mut input = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(4, 4).write_to(&mut input, image::ImageFormat::Png).unwrap();
        let mut request = image_request(ParseOptions::ImageResize { max_dimension: 2 });
        request.outputs = List::new(vec![OutputFd { slot: 1, max_bytes: 1.try_into().unwrap() }]).unwrap();
        assert_eq!(run(request, input.get_ref()), Err(ParseError::LimitExceeded));
    }

    fn html_request(queries: Vec<HtmlQuery>) -> ParseRequest {
        let mut request = crate::client::request(ParseOperation::ExtractHtml, ContentFormat::Text, 0);
        request.options = Some(ParseOptions::Html { queries: List::new(queries).unwrap() });
        request
    }

    #[test]
    fn isolated_html_preserves_attributes_and_recursive_json() {
        let request = html_request(vec![
            html_query("element", "div", HtmlOutputFormat::Element).unwrap(),
            html_query("tree", "div", HtmlOutputFormat::Json).unwrap(),
        ]);
        let (items, _) = run(request, b"<div class='card'><span>hello</span></div>").unwrap();
        let ParsedItem::Html { query, value, .. } = &items[0] else { panic!("html"); };
        assert_eq!(query.as_str(), "element");
        let value: serde_json::Value = serde_json::from_str(value.as_str()).unwrap();
        assert_eq!(value["attributes"]["class"], "card");
        assert_eq!(value["html"], "<span>hello</span>");
        let ParsedItem::Html { value, .. } = &items[1] else { panic!("html"); };
        let value: serde_json::Value = serde_json::from_str(value.as_str()).unwrap();
        assert_eq!(value["children"][0]["tag"], "span");
    }

    #[test]
    fn isolated_html_rejects_depth_result_and_selector_limits() {
        let deep = format!("{}x{}", "<div>".repeat(40), "</div>".repeat(40));
        assert_eq!(run(html_request(vec![html_query("x", "body > div", HtmlOutputFormat::Json).unwrap()]), deep.as_bytes()), Err(ParseError::LimitExceeded));
        assert_eq!(run(html_request(vec![html_query("x", "p", HtmlOutputFormat::Text).unwrap()]), "<p>x</p>".repeat(129).as_bytes()), Err(ParseError::LimitExceeded));
        assert_eq!(run(html_request(vec![html_query("x", "[[invalid", HtmlOutputFormat::Text).unwrap()]), b"hello"), Err(ParseError::InvalidContent));
    }

    #[test]
    fn isolated_search_uses_selector_priority_and_five_result_limit() {
        let mut request = crate::client::request(ParseOperation::ExtractHtml, ContentFormat::Text, 0);
        request.options = Some(ParseOptions::SearchHtml);
        let html = format!("<a class='result__a'>link should be ignored</a>{}", "<p class='result__snippet'>long enough snippet</p>".repeat(7));
        let (items, _) = run(request, html.as_bytes()).unwrap();
        assert_eq!(items.len(), 5);
        for item in items {
            let ParsedItem::Html { value, .. } = item else { panic!("html"); };
            assert_eq!(value.as_str(), "\"long enough snippet\"");
        }
    }
}
