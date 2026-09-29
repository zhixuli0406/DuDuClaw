//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::MethodHandler;

#[test]
fn parses_standard_mcp_json() {
    let text =
        r#"{ "mcpServers": { "fs": { "command": "npx", "args": ["-y", "pkg"], "env": {} } } }"#;
    let out = MethodHandler::parse_mcp_manifest(text, "fallback").unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "fs");
    assert_eq!(out[0].1.command, "npx");
}

#[test]
fn parses_catalog_list_and_single_item() {
    let list = r#"{ "servers": [ { "id": "a", "description": "d", "default_def": { "command": "npx", "args": [], "env": {} } } ] }"#;
    let out = MethodHandler::parse_mcp_manifest(list, "fb").unwrap();
    assert_eq!(out[0].0, "a");
    assert_eq!(out[0].2, "d");

    let single = r#"{ "id": "b", "default_def": { "command": "uvx", "args": [], "env": {} } }"#;
    let out = MethodHandler::parse_mcp_manifest(single, "fb").unwrap();
    assert_eq!(out[0].0, "b");
    assert_eq!(out[0].1.command, "uvx");
}

#[test]
fn parses_bare_def_with_fallback_name() {
    let text = r#"{ "command": "npx", "args": ["-y", "pkg"] }"#;
    let out = MethodHandler::parse_mcp_manifest(text, "my-repo").unwrap();
    assert_eq!(out[0].0, "my-repo");
}

#[test]
fn rejects_garbage_shapes() {
    assert!(MethodHandler::parse_mcp_manifest("not json", "fb").is_err());
    assert!(MethodHandler::parse_mcp_manifest(r#"{ "hello": 1 }"#, "fb").is_err());
    assert!(MethodHandler::parse_mcp_manifest(r#"{ "mcpServers": {} }"#, "fb").is_err());
}

#[test]
fn registry_server_json_remotes_bridged_via_mcp_remote() {
    // Real-world shape: Perspective-AI/mcp ships an MCP Registry
    // server.json with remotes only (no packages).
    let text = r#"{
            "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
            "name": "ai.getperspective/mcp",
            "description": "An AI concierge.",
            "remotes": [ { "type": "streamable-http", "url": "https://getperspective.ai/mcp" } ]
        }"#;
    let out = MethodHandler::parse_mcp_manifest(text, "fb").unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "ai.getperspective-mcp");
    assert_eq!(out[0].1.command, "npx");
    assert_eq!(
        out[0].1.args,
        vec!["-y", "mcp-remote", "https://getperspective.ai/mcp"]
    );
}

#[test]
fn registry_server_json_npm_and_pypi_packages() {
    let text = r#"{
            "name": "io.example/tool",
            "packages": [
                { "registryType": "npm", "identifier": "@example/mcp-tool" },
                { "registryType": "pypi", "identifier": "example-mcp-tool" },
                { "registryType": "nuget", "identifier": "ignored" }
            ]
        }"#;
    let out = MethodHandler::parse_mcp_manifest(text, "fb").unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].1.command, "npx");
    assert_eq!(out[0].1.args, vec!["-y", "@example/mcp-tool"]);
    assert_eq!(out[1].1.command, "uvx");
}

#[test]
fn mcp_servers_url_entry_bridged_command_entry_kept() {
    let text = r#"{ "mcpServers": {
            "remote": { "type": "http", "url": "https://x.example/mcp" },
            "local": { "command": "npx", "args": ["-y", "pkg"] }
        } }"#;
    let out = MethodHandler::parse_mcp_manifest(text, "fb").unwrap();
    assert_eq!(out.len(), 2);
    let remote = out.iter().find(|(n, _, _)| n == "remote").unwrap();
    assert_eq!(remote.1.args[1], "mcp-remote");
    let local = out.iter().find(|(n, _, _)| n == "local").unwrap();
    assert_eq!(local.1.command, "npx");
}

#[test]
fn extracts_config_snippet_from_readme_markdown() {
    let readme = r#"
# Some MCP Server

Install it like this:

```json
{
  "mcpServers": {
    "perspective": {
      "command": "npx",
      "args": ["-y", "mcp-remote", "https://getperspective.ai/mcp"]
    }
  }
}
```

The same snippet works in Cursor and VS Code:

```json
{
  "mcpServers": {
    "perspective": {
      "command": "npx",
      "args": ["-y", "mcp-remote", "https://getperspective.ai/mcp"]
    }
  }
}
```
"#;
    // Direct parse fails (markdown), extraction succeeds and dedupes the
    // repeated snippet down to one candidate.
    let out = MethodHandler::manifest_from_text(readme, "fb").unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "perspective");
    assert_eq!(out[0].1.command, "npx");
}

#[test]
fn markdown_without_snippets_keeps_json_error() {
    assert!(
        MethodHandler::manifest_from_text("# just a readme\nno config here", "fb").is_err()
    );
}

#[test]
fn first_json_object_is_string_aware() {
    let block = r#"prefix { "a": "brace } in string", "b": { "c": 1 } } trailing prose"#;
    let json = MethodHandler::first_json_object(block).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["b"]["c"], 1);
}
