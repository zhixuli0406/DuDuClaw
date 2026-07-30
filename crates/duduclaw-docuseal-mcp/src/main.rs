//! `duduclaw-docuseal-mcp` — MCP stdio server wrapping the DocuSeal REST API.
//!
//! ```sh
//! DOCUSEAL_API_KEY=... duduclaw-docuseal-mcp
//! DOCUSEAL_API_KEY=... DOCUSEAL_BASE_URL=https://sign.example.com/api duduclaw-docuseal-mcp
//! ```

use duduclaw_docuseal_mcp::{serve_stdio, DocusealApi};

const DEFAULT_BASE_URL: &str = "https://api.docuseal.com";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // Diagnostics go to stderr only — stdout is the JSON-RPC wire.
    let api_key = match std::env::var("DOCUSEAL_API_KEY") {
        Ok(k) if !k.trim().is_empty() => k,
        _ => {
            eprintln!("duduclaw-docuseal-mcp: DOCUSEAL_API_KEY is required");
            std::process::exit(2);
        }
    };
    let base_url =
        std::env::var("DOCUSEAL_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());

    let api = match DocusealApi::new(&base_url, &api_key) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("duduclaw-docuseal-mcp: {e}");
            std::process::exit(2);
        }
    };

    if let Err(e) = serve_stdio(api).await {
        eprintln!("duduclaw-docuseal-mcp: io error: {e}");
        std::process::exit(1);
    }
}
