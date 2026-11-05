//! Live request authority at the HTTP handoff. Every retry invokes this
//! method again; host/operator requests outside isolation keep their policy.
use std::future::Future;

pub(crate) trait LiveRequest {
    fn send_authorized(self) -> impl Future<Output = Result<reqwest::Response, String>> + Send;
}

impl LiveRequest for reqwest::RequestBuilder {
    fn send_authorized(self) -> impl Future<Output = Result<reqwest::Response, String>> + Send {
        let authority = duduclaw_mcp_boundary::io_authority::capture();
        async move {
            authority.scope(async move {
                duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| e.to_string())?;
                self.send().await.map_err(|e| e.without_url().to_string())
            }).await
        }
    }
}

#[cfg(test)]
#[path = "isolation_io_test_support.rs"]
mod isolation_io_test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

    #[tokio::test]
    async fn isolated_http_future_retains_authority_when_polled_outside_parent_scope() {
        let live = Arc::new(AtomicBool::new(true));
        // Invalid URL needs no socket and distinguishes builder validation
        // from the authority refusal which must happen first.
        let request = reqwest::Client::new().post("invalid URL");
        let future = duduclaw_mcp_boundary::io_authority::scope(
            isolation_io_test_support::context(live.clone()), async { request.send_authorized() }).await;
        live.store(false, Ordering::SeqCst);
        assert_eq!(future.await.unwrap_err(), duduclaw_mcp_boundary::VerificationError::Revoked.to_string());
    }
}
