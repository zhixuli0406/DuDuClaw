//! Provider implementations — each module translates the normalized types
//! to/from one native wire format via pure `build_request_body` /
//! `parse_response` functions (offline-testable) plus a thin HTTP shell.
//!
//! | module          | API                        | streaming (v1)        |
//! |-----------------|----------------------------|-----------------------|
//! | `anthropic`     | Messages API               | real SSE              |
//! | `openai`        | Responses API              | buffered (Done-only)  |
//! | `gemini`        | native generateContent     | buffered (Done-only)  |
//! | `openai_compat` | legacy chat/completions    | real SSE              |

pub mod anthropic;
pub mod gemini;
pub mod openai;
pub mod openai_compat;

pub use anthropic::AnthropicProvider;
pub use gemini::GeminiProvider;
pub use openai::OpenAiProvider;
pub use openai_compat::{preset, CompatPreset, OpenAiCompatProvider, COMPAT_PRESETS};
