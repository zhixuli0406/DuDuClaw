//! L5 computer-use primitives shared by the `computer_*` tool sessions
//! (`computer_use_sessions`): the action type, the error type, and the
//! sensitive-region detection and masking applied to every screenshot.
//!
//! The gateway-run, chat-triggered loop that called the Anthropic Messages
//! API with the `computer_20251124` tool was removed: it never completed a
//! session in a released build. [`ComputerAction`] keeps that tool's action
//! names, which the `computer_*` tools translate into.

use image::ImageEncoder;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_SELECTOR_LENGTH: usize = 100;
const MAX_SELECTORS: usize = 20;
/// Upper bound on the in-container `duduclaw-eval-dom` call. Exceeding it is a
/// detection failure, so the caller masks the full screenshot (fail closed).
const DOM_DETECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from a computer-use session.
#[derive(Debug)]
pub enum ComputerUseError {
    /// A container / Docker operation failed. (The name predates the removal
    /// of the Anthropic API loop; the text never carries API output now.)
    ApiError(String),
    /// A screenshot could not be decoded or re-encoded.
    ParseError(String),
    /// Computer use cannot start on this host (image missing, invalid
    /// `[computer_use]` config, Docker not answering). The text is
    /// operator-safe, carries no raw Docker output and is shown to the
    /// caller as is.
    Unavailable(String),
}

impl std::fmt::Display for ComputerUseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiError(msg) => write!(f, "API error: {msg}"),
            Self::ParseError(msg) => write!(f, "Parse error: {msg}"),
            Self::Unavailable(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ComputerUseError {}

// ---------------------------------------------------------------------------
// Sensitive area masking
// ---------------------------------------------------------------------------

/// Configuration for sensitive area masking in screenshots.
#[derive(Debug, Clone)]
pub struct MaskingConfig {
    /// CSS-like patterns identifying sensitive areas.
    /// Used to generate xdotool-compatible region queries.
    pub patterns: Vec<String>,
    /// Color to fill masked regions (default: black).
    pub fill_color: [u8; 3],
}

impl Default for MaskingConfig {
    fn default() -> Self {
        Self {
            patterns: vec![
                "input[type=password]".to_string(),
                ".credit-card".to_string(),
                "[data-sensitive]".to_string(),
            ],
            fill_color: [0, 0, 0], // black
        }
    }
}

/// Mask sensitive regions in a base64-encoded PNG screenshot.
///
/// This applies black rectangles over regions identified by the masking config.
/// The regions are specified as pixel coordinates [x, y, width, height].
///
/// In a full implementation, region detection would use:
/// 1. Container-side DOM inspection to find matching elements
/// 2. xdotool to get element coordinates
/// 3. This function to apply the mask to the screenshot
///
/// For now, this function applies masks at specified coordinates.
pub fn mask_screenshot_regions(
    screenshot_base64: &str,
    regions: &[[u32; 4]], // [x, y, width, height] for each region
    fill_color: [u8; 3],
) -> Result<String, ComputerUseError> {
    use base64::Engine;

    if regions.is_empty() {
        return Ok(screenshot_base64.to_string());
    }

    // Decode base64 to PNG bytes
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(screenshot_base64)
        .map_err(|e| ComputerUseError::ParseError(format!("invalid base64: {e}")))?;

    // Load image
    let mut img = image::load_from_memory(&png_bytes)
        .map_err(|e| ComputerUseError::ParseError(format!("invalid image: {e}")))?
        .to_rgba8();

    let (img_w, img_h) = (img.width(), img.height());
    let fill = image::Rgba([fill_color[0], fill_color[1], fill_color[2], 255]);

    // Apply mask rectangles.
    //
    // M7: `x + w` / `y + h` are attacker-influenced (regions come from
    // DOM-reported bounding boxes). Plain `+` overflows on large values —
    // a panic in debug builds, a silent wrap in release that would skip the
    // mask entirely and ship an UNMASKED sensitive region. Use
    // `saturating_add` and clamp both the start and end to the image bounds
    // so an out-of-range region is harmlessly empty rather than unsafe.
    for &[x, y, w, h] in regions {
        let x_start = x.min(img_w);
        let y_start = y.min(img_h);
        let x_end = x.saturating_add(w).min(img_w);
        let y_end = y.saturating_add(h).min(img_h);
        for py in y_start..y_end {
            for px in x_start..x_end {
                img.put_pixel(px, py, fill);
            }
        }
    }

    // Encode back to PNG
    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    encoder
        .write_image(img.as_raw(), img_w, img_h, image::ExtendedColorType::Rgba8)
        .map_err(|e| ComputerUseError::ParseError(format!("PNG encode failed: {e}")))?;

    // Encode to base64
    let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
    Ok(b64)
}

/// Validates that a CSS selector pattern contains only safe characters.
/// Rejects any characters that could escape a JavaScript string context.
fn is_safe_css_selector(selector: &str) -> bool {
    selector.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '-' | '_'
                    | '.'
                    | '#'
                    | '['
                    | ']'
                    | '='
                    | '"'
                    | ' '
                    | ','
                    | '*'
                    | '>'
                    | '+'
                    | '~'
                    | ':'
                    | '('
                    | ')'
            )
    })
}

