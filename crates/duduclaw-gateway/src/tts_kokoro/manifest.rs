//! Pinned, request-downloaded Kokoro assets. Weights are not bundled.
pub const REVISION: &str = "6cc0f0d2ebe369a68b0df87c2b65c1af8c0ac3e3";
pub const VOICES: &[&str] = &["zf_001", "zm_010", "af_maple"];
pub struct Asset {
    pub path: &'static str,
    pub url: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}
macro_rules! model_asset {
    ($path:literal, $size:literal, $sha:literal) => {
        Asset { path: $path, url: concat!("https://huggingface.co/onnx-community/Kokoro-82M-v1.1-zh-ONNX/resolve/6cc0f0d2ebe369a68b0df87c2b65c1af8c0ac3e3/", $path), size: $size, sha256: $sha }
    };
}
pub const ASSETS: &[Asset] = &[
    model_asset!("onnx/model_uint8.onnx", 127356597, "a39469be791eeaa3089c1ed5e58b8731d1f2462ea0e7dae2bc44388e58f973d8"),
    model_asset!("voices/zf_001.bin", 522240, "0a89ec12bb93fb9c74077924daf02568baad64e1f869389f5aaee01a386035f8"),
    model_asset!("voices/zm_010.bin", 522240, "73b088f7e0dc47adca4d6a642ee68843df90ff56ec2800c29d96609989d6de0a"),
    model_asset!("voices/af_maple.bin", 522240, "bd5b230b916ea98c67a3a7a833a3ce43e535ce56a81503f4166ee9390b9ddeeb"),
    Asset { path: "us_gold.json", url: "https://raw.githubusercontent.com/hexgrad/misaki/fba1236595f2d2bf21d414ba6e57d25256afada3/misaki/data/us_gold.json", size: 3000469, sha256: "dc414872a49a28ae6c141463d502fd945f3b2fde040484fdc47d00cc4612686f" },
];
pub fn total_bytes() -> u64 {
    ASSETS.iter().map(|a| a.size).sum::<u64>()
        + duduclaw_onnx::manifest::ort_archive_for_current_platform().map_or(0, |a| a.size)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_assets_are_pinned() {
        for a in ASSETS {
            assert!(a.url.contains(REVISION) || a.url.contains("fba1236595f2d2bf21d414ba6e57d25256afada3"));
            assert_eq!(a.sha256.len(), 64);
            assert!(a.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }
}
