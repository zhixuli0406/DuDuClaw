//! Tests for the ACP module — reverse-RPC types and Agent Card generation.

#[test]
fn test_reverse_rpc_types() {
    use super::types::*;

    // Test FsReadFileParams serialization
    let params = FsReadFileParams {
        path: "/tmp/test.txt".to_string(),
    };
    let json = serde_json::to_value(&params).unwrap();
    assert_eq!(json["path"], "/tmp/test.txt");

    // Test FsReadFileResult
    let result = FsReadFileResult {
        content: "hello world".to_string(),
    };
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["content"], "hello world");

    // Test FsWriteFileParams
    let params = FsWriteFileParams {
        path: "/tmp/output.txt".to_string(),
        content: "new content".to_string(),
    };
    let json = serde_json::to_value(&params).unwrap();
    assert_eq!(json["path"], "/tmp/output.txt");
    assert_eq!(json["content"], "new content");

    // Test RequestPermissionParams
    let params = RequestPermissionParams {
        tool_name: "Bash".to_string(),
        description: "Run git status".to_string(),
    };
    let json = serde_json::to_value(&params).unwrap();
    assert_eq!(json["tool_name"], "Bash");

    // Test RequestPermissionResult
    let result = RequestPermissionResult { granted: true };
    let json = serde_json::to_value(&result).unwrap();
    assert!(json["granted"].as_bool().unwrap());
}

#[test]
fn test_agent_card_well_known_structure() {
    use super::server::AcpServer;

    let card = AcpServer::generate_agent_card(
        "DuDuClaw Agent",
        "AI agent with channel routing and self-evolution",
        "/.well-known/agent.json",
    );

    let json = serde_json::to_value(&card).unwrap();

    // Verify .well-known/agent.json structure matches A2A spec
    assert!(json.get("name").is_some());
    assert!(json.get("description").is_some());
    assert!(json.get("url").is_some());
    assert!(json.get("version").is_some());
    assert!(json.get("capabilities").is_some());
    assert!(json.get("skills").is_some());

    // Verify URL matches .well-known path
    assert_eq!(json["url"], "/.well-known/agent.json");

    // Verify skills are non-empty
    let skills = json["skills"].as_array().unwrap();
    assert!(!skills.is_empty());

    // Each skill should have name, description, tags
    for skill in skills {
        assert!(skill.get("name").is_some());
        assert!(skill.get("description").is_some());
        assert!(skill.get("tags").is_some());
    }
}
