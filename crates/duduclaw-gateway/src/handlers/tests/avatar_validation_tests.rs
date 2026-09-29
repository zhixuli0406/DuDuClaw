//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::{MAX_AVATAR_DECODED_BYTES, decode_avatar_data_uri};
use base64::{Engine, engine::general_purpose::STANDARD as B64};

/// Smallest valid PNG header (8 magic bytes + a bit of filler).
fn png_bytes() -> Vec<u8> {
    let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(&[0u8; 16]);
    v
}

fn data_uri(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", B64.encode(bytes))
}

#[test]
fn accepts_real_png() {
    let (bytes, ext) = decode_avatar_data_uri(&data_uri("image/png", &png_bytes())).unwrap();
    assert_eq!(ext, "png");
    assert_eq!(bytes.len(), 24);
}

#[test]
fn rejects_svg_mime() {
    // SVG is never accepted (XSS vector), even as a valid data URI.
    let uri = data_uri("image/svg+xml", b"<svg></svg>");
    assert!(decode_avatar_data_uri(&uri).is_err());
}

#[test]
fn rejects_mime_magic_mismatch() {
    // Declares PNG but ships JPEG magic bytes — must fail magic-byte check.
    let jpeg = vec![0xFF, 0xD8, 0xFF, 0x00, 0x00];
    let uri = data_uri("image/png", &jpeg);
    assert!(decode_avatar_data_uri(&uri).is_err());
}

#[test]
fn rejects_oversized_payload() {
    // A valid-magic PNG larger than the 512 KB ceiling is rejected.
    let mut big = png_bytes();
    big.resize(MAX_AVATAR_DECODED_BYTES + 1, 0);
    let uri = data_uri("image/png", &big);
    assert!(decode_avatar_data_uri(&uri).is_err());
}

#[test]
fn accepts_webp_and_jpeg() {
    let mut webp = b"RIFF".to_vec();
    webp.extend_from_slice(&[0u8; 4]); // file size
    webp.extend_from_slice(b"WEBP");
    webp.extend_from_slice(&[0u8; 4]);
    assert_eq!(
        decode_avatar_data_uri(&data_uri("image/webp", &webp))
            .unwrap()
            .1,
        "webp"
    );
    let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    assert_eq!(
        decode_avatar_data_uri(&data_uri("image/jpeg", &jpeg))
            .unwrap()
            .1,
        "jpg"
    );
}
