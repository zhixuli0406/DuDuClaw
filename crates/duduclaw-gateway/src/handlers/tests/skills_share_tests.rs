//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! `skills.share` must resolve the same union of sources the my-skills
//! list shows (agent SKILLS dir + global `<home>/skills`), and must reject
//! traversal-shaped names before touching the filesystem.
use super::*;

#[tokio::test]
async fn share_falls_back_to_global_skill() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("skills")).unwrap();
    std::fs::write(
        home.path().join("skills").join("pptx.md"),
        "---\nname: pptx\n---\n\nbody",
    )
    .unwrap();
    // Agent exists but has no local copy of the skill.
    std::fs::create_dir_all(
        home.path()
            .join("agents")
            .join("ceo-assistant")
            .join("SKILLS"),
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_skills_share(json!({ "agent_id": "ceo-assistant", "skill_name": "pptx" }))
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: true, .. }),
        "sharing a global-scope skill must succeed: {frame:?}"
    );
    assert!(
        home.path()
            .join("shared")
            .join("skills")
            .join("pptx.md")
            .exists()
    );
}

#[tokio::test]
async fn share_prefers_agent_copy_over_global() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("skills")).unwrap();
    std::fs::write(home.path().join("skills").join("pptx.md"), "global-body").unwrap();
    let agent_skills = home
        .path()
        .join("agents")
        .join("ceo-assistant")
        .join("SKILLS");
    std::fs::create_dir_all(&agent_skills).unwrap();
    std::fs::write(agent_skills.join("pptx.md"), "agent-body").unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_skills_share(json!({ "agent_id": "ceo-assistant", "skill_name": "pptx" }))
        .await;
    assert!(matches!(frame, WsFrame::Response { ok: true, .. }));
    let shared =
        std::fs::read_to_string(home.path().join("shared").join("skills").join("pptx.md"))
            .unwrap();
    assert!(
        shared.contains("agent-body"),
        "agent copy must win: {shared}"
    );
}

#[tokio::test]
async fn share_rejects_traversal_names() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    for (agent, skill) in [
        ("ceo-assistant", "../evil"),
        ("ceo-assistant", ".hidden"),
        ("../escape", "pptx"),
    ] {
        let frame = handler
            .handle_skills_share(json!({ "agent_id": agent, "skill_name": skill }))
            .await;
        assert!(
            matches!(frame, WsFrame::Response { ok: false, .. }),
            "must reject ({agent}, {skill})"
        );
    }
}
