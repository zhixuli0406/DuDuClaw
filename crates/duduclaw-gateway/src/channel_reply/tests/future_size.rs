//! Stack-budget regression guard for the channel reply pipeline.
//!
//! Every messaging adapter (LINE, Telegram, Discord, Slack, Google Chat,
//! WebChat, …) awaits `build_guarded_reply_for_agent` or
//! `build_guarded_reply_with_session` inside its own deep chain of
//! task-local scopes. In an unoptimised build each layer that wraps or
//! awaits a future keeps a by-value copy of it in its poll frame, so the
//! size of these futures is multiplied many times on a tokio worker's
//! 2 MiB stack. A debug gateway overflowed that stack on an ordinary LINE
//! message (2026-10-23: main needed more than 1.75 MiB of the 2 MiB for one
//! plain turn). These checks keep the entry futures small and the large
//! futures boxed before they are wrapped.
use super::*;

/// Upper bound for the futures every channel adapter awaits. The heavy turn
/// state lives behind a `Box` (see `guarded.rs`); what remains is the
/// entry's own locals.
const ENTRY_FUTURE_MAX_BYTES: usize = 4 * 1024;

fn test_ctx(home: &std::path::Path) -> ReplyContext {
    let registry = Arc::new(tokio::sync::RwLock::new(
        duduclaw_agent::AgentRegistry::new(home.join("agents")),
    ));
    let sessions = Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
    let status: ChannelStatusMap = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    ReplyContext::new(registry, home.to_path_buf(), sessions, status, tx)
}

fn size_of_future<F: std::future::Future>(f: &F) -> usize {
    std::mem::size_of_val(f)
}

#[test]
fn channel_reply_entry_futures_stay_small() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = test_ctx(dir.path());
    // The futures are only constructed and measured, never polled.
    let for_agent = build_guarded_reply_for_agent("hi", &ctx, "main", "line:U1", "U1", None);
    let with_session = build_guarded_reply_with_session("hi", &ctx, "line:U1", "U1", None);
    let capturing = build_reply_with_session_inner_capturing_operator_result(
        "hi", &ctx, None, "line:U1", "U1", None,
    );
    let inner = build_reply_with_session_inner(
        "hi", &ctx, None, "line:U1", "U1", None,
    );
    let sizes = [
        ("build_guarded_reply_for_agent", size_of_future(&for_agent)),
        ("build_guarded_reply_with_session", size_of_future(&with_session)),
        ("build_reply_with_session_inner_capturing_operator_result", size_of_future(&capturing)),
        ("build_reply_with_session_inner", size_of_future(&inner)),
    ];
    for (name, size) in sizes {
        eprintln!("future size: {name} = {size} bytes");
    }
    for (name, size) in &sizes[..3] {
        assert!(
            *size <= ENTRY_FUTURE_MAX_BYTES,
            "{name} future is {size} bytes (limit {ENTRY_FUTURE_MAX_BYTES}); box the large \
             await inside it instead of raising the limit"
        );
    }
}

/// The boxes that keep the large futures out of the task-local layers are
/// invisible to `size_of_val` (a layer's poll frame, not its state, holds
/// the copies), so pin their shape in the source: the pipeline future is
/// boxed before it is wrapped, and the LINE worker boxes the event future
/// before its own five layers.
#[test]
fn large_reply_futures_are_boxed_before_task_local_layers() {
    let guarded = include_str!("../guarded.rs");
    assert!(
        guarded.contains("let pipeline = Box::pin(build_reply_with_session_inner("),
        "the reply pipeline future must be boxed before the scope chain wraps it"
    );
    assert!(
        guarded.contains("cited.clone(),\n                                        pipeline,"),
        "the innermost task-local layer must wrap the boxed pipeline"
    );
    assert_eq!(
        guarded
            .matches("Box::pin(build_reply_with_session_inner_capturing_operator_result(")
            .count(),
        2,
        "both guarded entry points box the capturing future"
    );
    let worker = include_str!("../../line/ingress/worker.rs");
    assert!(worker.contains("Box::pin(process_line_events("));
    assert!(worker.contains("Box::pin(process_line_decision("));
}
