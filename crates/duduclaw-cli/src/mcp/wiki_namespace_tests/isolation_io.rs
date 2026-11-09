use super::*;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}, mpsc};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, LiveRequestVerifier, Principal, RequestContext, VerificationError},
    ids::{EmployeeId, RunId, Sha256Digest}};

struct Live {
    active: Arc<AtomicBool>,
    admitted: Mutex<Option<mpsc::Sender<()>>>,
}
impl LiveRequestVerifier for Live {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> std::result::Result<(), VerificationError> {
        let allowed = self.active.load(Ordering::SeqCst);
        if let Some(sender) = self.admitted.lock().unwrap().take() { sender.send(()).unwrap(); }
        if allowed { Ok(()) } else { Err(VerificationError::Revoked) }
    }
}
fn context(active: Arc<AtomicBool>, admitted: Option<mpsc::Sender<()>>) -> RequestContext {
    RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
        gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [0; 32], jti: [0; 16],
    }, Arc::new(Live { active, admitted: Mutex::new(admitted) }))
}

#[tokio::test(flavor = "current_thread")]
async fn revoked_after_initial_admission_cannot_replace_wiki_page_after_delivery_fence() {
    let home = TempDir::new();
    create_agent_dir(home.path(), "alice");
    let original = serde_json::json!({"page_path":"notes/hello.md", "content":"---\ntitle: Hello\n---\noriginal"});
    let written = handle_wiki_write(&original, home.path(), "alice").await;
    assert!(!written["isError"].as_bool().unwrap_or(false));
    let wiki = home.path().join("agents/alice/wiki");
    let paths = [wiki.join("notes/hello.md"), wiki.join("_index.md"), wiki.join("_log.md")];
    let before: Vec<_> = paths.iter().map(|path| fs::read(path).unwrap()).collect();
    let lease = duduclaw_memory::WikiDeliveryFence::for_agent_wiki(home.path(), "alice").try_shared().unwrap();
    let active = Arc::new(AtomicBool::new(true));
    let (admitted, wait) = mpsc::channel();
    let revoke = active.clone();
    let releaser = std::thread::spawn(move || {
        wait.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        revoke.store(false, Ordering::SeqCst);
        drop(lease);
    });
    let replacement = serde_json::json!({"page_path":"notes/hello.md", "content":"---\ntitle: Changed\n---\nreplacement"});
    let result = duduclaw_mcp_boundary::io_authority::scope(context(active, Some(admitted)),
        handle_wiki_write(&replacement, home.path(), "alice")).await;
    releaser.join().unwrap();
    assert!(result["isError"].as_bool().unwrap_or(false));
    for (path, bytes) in paths.iter().zip(before) { assert_eq!(fs::read(path).unwrap(), bytes); }
}

#[tokio::test]
async fn already_revoked_wiki_write_creates_no_agent_or_scaffold() {
    let home = TempDir::new();
    let args = serde_json::json!({"page_path":"notes/hello.md", "content":"refused"});
    let result = duduclaw_mcp_boundary::io_authority::scope(context(Arc::new(AtomicBool::new(false)), None),
        handle_wiki_write(&args, home.path(), "missing-agent")).await;
    assert!(result["isError"].as_bool().unwrap_or(false));
    assert!(!home.path().join("agents/missing-agent").exists());
}

#[tokio::test]
async fn already_revoked_shared_write_creates_no_scaffold() {
    let home = TempDir::new();
    let result = duduclaw_mcp_boundary::io_authority::scope(
        context(Arc::new(AtomicBool::new(false)), None), async {
            ensure_shared_wiki_dir(&home.path().join("shared/wiki"))
        }).await;
    assert!(result.is_err());
    assert!(!home.path().join("shared/wiki").exists());
}
