//! Text encoding detection for plain-text formats (txt, md, csv, html).
//!
//! Order: a UTF-16 BOM, then UTF-8 (with or without BOM), then Big5 (which
//! encoding_rs implements as the HKSCS/CP950 superset Windows uses in
//! Taiwan). If more than 1% of the decoded characters are replacement
//! characters the encoding is refused rather than indexed as garbage.

use super::ExtractError;

/// Share of U+FFFD above which a decode is refused.
const MAX_REPLACEMENT_RATIO: f64 = 0.01;

fn unknown() -> ExtractError {
    ExtractError::new(
        "encoding_unknown",
        "無法判斷這個檔案的文字編碼。請另存成 UTF-8 後再上傳。",
    )
}

fn replacement_ratio(s: &str) -> f64 {
    let total = s.chars().count();
    if total == 0 {
        return 0.0;
    }
    s.chars().filter(|&c| c == '\u{FFFD}').count() as f64 / total as f64
}

/// Decode `bytes` to text.
pub fn decode_text(bytes: &[u8]) -> Result<String, ExtractError> {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let enc = if bytes[0] == 0xFF {
            encoding_rs::UTF_16LE
        } else {
            encoding_rs::UTF_16BE
        };
        let (text, _, _) = enc.decode(bytes);
        return accept(text.into_owned());
    }
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if let Ok(s) = std::str::from_utf8(body) {
        return Ok(s.to_string());
    }
    let (text, _, _) = encoding_rs::BIG5.decode(body);
    accept(text.into_owned())
}

fn accept(text: String) -> Result<String, ExtractError> {
    if replacement_ratio(&text) > MAX_REPLACEMENT_RATIO {
        Err(unknown())
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_with_bom() {
        assert_eq!(decode_text("\u{FEFF}特休".as_bytes()).unwrap(), "特休");
    }

    #[test]
    fn big5_is_decoded() {
        let (bytes, _, _) = encoding_rs::BIG5.encode("員工請假規定");
        assert_eq!(decode_text(&bytes).unwrap(), "員工請假規定");
    }

    #[test]
    fn utf16le_is_decoded() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "請假".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_text(&bytes).unwrap(), "請假");
    }

    #[test]
    fn binary_garbage_is_refused() {
        let bytes: Vec<u8> = (0..400u32).map(|i| (i * 37 % 256) as u8 | 0x80).collect();
        assert_eq!(decode_text(&bytes).unwrap_err().code, "encoding_unknown");
    }
}
