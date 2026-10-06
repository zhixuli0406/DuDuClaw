use super::*;

fn write_binding(home: &Path, entries: &[(&str, &str)]) {
    let mut text = String::from("schema = 1\n");
    for (agent, version) in entries {
        text.push_str(&format!(
            "[agents.{agent}]\npreset_id = \"sales\"\nversion = \"{version}\"\n\
             content_sha256 = \"abc\"\nbound_at = \"2026-10-05T00:00:00Z\"\n\
             bound_by = \"operator\"\nreason = \"test\"\n"
        ));
    }
    std::fs::write(duduclaw_core::preset::bindings_path(home), text).unwrap();
}

fn write_resolved(home: &Path, agent: &str, body: &str) {
    let path = duduclaw_core::preset::agent_resolved_path(home, agent);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

async fn pending_request(home: &Path, broker: &ApprovalBroker) -> ApprovalId {
    let p = json!({"tool":"send","args":{"to":"one"}});
    broker
        .request_bound(RequestKind::Approval, "alice", "send", p.clone(), binding(home, &p, "workflow_v1"))
        .await
        .unwrap()
}

/// Rebinding, unbinding or a changed materialized resolution is policy drift.
#[tokio::test]
async fn preset_rebind_or_resolution_change_refuses_bound_decision() {
    type Mutation = fn(&Path);
    let mutations: [(&str, Mutation); 3] = [
        ("resolution", |h| write_resolved(h, "alice", "[capabilities]\ncomputer_use=false\n")),
        ("rebind", |h| write_binding(h, &[("alice", "2")])),
        ("unbind", |h| write_binding(h, &[])),
    ];
    for (name, mutate) in mutations {
        let home = fixture();
        write_binding(home.path(), &[("alice", "1")]);
        write_resolved(home.path(), "alice", "[capabilities]\ncomputer_use=true\n");
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let before = policy_revision(home.path(), "alice").unwrap();
        let id = pending_request(home.path(), &broker).await;
        mutate(home.path());
        assert_ne!(policy_revision(home.path(), "alice").unwrap(), before, "{name}");
        assert!(broker.decide_bound(&id, &context(), true).await.is_err(), "{name}");
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Pending, "{name}");
    }
    // Control: the same preset-bound setup without a change decides normally.
    let home = fixture();
    write_binding(home.path(), &[("alice", "1")]);
    write_resolved(home.path(), "alice", "[capabilities]\ncomputer_use=true\n");
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let id = pending_request(home.path(), &broker).await;
    assert!(broker.decide_bound(&id, &context(), true).await.is_ok());
}

/// An employee with no preset keeps its revision when other employees bind.
#[tokio::test]
async fn employee_without_preset_is_unaffected_by_other_bindings() {
    let home = fixture();
    let unbound = policy_revision(home.path(), "alice").unwrap();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let id = pending_request(home.path(), &broker).await;
    write_binding(home.path(), &[("bob", "1")]);
    write_resolved(home.path(), "bob", "[capabilities]\ncomputer_use=true\n");
    assert_eq!(policy_revision(home.path(), "alice").unwrap(), unbound);
    write_binding(home.path(), &[("bob", "2")]);
    assert_eq!(policy_revision(home.path(), "alice").unwrap(), unbound);
    assert!(broker.decide_bound(&id, &context(), true).await.is_ok());
}
