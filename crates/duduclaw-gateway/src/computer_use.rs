//! L5 Computer Use client — calls the Claude Messages API with `computer_use`
//! tool to control a virtual display inside a container sandbox.
//!
//! This module follows the same HTTP client patterns as `direct_api.rs` (shared
//! `OnceLock<reqwest::Client>`, 120s timeout) but adds the `computer_20251124`
//! tool type and multi-turn tool-use conversation handling.
//!
//! Reference: <https://docs.anthropic.com/en/docs/agents-and-tools/computer-use>

use duduclaw_core::truncate_bytes;
use image::ImageEncoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const API_BASE: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const BETA_HEADER: &str = "computer-use-2025-01-24";
const DEFAULT_MAX_TOKENS: u32 = 4096;
const DEFAULT_DISPLAY_WIDTH: u32 = 1280;
const DEFAULT_DISPLAY_HEIGHT: u32 = 800;
const DEFAULT_MAX_ACTIONS: u32 = 50;
const MAX_SELECTOR_LENGTH: usize = 100;
const MAX_SELECTORS: usize = 20;

// ---------------------------------------------------------------------------
// Shared HTTP client (same pattern as direct_api.rs)
// ---------------------------------------------------------------------------

static HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client")
    })
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from the Computer Use session.
#[derive(Debug)]
pub enum ComputerUseError {
    /// The session exceeded `max_actions` without completing.
    MaxActionsExceeded,
    /// HTTP or Anthropic API error.
    ApiError(String),
    /// Failed to parse the API response.
    ParseError(String),
}

impl std::fmt::Display for ComputerUseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MaxActionsExceeded => write!(f, "Maximum actions exceeded"),
            Self::ApiError(msg) => write!(f, "API error: {msg}"),
            Self::ParseError(msg) => write!(f, "Parse error: {msg}"),
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

    // Apply mask rectangles
    for &[x, y, w, h] in regions {
        let x_end = (x + w).min(img_w);
        let y_end = (y + h).min(img_h);
        for py in y..y_end {
            for px in x..x_end {
                img.put_pixel(px, py, fill);
            }
        }
    }

    // Encode back to PNG
    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    encoder
        .write_image(
            img.as_raw(),
            img_w,
            img_h,
            image::ExtendedColorType::Rgba8,
        )
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
            || matches!(c, '-' | '_' | '.' | '#' | '[' | ']' | '=' | '"' | ' '
                | ',' | '*' | '>' | '+' | '~' | ':' | '(' | ')')
    })
}

/// Detect sensitive regions by executing DOM queries inside a container.
///
/// Runs JavaScript in the browser container to find elements matching
/// the CSS selectors in `MaskingConfig.patterns`, and returns their
/// bounding rectangles as pixel coordinates.
pub async fn detect_sensitive_regions(
    container_name: &str,
    patterns: &[String],
) -> Result<Vec<[u32; 4]>, ComputerUseError> {
    // Validate container name to prevent argument injection
    if container_name.is_empty()
        || container_name.len() > 128
        || !container_name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        || container_name.starts_with('-')
    {
        return Err(ComputerUseError::ApiError(format!("invalid container name: {container_name}")));
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
    let selector_json = serde_json::to_string(&combined)
        .unwrap_or_else(|_| "\"\"".to_string());
    let js = format!(
        r#"JSON.stringify(
            Array.from(document.querySelectorAll({selector_json}))
                .map(el => {{
                    const r = el.getBoundingClientRect();
                    return [Math.round(r.x), Math.round(r.y), Math.round(r.width), Math.round(r.height)];
                }})
        )"#
    );

    // Execute in container via docker exec
    let output = tokio::process::Command::new("docker")
        .args([
            "exec",
            container_name,
            "chromium-browser",
            "--headless",
            "--disable-gpu",
            &format!("--evaluate-script={js}"),
        ])
        .output()
        .await
        .map_err(|e| ComputerUseError::ApiError(format!("container exec failed: {e}")))?;

    if !output.status.success() {
        // If detection fails, return empty (don't block the operation)
        tracing::warn!("Sensitive region detection failed, proceeding without masking");
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let regions: Vec<[u32; 4]> = serde_json::from_str(stdout.trim()).unwrap_or_default();

    if !regions.is_empty() {
        tracing::info!(count = regions.len(), "Masked sensitive regions in screenshot");
    }

    Ok(regions)
}

// ---------------------------------------------------------------------------
// Computer actions (maps to `computer_20251124` tool spec)
// ---------------------------------------------------------------------------

/// An action the model requests on the virtual display.
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
// Message / content types for conversation history
// ---------------------------------------------------------------------------

/// A single conversation message (user or assistant turn).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

/// Content blocks that appear inside messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: ImageSource },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlock>,
    },
}

