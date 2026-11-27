//! Final-answer hygiene and one shared retry allowance per channel turn.
use super::reply_origin::{ReplyAttempt, ReplyFailure, ReplyOrigin};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) fn public_session(session: &str) -> bool {
    crate::odoo_livechat::is_public_visitor_session(session)
        || session.split_once(':').is_some_and(|(kind, id)| kind == "widget-visitor" && !id.is_empty())
        || crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL
            .try_with(|user| crate::webchat::is_widget_visitor(user)).unwrap_or(false)
}

pub(crate) fn retry_session(session: &str) -> bool {
    session.split_once(':').is_some_and(|(kind, id)| kind == "webchat" && !id.is_empty())
        || public_session(session)
}

pub(crate) fn current_session_retries_empty() -> bool {
    duduclaw_memory::feedback::CURRENT_SESSION_ID
        .try_with(|sid| sid.as_deref().is_some_and(retry_session)).unwrap_or(false)
}

pub(super) struct Assembly<'a> {
    session: &'a str,
    home: &'a Path,
    agent_dir: Option<&'a Path>,
    agent: &'a str,
    retried: AtomicBool,
}

impl<'a> Assembly<'a> {
    pub(super) fn new(session: &'a str, home: &'a Path, agent_dir: Option<&'a Path>, agent: &'a str) -> Self {
        Self { session, home, agent_dir, agent, retried: AtomicBool::new(false) }
    }

    pub(super) async fn run<F, Fut>(&self, mut call: F) -> Result<ReplyAttempt, ReplyFailure>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<ReplyAttempt, ReplyFailure>>,
    {
        // Every model call of a channel reply goes through here (Claude CLI,
        // multi-runtime, MoA, local inference, paid Direct API). It is always
        // an employee turn: without that employee's directory nothing runs.
        if self.agent_dir.is_none() {
            return Err(ReplyFailure::simple(ReplyOrigin::Unknown, super::no_employee_spawn_refusal()));
        }
        loop {
            let result = call().await.map(|mut reply| {
                reply.text = duduclaw_core::think_tags::strip_reasoning_prefix(&reply.text);
                reply
            });
            // Parsers may already have removed structured thinking. Their
            // existing empty-response errors carry no raw reasoning, so treat
            // those as no-visible-answer too (also covers genuinely empty runs).
            let empty = match &result {
                Ok(reply) => reply.text.trim().is_empty(),
                Err(error) => error.retryable_empty(),
            };
            let eligible = retry_session(self.session);
            if !empty || !eligible || crate::autonomous_runtime::was_denied()
                || self.retried.swap(true, Ordering::SeqCst)
            {
                return result;
            }
            // Remain inside the caller's cancellation scope. Yield before
            // starting another call so cancellation can drop this future.
            tokio::task::yield_now().await;
            crate::budget::gate_before_spawn(self.home, self.agent_dir, self.agent, "reply_empty_retry")
                .await.map_err(|error| ReplyFailure::simple(ReplyOrigin::Unknown, error.legacy_message()))?;
            // Reinvoke the same boundary: rotation, spawn gates and provider
            // preflight are run again; no direct subprocess bypass here.
        }
    }

    pub(super) async fn text<F, Fut>(&self, origin: ReplyOrigin, mut call: F) -> Result<String, String>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        if !origin.is_claude_cli() {
            crate::on_behalf_of::access::check_final_main_spawn(self.home, self.agent_dir, None).await?;
        }
        self.run(|| {
            let work = call();
            async move {
                work.await.map(|text| ReplyAttempt::new(text, origin))
                    .map_err(|error| ReplyFailure::simple(origin, error))
            }
        }).await.map(|reply| reply.text).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn empty_thinking_retries_once_only_for_web_and_public() {
        for (session, expected) in [("webchat:owner", 2), ("odoolivechat:site:1", 2),
            ("widget-visitor:abc", 2), ("line:owner", 1), ("xwebchat:owner", 1)] {
            let home = tempfile::tempdir().unwrap();
            let assembly = Assembly::new(session, home.path(), Some(home.path()), "");
            let count = AtomicUsize::new(0);
            let reply = assembly.text(ReplyOrigin::Local, || async {
                count.fetch_add(1, Ordering::SeqCst);
                Ok("<thinking>秘密🙂</thinking>".into())
            }).await.unwrap();
            assert!(reply.is_empty());
            assert_eq!(count.load(Ordering::SeqCst), expected);
            // The allowance is shared with all later fallback calls.
            assembly.text(ReplyOrigin::Local, || async {
                count.fetch_add(1, Ordering::SeqCst);
                Ok("<think>秘密</think>".into())
            }).await.unwrap();
            assert_eq!(count.load(Ordering::SeqCst), expected + 1);
        }
    }

    #[tokio::test]
    async fn retry_can_return_visible_cjk_answer_and_stops_on_refusal() {
        let home = tempfile::tempdir().unwrap();
        for refuse in [false, true] {
            let assembly = Assembly::new("webchat:owner", home.path(), Some(home.path()), "");
            let count = AtomicUsize::new(0);
            let result = assembly.text(ReplyOrigin::Local, || async {
                if count.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok("<reasoning>秘密🙂</reasoning>".into())
                } else if refuse {
                    Err(format!("{}denied", duduclaw_agent::mcp_spawn_gate::SPAWN_GATE_ERROR_PREFIX))
                } else {
                    Ok("<think>秘密</think>答案🙂".into())
                }
            }).await;
            if refuse { assert!(result.is_err()); }
            else { assert_eq!(result.unwrap(), "答案🙂"); }
            assert_eq!(count.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn gate_refusal_and_cancellation_errors_are_terminal() {
        let home = tempfile::tempdir().unwrap();
        for error in [format!("{}Empty response", duduclaw_agent::mcp_spawn_gate::SPAWN_GATE_ERROR_PREFIX), "cancelled".into(), format!("{}unavailable", crate::budget::BUDGET_REFUSAL_PREFIX)] {
            let assembly = Assembly::new("webchat:owner", home.path(), Some(home.path()), "");
            let count = AtomicUsize::new(0);
            assert!(assembly.text(ReplyOrigin::Local, || async {
                count.fetch_add(1, Ordering::SeqCst);
                Err(error.clone())
            }).await.is_err());
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn dropping_cancelled_work_does_not_start_retry() {
        use std::future::Future;
        let home = tempfile::tempdir().unwrap();
        let assembly = Assembly::new("webchat:owner", home.path(), Some(home.path()), "");
        let count = AtomicUsize::new(0);
        let mut work = Box::pin(assembly.text(ReplyOrigin::Local, || async {
            count.fetch_add(1, Ordering::SeqCst);
            Ok("<think>秘密</think>".into())
        }));
        std::future::poll_fn(|cx| {
            assert!(work.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        }).await;
        drop(work);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
