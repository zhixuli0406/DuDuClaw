//! `duduclaw knowledge-extract` (hidden): the document-extraction child the
//! gateway spawns (`duduclaw_gateway::knowledge::extract_spawn`). Not for
//! people.
//!
//! Prints one JSON line (`ExtractOutput`) and exits 0, or prints one
//! `ExtractError` JSON line on stderr and exits 2. A panic exits 101 with a
//! one-word message (no document text reaches stderr).

use std::path::Path;

use duduclaw_knowledge::extract::{ExtractError, Format, extract};

/// Largest input read (above the dashboard's 200 MB ceiling).
const MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;

fn fail(e: &ExtractError) -> ! {
    eprintln!(
        "{}",
        serde_json::to_string(e).unwrap_or_else(|_| "{}".to_string())
    );
    std::process::exit(2)
}

/// Run the extractor and exit.
pub fn run(format: &str, input: &Path, max_chars: usize) -> ! {
    std::panic::set_hook(Box::new(|_| eprintln!("knowledge-extract: panic")));
    let Some(format) = Format::parse(format) else {
        fail(&ExtractError::new(
            "unsupported_format",
            "不支援這種檔案格式。",
        ));
    };
    let too_big = std::fs::metadata(input)
        .map(|m| m.len() > MAX_INPUT_BYTES)
        .unwrap_or(false);
    if too_big {
        fail(&ExtractError::new("too_large", "檔案過大。"));
    }
    let bytes = match std::fs::read(input) {
        Ok(b) => b,
        Err(_) => fail(&ExtractError::new(
            "extract_crashed",
            "無法讀取文件原始檔。",
        )),
    };
    match extract(format, &bytes, max_chars) {
        Ok(out) => {
            println!("{}", serde_json::to_string(&out).unwrap_or_default());
            std::process::exit(0)
        }
        Err(e) => fail(&e),
    }
}