/// Image source for base64-encoded screenshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub media_type: String,
    pub data: String,
}

// ---------------------------------------------------------------------------
// Result type
// ---------------------------------------------------------------------------

/// The outcome of a single `execute_step` call.
pub struct ComputerUseResult {
    /// Actions the model wants executed on the virtual display.
    pub actions: Vec<ComputerAction>,
    /// Optional text reasoning from the model.
    pub text_response: Option<String>,
    /// Input tokens consumed.
    pub input_tokens: u64,
    /// Output tokens consumed.
    pub output_tokens: u64,
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A stateful Computer Use session that tracks action count and enforces limits.
pub struct ComputerUseSession {
    api_key: String,
    model: String,
    display_width: u32,
    display_height: u32,
    max_actions: u32,
    actions_taken: u32,
    pub masking: Option<MaskingConfig>,
}

impl ComputerUseSession {
    /// Create a new session with default display dimensions and action limit.
    pub fn new(api_key: String, model: String, masking: Option<MaskingConfig>) -> Self {
        Self {
            api_key,
            model,
            display_width: DEFAULT_DISPLAY_WIDTH,
            display_height: DEFAULT_DISPLAY_HEIGHT,
            max_actions: DEFAULT_MAX_ACTIONS,
            actions_taken: 0,
            masking,
        }
    }

    /// Create a session with custom display size and action limit.
    pub fn with_config(
        api_key: String,
        model: String,
        display_width: u32,
        display_height: u32,
        max_actions: u32,
        masking: Option<MaskingConfig>,
    ) -> Self {
        Self {
            api_key,
            model,
            display_width,
            display_height,
            max_actions,
            actions_taken: 0,
            masking,
        }
    }

    /// How many actions have been taken so far.
    pub fn actions_taken(&self) -> u32 {
        self.actions_taken
    }

    /// Build the `computer_20251124` tool definition for the API request.
    pub fn build_tool_definition(&self) -> Value {
        serde_json::json!({
            "type": "computer_20251124",
            "name": "computer",
            "display_width_px": self.display_width,
            "display_height_px": self.display_height
        })
    }

    /// Execute one step of the computer-use loop.
    ///
    /// Sends the current screenshot plus conversation history to the API and
    /// returns any actions the model wants performed and optional text reasoning.
    pub async fn execute_step(
        &mut self,
        screenshot_base64: &str,
        task: &str,
        conversation: &[Message],
    ) -> Result<ComputerUseResult, ComputerUseError> {
        if self.actions_taken >= self.max_actions {
            warn!(
                taken = self.actions_taken,
                max = self.max_actions,
                "Computer use action limit reached"
            );
            return Err(ComputerUseError::MaxActionsExceeded);
        }

        let body = self.build_request_body(screenshot_base64, task, conversation);
        let client = http_client();

        let response = client
            .post(API_BASE)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", BETA_HEADER)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ComputerUseError::ApiError(format!("HTTP request failed: {e}")))?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| ComputerUseError::ApiError(format!("Failed to read body: {e}")))?;

        if !status.is_success() {
            return Err(ComputerUseError::ApiError(format!(
                "API error ({status}): {}",
                truncate_bytes(&response_text, 300)
            )));
        }

        let parsed: Value = serde_json::from_str(&response_text)
            .map_err(|e| ComputerUseError::ParseError(format!("JSON parse failed: {e}")))?;

        self.parse_response(&parsed)
    }

    /// Assemble the full JSON request body.
    fn build_request_body(
        &self,
        screenshot_base64: &str,
        task: &str,
        conversation: &[Message],
    ) -> Value {
        let system_prompt = format!(
            "You are controlling a computer with a {w}x{h} display to accomplish the following task:\n\n{task}",
            w = self.display_width,
            h = self.display_height,
        );

        // Build messages: conversation history + current screenshot
        let mut messages: Vec<Value> = conversation
            .iter()
            .map(|m| serde_json::to_value(m).unwrap_or_default())
            .collect();

        // Append the current screenshot as a user turn
        let screenshot_message = serde_json::json!({
            "role": "user",
            "content": [
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": screenshot_base64
                    }
                },
                {
                    "type": "text",
                    "text": "Here is the current screenshot. What action should I take next?"
                }
            ]
        });
        messages.push(screenshot_message);