/// Exit status `duduclaw-eval-dom` uses for exactly one failure: more than
/// one browser page is visible at once (e.g. after `ctrl+n`), so it cannot
/// tell which one is on screen. Every other failure exits 1. Mirrors
/// `EXIT_SEVERAL_PAGES` in `container/scripts/duduclaw-eval-dom`.
pub const EVAL_DOM_EXIT_SEVERAL_PAGES: i32 = 3;

/// Why sensitive-region detection produced no regions. Either way the caller
/// masks the whole screenshot (fail closed); the kind only tells it why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionDetectFailure {
    /// The helper reported several visible pages
    /// ([`EVAL_DOM_EXIT_SEVERAL_PAGES`]).
    SeveralPages,
    /// Any other failure: spawn error, timeout, other non-zero exit,
    /// unparseable output, invalid container name. The text is for logs only.
    Failed(String),
}

impl std::fmt::Display for RegionDetectFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SeveralPages => f.write_str("several browser pages are visible at once"),
            Self::Failed(msg) => f.write_str(msg),
        }
    }
}

/// Detect sensitive regions by executing DOM queries inside a container.
///
/// Runs JavaScript in the browser container to find elements matching
/// the CSS selectors in `MaskingConfig.patterns`, and returns their
/// bounding rectangles as pixel coordinates.
pub async fn detect_sensitive_regions(
    container_name: &str,
    patterns: &[String],
) -> Result<Vec<[u32; 4]>, RegionDetectFailure> {
    // Validate container name to prevent argument injection
    if container_name.is_empty()
        || container_name.len() > 128
        || !container_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        || container_name.starts_with('-')
    {
        return Err(RegionDetectFailure::Failed(format!(
            "invalid container name: {container_name}"
        )));
    }
    if patterns.is_empty() {
        return Ok(Vec::new());
    }

    // Filter out any patterns that contain characters unsafe in a JS string literal,
    // and enforce length and count limits to prevent CLI argument injection.
    let safe_patterns: Vec<&str> = patterns
        .iter()
        .filter(|p| is_safe_css_selector(p) && p.len() <= MAX_SELECTOR_LENGTH)
        .take(MAX_SELECTORS)
        .map(|p| p.as_str())
        .collect();
    if safe_patterns.is_empty() {
        return Ok(vec![]);
    }

    // SEC2-M5: Use JSON serialization for the selector string so that any
    // remaining special characters (e.g. double-quotes) are properly escaped
    // and cannot break out of the JavaScript string context.
    let combined = safe_patterns.join(", ");
    let selector_json = serde_json::to_string(&combined).unwrap_or_else(|_| "\"\"".to_string());
    let js = format!(
        r#"JSON.stringify(
            Array.from(document.querySelectorAll({selector_json}))
                .map(el => {{
                    const r = el.getBoundingClientRect();
                    return [Math.round(r.x), Math.round(r.y), Math.round(r.width), Math.round(r.height)];
                }})
        )"#
    );

    // Execute the DOM-rect query inside the container.
    //
    // HS5 fix (invocation): `chromium-browser --evaluate-script=...` is NOT a
    // valid Chromium flag, so the old call failed virtually every time. We run
    // the JS through the container's CDP helper instead. The container image
    // ships `duduclaw-eval-dom` (`container/scripts/duduclaw-eval-dom`, Python
    // standard library, loopback DevTools port) that reads the script from argv
    // and prints the JSON result on stdout; any failure exits non-zero. The JS is passed as a single argument; it is already constrained
    // to JSON-escaped, validated CSS selectors (see `safe_patterns` above) so it
    // cannot inject shell metacharacters across the `docker exec` boundary.
    //
    // Bounded: a wedged browser (e.g. thread cap hit) must not stall the
    // screenshot loop. On timeout we return `Err`, which makes
    // `capture_masked_screenshot` mask the whole screenshot (fail closed);
    // `kill_on_drop` kills the `docker exec` client when the future is dropped.
    let exec = tokio::process::Command::new("docker")
        .args(["exec", container_name, "duduclaw-eval-dom", &js])
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(DOM_DETECT_TIMEOUT, exec).await {
        Ok(result) => result
            .map_err(|e| RegionDetectFailure::Failed(format!("container exec failed: {e}")))?,
        Err(_) => {
            tracing::warn!(
                timeout_secs = DOM_DETECT_TIMEOUT.as_secs(),
                "Sensitive region detection timed out"
            );
            return Err(RegionDetectFailure::Failed(format!(
                "sensitive region detection timed out after {}s",
                DOM_DETECT_TIMEOUT.as_secs()
            )));
        }
    };

    if !output.status.success() {
        // HS5 fix (fail closed): detection failure must NOT silently proceed
        // without masking — that ships an unmasked screenshot. Return an error
        // so the caller can fail closed (mask the full screen / abort upload).
        // The several-pages case is told apart by the helper's exit status
        // alone, never by its free-text stderr.
        if output.status.code() == Some(EVAL_DOM_EXIT_SEVERAL_PAGES) {
            tracing::warn!("Sensitive region detection: several pages are visible at once");
            return Err(RegionDetectFailure::SeveralPages);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(stderr = %stderr.trim(), "Sensitive region detection failed");
        return Err(RegionDetectFailure::Failed(format!(
            "sensitive region detection failed: {}",
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // HS5 fix (fail closed): an unparseable result is also a failure, not an
    // "all clear". The previous `unwrap_or_default()` turned malformed output
    // into an empty (no-mask) result.
    let regions: Vec<[u32; 4]> = serde_json::from_str(stdout.trim()).map_err(|e| {
        RegionDetectFailure::Failed(format!("could not parse detected regions: {e}"))
    })?;

    if !regions.is_empty() {
        tracing::info!(
            count = regions.len(),
            "Detected sensitive regions in screenshot"
        );
    }

    Ok(regions)
}

// ---------------------------------------------------------------------------
// Computer actions (named after the `computer_20251124` tool spec)
// ---------------------------------------------------------------------------

/// An action on the virtual display (the `computer_*` tools translate into it).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action")]
pub enum ComputerAction {
    #[serde(rename = "screenshot")]
    Screenshot,
    #[serde(rename = "left_click")]
    LeftClick { coordinate: [u32; 2] },
    #[serde(rename = "right_click")]
    RightClick { coordinate: [u32; 2] },
    #[serde(rename = "double_click")]
    DoubleClick { coordinate: [u32; 2] },
    #[serde(rename = "type")]
    Type { text: String },
    #[serde(rename = "key")]
    Key { text: String },
    #[serde(rename = "scroll")]
    Scroll {
        coordinate: [u32; 2],
        direction: String,
        amount: u32,
    },
    #[serde(rename = "mouse_move")]
    MouseMove { coordinate: [u32; 2] },
    #[serde(rename = "wait")]
    Wait { duration: u32 },
    #[serde(rename = "zoom")]
    Zoom { coordinate: [u32; 4] },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_serialize_screenshot() {
        let action = ComputerAction::Screenshot;
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["action"], "screenshot");
    }

    #[test]
    fn action_serialize_left_click() {
        let action = ComputerAction::LeftClick {
            coordinate: [640, 400],
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["action"], "left_click");
        assert_eq!(json["coordinate"], serde_json::json!([640, 400]));
    }

    #[test]
    fn action_serialize_type() {
        let action = ComputerAction::Type {
            text: "hello world".into(),
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["action"], "type");
        assert_eq!(json["text"], "hello world");
    }

    #[test]
    fn action_serialize_key() {
        let action = ComputerAction::Key {
            text: "ctrl+s".into(),
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["action"], "key");
        assert_eq!(json["text"], "ctrl+s");
    }

    #[test]
    fn action_serialize_scroll() {
        let action = ComputerAction::Scroll {
            coordinate: [100, 200],
            direction: "down".into(),
            amount: 3,
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["action"], "scroll");
        assert_eq!(json["direction"], "down");
        assert_eq!(json["amount"], 3);
    }

    #[test]
    fn action_deserialize_roundtrip() {
        let action = ComputerAction::DoubleClick {
            coordinate: [10, 20],
        };
        let json = serde_json::to_value(&action).unwrap();
        let deserialized: ComputerAction = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized, action);
    }

    #[test]
    fn action_serialize_zoom() {
        let action = ComputerAction::Zoom {
            coordinate: [100, 200, 300, 400],
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["action"], "zoom");
        assert_eq!(json["coordinate"], serde_json::json!([100, 200, 300, 400]));
    }

    #[test]
    fn error_display() {
        let e = ComputerUseError::ApiError("timeout".into());
        assert_eq!(e.to_string(), "API error: timeout");

        let e = ComputerUseError::ParseError("bad json".into());
        assert_eq!(e.to_string(), "Parse error: bad json");
    }

    #[test]
    fn mask_screenshot_creates_black_regions() {
        use base64::Engine;

        // Create a tiny 4x4 white PNG
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 255, 255, 255]));
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(img.as_raw(), 4, 4, image::ExtendedColorType::Rgba8)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);

        // Mask a 2x2 region at (1,1)
        let masked = mask_screenshot_regions(&b64, &[[1, 1, 2, 2]], [0, 0, 0]).unwrap();

        // Decode and verify
        let masked_bytes = base64::engine::general_purpose::STANDARD
            .decode(&masked)
            .unwrap();
        let masked_img = image::load_from_memory(&masked_bytes).unwrap().to_rgba8();
        // (0,0) should still be white
        assert_eq!(
            masked_img.get_pixel(0, 0),
            &image::Rgba([255, 255, 255, 255])
        );
        // (1,1) should be black
        assert_eq!(masked_img.get_pixel(1, 1), &image::Rgba([0, 0, 0, 255]));
        // (2,2) should be black
        assert_eq!(masked_img.get_pixel(2, 2), &image::Rgba([0, 0, 0, 255]));
        // (3,3) should be white
        assert_eq!(
            masked_img.get_pixel(3, 3),
            &image::Rgba([255, 255, 255, 255])
        );
    }

    #[test]
    fn mask_empty_regions_is_noop() {
        // With empty regions, should return input unchanged
        let result = mask_screenshot_regions("dGVzdA==", &[], [0, 0, 0]).unwrap();
        assert_eq!(result, "dGVzdA==");
    }

    #[test]
    fn mask_region_near_u32_max_does_not_overflow() {
        // M7 regression: a region with x/y/w/h near u32::MAX must not panic
        // (debug) or wrap (release). `saturating_add` + clamp keeps it in
        // bounds; the mask is harmlessly empty rather than unsafe.
        use base64::Engine;

        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 255, 255, 255]));
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(img.as_raw(), 4, 4, image::ExtendedColorType::Rgba8)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);

        // x + w would overflow u32 with naive `+`.
        let masked = mask_screenshot_regions(
            &b64,
            &[[u32::MAX - 1, u32::MAX - 1, u32::MAX, u32::MAX]],
            [0, 0, 0],
        )
        .expect("must not panic on overflowing region");

        // Out-of-bounds region masks nothing → image unchanged (still white).
        let masked_bytes = base64::engine::general_purpose::STANDARD
            .decode(&masked)
            .unwrap();
        let masked_img = image::load_from_memory(&masked_bytes).unwrap().to_rgba8();
        assert_eq!(
            masked_img.get_pixel(0, 0),
            &image::Rgba([255, 255, 255, 255])
        );
        assert_eq!(
            masked_img.get_pixel(3, 3),
            &image::Rgba([255, 255, 255, 255])
        );
    }

    #[test]
    fn mask_region_clamps_oversized_to_bounds() {
        // A region starting in-bounds but extending past the edge clamps to the
        // image rather than overflowing or skipping the mask.
        use base64::Engine;

        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 255, 255, 255]));
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(img.as_raw(), 4, 4, image::ExtendedColorType::Rgba8)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);

        let masked =
            mask_screenshot_regions(&b64, &[[2, 2, u32::MAX, u32::MAX]], [0, 0, 0]).unwrap();
        let masked_bytes = base64::engine::general_purpose::STANDARD
            .decode(&masked)
            .unwrap();
        let masked_img = image::load_from_memory(&masked_bytes).unwrap().to_rgba8();
        // (3,3) inside the clamped region → black; (0,0) untouched → white.
        assert_eq!(masked_img.get_pixel(3, 3), &image::Rgba([0, 0, 0, 255]));
        assert_eq!(
            masked_img.get_pixel(0, 0),
            &image::Rgba([255, 255, 255, 255])
        );
    }

    #[test]
    fn masking_config_default() {
        let cfg = MaskingConfig::default();
        assert_eq!(cfg.patterns.len(), 3);
        assert!(cfg.patterns[0].contains("password"));
        assert_eq!(cfg.fill_color, [0, 0, 0]);
    }

    #[test]
    fn safe_css_selector_allows_valid_patterns() {
        assert!(is_safe_css_selector("input[type=password]"));
        assert!(is_safe_css_selector(".credit-card"));
        assert!(is_safe_css_selector("[data-sensitive]"));
        assert!(is_safe_css_selector("#main > .container"));
        assert!(is_safe_css_selector("div.foo + span:hover"));
        assert!(is_safe_css_selector("*"));
    }

    #[test]
    fn safe_css_selector_rejects_injection_chars() {
        // semicolons, braces, backticks, dollar signs, newlines
        assert!(!is_safe_css_selector("div; alert(1)"));
        assert!(!is_safe_css_selector("div{color:red}"));
        assert!(!is_safe_css_selector("`template`"));
        assert!(!is_safe_css_selector("$var"));
        assert!(!is_safe_css_selector("div\nalert(1)"));
        assert!(!is_safe_css_selector("div\\'"));
        assert!(!is_safe_css_selector("div\\"));
    }
}