        serde_json::json!({
            "model": self.model,
            "max_tokens": DEFAULT_MAX_TOKENS,
            "system": system_prompt,
            "tools": [self.build_tool_definition()],
            "messages": messages
        })
    }

    /// Parse the API response, extracting actions and text.
    fn parse_response(&mut self, response: &Value) -> Result<ComputerUseResult, ComputerUseError> {
        let content = response
            .get("content")
            .and_then(|c| c.as_array())
            .ok_or_else(|| {
                ComputerUseError::ParseError("Missing 'content' array in response".to_string())
            })?;

        let mut actions = Vec::new();
        let mut text_parts = Vec::new();

        for block in content {
            let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match block_type {
                "text" => {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        text_parts.push(text.to_string());
                    }
                }
                "tool_use" => {
                    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    if name == "computer" {
                        if let Some(input) = block.get("input") {
                            match serde_json::from_value::<ComputerAction>(input.clone()) {
                                Ok(action) => {
                                    self.actions_taken += 1;
                                    actions.push(action);
                                }
                                Err(e) => {
                                    warn!(
                                        error = %e,
                                        input = %input,
                                        "Failed to parse computer action"
                                    );
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        let input_tokens = response
            .get("usage")
            .and_then(|u| u.get("input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let output_tokens = response
            .get("usage")
            .and_then(|u| u.get("output_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let text_response = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join("\n"))
        };

        info!(
            actions_count = actions.len(),
            total_actions = self.actions_taken,
            max_actions = self.max_actions,
            input_tokens,
            output_tokens,
            "Computer use step completed"
        );

        Ok(ComputerUseResult {
            actions,
            text_response,
            input_tokens,
            output_tokens,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definition_structure() {
        let session = ComputerUseSession::new("test-key".into(), "claude-sonnet-4-20250514".into(), None);
        let def = session.build_tool_definition();

        assert_eq!(def["type"], "computer_20251124");
        assert_eq!(def["name"], "computer");
        assert_eq!(def["display_width_px"], 1280);
        assert_eq!(def["display_height_px"], 800);
    }

    #[test]
    fn tool_definition_custom_size() {
        let session = ComputerUseSession::with_config(
            "key".into(),
            "model".into(),
            1920,
            1080,
            100,
            None,
        );
        let def = session.build_tool_definition();
        assert_eq!(def["display_width_px"], 1920);
        assert_eq!(def["display_height_px"], 1080);
    }

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
    fn max_actions_enforced() {
        let mut session = ComputerUseSession::with_config(
            "key".into(),
            "model".into(),
            1280,
            800,
            2,
            None,
        );
        // Simulate having taken the max number of actions
        session.actions_taken = 2;

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let result = rt.block_on(session.execute_step("base64data", "test task", &[]));
        assert!(matches!(result, Err(ComputerUseError::MaxActionsExceeded)));
    }

    #[test]
    fn parse_response_extracts_actions_and_text() {
        let mut session = ComputerUseSession::new("key".into(), "model".into(), None);
        let response = serde_json::json!({
            "content": [
                {"type": "text", "text": "I see a button. Let me click it."},
                {
                    "type": "tool_use",
                    "id": "toolu_01",
                    "name": "computer",
                    "input": {"action": "left_click", "coordinate": [500, 300]}
                }
            ],
            "usage": {
                "input_tokens": 1500,
                "output_tokens": 42
            }
        });

        let result = session.parse_response(&response).unwrap();
        assert_eq!(result.actions.len(), 1);
        assert!(matches!(&result.actions[0], ComputerAction::LeftClick { coordinate } if *coordinate == [500, 300]));
        assert_eq!(result.text_response.as_deref(), Some("I see a button. Let me click it."));
        assert_eq!(result.input_tokens, 1500);
        assert_eq!(result.output_tokens, 42);
        assert_eq!(session.actions_taken(), 1);
    }

    #[test]
    fn message_serialization() {
        let msg = Message {
            role: "user".into(),
            content: vec![
                ContentBlock::Text {
                    text: "Hello".into(),
                },
                ContentBlock::Image {
                    source: ImageSource {
                        source_type: "base64".into(),
                        media_type: "image/png".into(),
                        data: "abc123".into(),
                    },
                },
            ],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"].as_array().unwrap().len(), 2);
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][1]["type"], "image");
    }

    #[test]
    fn error_display() {
        let e = ComputerUseError::MaxActionsExceeded;
        assert_eq!(e.to_string(), "Maximum actions exceeded");

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
