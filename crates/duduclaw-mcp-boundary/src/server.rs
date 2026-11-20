use std::{future::Future, pin::Pin};
use serde_json::Value;
use crate::{RequestContext, VerificationError};
pub use crate::launch_steps::{ContextStep, SessionStep};

pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Implemented by CLI's adapter around McpDispatcher; the gateway cannot depend
/// on the CLI crate. Implementors must retain every legacy dispatch gate.
pub trait Dispatcher: Send + Sync {
    /// Startup admission is fail-closed unless the concrete dispatcher proves
    /// that every non-refused tool has its installed policy and executor.
    fn require_complete_services(&self) -> Result<(), VerificationError> { Err(VerificationError::Unavailable) }

    /// A complete employee-scope implementation may still lack a finite-grant
    /// adapter. Admission must verify the exact activated capability mode.
    fn require_context_services(&self, _context: &RequestContext) -> Result<(), VerificationError> {
        self.require_complete_services()
    }

    fn dispatch<'a>(&'a self, context: RequestContext, agent_slug: &'a str, request: Value)
        -> Pin<Box<dyn Future<Output = Result<Option<Value>, VerificationError>> + Send + 'a>>;
}

/// Bounded JSONL decoder. Batch requests and caller-supplied authority at the
/// envelope level are refused; params are checked by the method adapter.
pub fn decode(line: &[u8]) -> Result<Value, VerificationError> {
    if line.is_empty() || line.len() > MAX_REQUEST_BYTES { return Err(VerificationError::Invalid); }
    let request: Value = serde_json::from_slice(line).map_err(|_| VerificationError::Invalid)?;
    let object = request.as_object().ok_or(VerificationError::Invalid)?;
    if object.keys().any(|key| !matches!(key.as_str(), "jsonrpc" | "id" | "method" | "params"))
        || request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || request.get("method").and_then(Value::as_str).is_none()
        || request.get("id").is_some_and(|id| !(id.is_string() || id.is_i64() || id.is_u64())) {
        return Err(VerificationError::Invalid);
    }
    Ok(request)
}

/// Fixed-size chunk reads; no unbounded read_line allocation. Partial EOF fails.
pub async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R)
    -> Result<Option<Vec<u8>>, VerificationError> {
    read_frame_checked(reader).await.map_err(|(_, error)| error)
}

/// R8z-5: [`read_frame`] with the fixed step that refused.
async fn read_frame_checked<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R)
    -> Result<Option<Vec<u8>>, (SessionStep, VerificationError)> {
    use tokio::io::AsyncBufReadExt;
    let mut frame = Vec::new();
    loop {
        let bytes = reader.fill_buf().await.map_err(|_| (SessionStep::ReadFailed, VerificationError::Unavailable))?;
        if bytes.is_empty() {
            return if frame.is_empty() { Ok(None) } else { Err((SessionStep::FramePartial, VerificationError::Invalid)) };
        }
        let end = bytes.iter().position(|byte| *byte == b'\n');
        let count = end.unwrap_or(bytes.len());
        if count > MAX_REQUEST_BYTES.saturating_sub(frame.len()) {
            return Err((SessionStep::FrameTooLarge, VerificationError::Invalid));
        }
        frame.extend_from_slice(&bytes[..count]);
        reader.consume(count + usize::from(end.is_some()));
        if end.is_some() { return Ok(Some(frame)); }
    }
}

/// Serve exactly one activated endpoint, sequentially. A second in-flight
/// request closes the connection, bounding concurrency and queued work. The
/// request deadline includes approvals. EOF cancels the handler future; a
/// disconnected call cannot publish a later result. No pathname listener exists.
#[cfg(target_os = "linux")]
pub async fn serve(connection: crate::Connection, dispatcher: &dyn Dispatcher)
    -> Result<(), VerificationError> {
    serve_checked(connection, dispatcher).await.map(|_| ()).map_err(|(_, error)| error)
}

/// R8z-5: [`serve`] with the fixed step at which the session ended. `Err`
/// carries exactly `serve`'s error; `Ok(Some(step))` is an `Ok(())` end of
/// `serve` that is still abnormal (the peer closed before any frame, or
/// while a request was being handled); `Ok(None)` is the normal end (the
/// peer closed between requests). Nothing here logs: the owner observes it.
#[cfg(target_os = "linux")]
pub async fn serve_checked(mut connection: crate::Connection, dispatcher: &dyn Dispatcher)
    -> Result<Option<SessionStep>, (SessionStep, VerificationError)> {
    serve_with_deadline_checked(&mut connection, dispatcher,
        std::time::Duration::from_millis(crate::config::CLI_TOOL_TIMEOUT_MS)).await
}

/// HX5 (Q5): whether `frame`, read while request `id` is in flight, is the
/// MCP `notifications/cancelled` for exactly that request. Dropping the call
/// future then runs its cancel path (in-run helpers send `HelperCancel`); MCP
/// sends no response for a cancelled request. Any other frame stays a
/// protocol violation (one request in flight per connection).
pub fn cancels_request(frame: &Value, id: &Value) -> bool {
    frame.get("id").is_none()
        && frame.get("method").and_then(Value::as_str) == Some("notifications/cancelled")
        && frame.get("params").and_then(|params| params.get("requestId"))
            .is_some_and(|request| !id.is_null() && request == id)
}

#[cfg(all(target_os = "linux", test))]
pub(crate) async fn serve_with_deadline(connection: &mut crate::Connection, dispatcher: &dyn Dispatcher,
    request_timeout: std::time::Duration) -> Result<(), VerificationError> {
    serve_with_deadline_checked(connection, dispatcher, request_timeout).await
        .map(|_| ()).map_err(|(_, error)| error)
}

/// R8z-5: the outcome of [`serve_checked`].
#[cfg(target_os = "linux")]
pub(crate) type SessionOutcome = Result<Option<SessionStep>, (SessionStep, VerificationError)>;

#[cfg(target_os = "linux")]
fn at(step: SessionStep) -> impl Fn(VerificationError) -> (SessionStep, VerificationError) {
    move |error| (step, error)
}

#[cfg(target_os = "linux")]
fn context_step((step, error): (ContextStep, VerificationError)) -> (SessionStep, VerificationError) {
    (SessionStep::Context(step), error)
}

#[cfg(target_os = "linux")]
fn dispatch_step(error: VerificationError) -> (SessionStep, VerificationError) {
    (SessionStep::Dispatcher(error), error)
}

/// The serving loop. Every exit is unchanged from before R8z-5 except one
/// (the R8z-5 fix below): each `Err` now also names its [`SessionStep`].
#[cfg(target_os = "linux")]
pub(crate) async fn serve_with_deadline_checked(connection: &mut crate::Connection, dispatcher: &dyn Dispatcher,
    request_timeout: std::time::Duration) -> SessionOutcome {
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use SessionStep as S;
    let expired = (S::RequestTimeout, VerificationError::Unavailable);
    let socket = connection.socket.take().ok_or((S::SocketMissing, VerificationError::Invalid))?;
    socket.set_nonblocking(true).map_err(|_| (S::SocketSetup, VerificationError::Unavailable))?;
    let socket = tokio::net::UnixStream::from_std(socket).map_err(|_| (S::SocketSetup, VerificationError::Unavailable))?;
    let (reader, mut writer) = socket.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    let agent_slug = connection.agent_slug().map_err(at(S::AgentSlug))?;
    let verifier = connection.verifier();
    let mut revoked = connection.revocation().map_err(at(S::Subscribe))?;
    // The error stays `serve`'s `Revoked`. The step names this connection's
    // own live check when one of its context reads revoked the capability
    // (that read signals the revocation before it returns); any other
    // revocation (release, sweep, control loss) is `revoked`.
    let revocation = || (connection.revocation_cause().map_or(S::Revoked, S::Context), VerificationError::Revoked);
    // This future is polled alongside the handler/deadline, never awaited in
    // a selected branch. Slow authority I/O cannot monopolize the reactor.
    // R8z-7: a full verification at most one watch period (100 ms) after the
    // previous one of this session completed — the request reads below count,
    // so the watch no longer runs back to back with them (it used to hold the
    // connection's single authority slot while a request's read queued).
    let watch = async {
        loop {
            verifier.watch_checked().await.map_err(context_step)?;
        }
        #[allow(unreachable_code)]
        Ok::<Option<SessionStep>, (SessionStep, VerificationError)>(None)
    };
    tokio::pin!(watch);
    // Admission uses the same bounded, cancellable authority read as requests.
    let context = tokio::select! {
        biased;
        _ = wait_revoked(&mut revoked) => return Err(revocation()),
        result = &mut watch => return result,
        result = peer_eof(&mut reader) => return result.map(|()| Some(S::PeerClosedBeforeRequest)),
        result = tokio::time::timeout(request_timeout, verifier.context_checked()) =>
            result.map_err(|_| (S::AdmissionTimeout, VerificationError::Unavailable))?.map_err(context_step)?,
    };
    dispatcher.require_context_services(&context).map_err(at(S::Readiness))?;
    let mut window = tokio::time::Instant::now();
    let mut requests = 0u16;
    let mut received = false;
    loop {
        let frame = {
            let read = read_frame_checked(&mut reader);
            tokio::pin!(read);
            loop {
                tokio::select! {
                    biased;
                    _ = wait_revoked(&mut revoked) => return Err(revocation()),
                    result = &mut read => break result?,
                    result = &mut watch => return result,
                }
            }
        };
        // The peer closed between requests: the normal end, unless it never
        // sent anything.
        let Some(frame) = frame else { return Ok((!received).then_some(S::PeerClosedBeforeRequest)); };
        received = true;
        if window.elapsed() >= Duration::from_secs(60) {
            window = tokio::time::Instant::now();
            requests = 0;
        }
        if requests == 120 { return Err((S::WindowLimit, VerificationError::Unavailable)); }
        requests += 1;
        let request = decode(&frame).map_err(at(S::FrameInvalid))?;
        // A notification (no id) is never answered; the peer does not wait
        // for it and may send its next request at once.
        let notification = request.get("id").is_none();
        // A peer that closes while a request is handled or answered loses
        // that answer; a notification has none to lose.
        let in_request = (!notification).then_some(S::PeerClosedInRequest);
        let deadline = tokio::time::sleep(request_timeout);
        tokio::pin!(deadline);
        // R8z-7: a full verification started after this frame was read (it
        // never reuses an earlier one). Its context carries the single-use
        // dispatch proof for this dispatch's turn resolution.
        let context = tokio::select! {
            biased;
            _ = wait_revoked(&mut revoked) => return Err(revocation()),
            result = verifier.dispatch_context_checked() => result.map_err(context_step)?,
            result = &mut watch => return result,
            result = peer_eof(&mut reader) => return result.map(|()| in_request),
            _ = &mut deadline => return Err(expired),
        };
        // R8z-7: an unused proof ends with this request's handler (whichever
        // way this iteration ends), never reaching a later await of it.
        let proof = RetireDispatchProof(context.clone());
        let request_id = request.get("id").cloned().unwrap_or(Value::Null);
        let call = dispatcher.dispatch(context, &agent_slug, request);
        tokio::pin!(call);
        // Immediately ready protocol messages may share a read. Only a call
        // that actually yields can conflict with a second in-flight request.
        let immediate = tokio::select! {
            biased;
            _ = wait_revoked(&mut revoked) => return Err(revocation()),
            result = std::future::poll_fn(|cx| match call.as_mut().poll(cx) {
                std::task::Poll::Ready(value) => std::task::Poll::Ready(Some(value)),
                std::task::Poll::Pending => std::task::Poll::Ready(None),
            }) => result,
        };
        let response = if let Some(result) = immediate { result.map_err(dispatch_step)? } else if notification {
            // R8z-5: a yielding notification handler (every handler of the
            // production dispatcher yields: it resolves the run's turn in
            // the blocking pool) is not a request in flight. The peer's next
            // frame — `tools/list` right after `notifications/initialized` —
            // stays buffered and is read after this handler, never refused
            // as a second in-flight request. Still one handler at a time.
            tokio::select! {
                biased;
                _ = wait_revoked(&mut revoked) => return Err(revocation()),
                result = &mut watch => return result,
                result = &mut call => result.map_err(dispatch_step)?,
                result = peer_eof(&mut reader) => return result.map(|()| in_request),
                _ = &mut deadline => return Err(expired),
            }
        } else {
            let in_flight = tokio::select! {
                biased;
                _ = wait_revoked(&mut revoked) => return Err(revocation()),
                result = &mut watch => return result,
                result = &mut call => Some(result.map_err(dispatch_step)?),
                next = reader.fill_buf() => {
                    // EOF = connection closed mid-call: dropping the call
                    // future cancels it (in-run helpers: `HelperCancel`).
                    if next.map_err(|_| (S::ReadFailed, VerificationError::Unavailable))?.is_empty() {
                        return Ok(in_request);
                    }
                    None
                }
                _ = &mut deadline => return Err(expired),
            };
            match in_flight {
                Some(response) => response,
                None => {
                    let next = tokio::select! {
                        biased;
                        _ = wait_revoked(&mut revoked) => return Err(revocation()),
                        result = read_frame_checked(&mut reader) => result?,
                        _ = &mut deadline => return Err(expired),
                    };
                    let Some(next) = next else { return Ok(in_request); };
                    // HX5 (Q5): `notifications/cancelled` for this request
                    // drops the call (its cancel path runs) and keeps the
                    // connection; MCP answers no cancelled request.
                    if cancels_request(&decode(&next).map_err(at(S::FrameInvalid))?, &request_id) { continue; }
                    return Err((S::SecondRequest, VerificationError::Invalid));
                }
            }
        };
        drop(proof);
        // A response is published only under a full verification started
        // after its handler ended. R8z-7: a handler with nothing to publish
        // (a notification) needs no such read; it was dispatched under its
        // own, and the watch and revocation stay armed while the next frame
        // is read.
        let Some(response) = response else { continue; };
        tokio::select! {
            biased;
            _ = wait_revoked(&mut revoked) => return Err(revocation()),
            result = verifier.context_checked() => { result.map_err(context_step)?; },
            result = &mut watch => return result,
            result = peer_eof(&mut reader) => return result.map(|()| in_request),
            _ = &mut deadline => return Err(expired),
        }
        {
            let bytes = encode_response(&response).map_err(at(S::ResponseEncode))?;
            let write = tokio::time::timeout(Duration::from_secs(5), writer.write_all(&bytes));
            tokio::pin!(write);
            tokio::select! {
                biased;
                _ = wait_revoked(&mut revoked) => return Err(revocation()),
                    result = &mut watch => return result,
                    result = &mut write => {
                        result.map_err(|_| (S::WriteTimeout, VerificationError::Unavailable))?
                            .map_err(|_| (S::WriteFailed, VerificationError::Unavailable))?;
                    }
            }
        }
    }
}

/// R8z-7: consumes the dispatch proof of a request's context when dropped
/// (its handler ended, was cancelled, or the session ended). The proof is
/// shared by every clone of that context, so no clone a handler kept can
/// present it afterwards.
#[cfg(target_os = "linux")]
struct RetireDispatchProof(RequestContext);
#[cfg(target_os = "linux")]
impl Drop for RetireDispatchProof {
    fn drop(&mut self) { let _ = self.0.take_dispatch_proof(); }
}

/// Resolves `Ok(())` at EOF; a read error is `read_failed`.
#[cfg(target_os = "linux")]
async fn peer_eof<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Result<(), (SessionStep, VerificationError)> {
    use tokio::io::AsyncBufReadExt;
    if reader.fill_buf().await.map_err(|_| (SessionStep::ReadFailed, VerificationError::Unavailable))?.is_empty() {
        return Ok(());
    }
    // A buffered next protocol message is not EOF. Preserve it for the normal
    // dispatch stage, which applies the one-in-flight-request rule.
    std::future::pending().await
}

/// Authority-owned sticky cancellation. A closed authority channel also denies.
/// This is independent of the periodic live-store sweep and its timer.
#[cfg(any(target_os = "linux", test))]
pub(crate) async fn wait_revoked(revoked: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *revoked.borrow_and_update() { return; }
        if revoked.changed().await.is_err() { return; }
    }
}

pub fn encode_response(value: &Value) -> Result<Vec<u8>, VerificationError> {
    struct Limited(Vec<u8>);
    impl std::io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_RESPONSE_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut output = Limited(Vec::new());
    serde_json::to_writer(&mut output, value).map_err(|_| VerificationError::Invalid)?;
    output.0.push(b'\n');
    Ok(output.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Incomplete;
    impl Dispatcher for Incomplete {
        fn dispatch<'a>(&'a self, _: RequestContext, _: &'a str, _: Value)
            -> Pin<Box<dyn Future<Output = Result<Option<Value>, VerificationError>> + Send + 'a>> {
            panic!("an incomplete dispatcher must not receive calls")
        }
    }
    fn context() -> RequestContext {
        use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
            EmployeeBinding, Lane, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};
        RequestContext::from_verified_record(CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
            task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
            employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
            tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
            gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
            nonce: [5; 32], jti: [6; 16],
        })
    }
    /// HX5 (Q5): only the cancel notification for the in-flight request id
    /// drops the call; anything else stays a second-request violation.
    #[test]
    fn mcp_cancelled_frame_matches_only_the_in_flight_request() {
        use serde_json::json;
        let cancel = |id: Value| json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id}});
        assert!(cancels_request(&cancel(json!(7)), &json!(7)));
        assert!(cancels_request(&cancel(json!("call")), &json!("call")));
        assert!(!cancels_request(&cancel(json!(8)), &json!(7)));
        assert!(!cancels_request(&cancel(json!("7")), &json!(7)));
        assert!(!cancels_request(&cancel(Value::Null), &Value::Null));
        let mut with_id = cancel(json!(7));
        with_id["id"] = json!(9);
        assert!(!cancels_request(&with_id, &json!(7)));
        assert!(!cancels_request(&json!({"jsonrpc":"2.0","method":"notifications/progress",
            "params":{"requestId":7}}), &json!(7)));
        assert!(!cancels_request(&json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{}}), &json!(7)));
    }

    #[test]
    fn missing_readiness_implementation_refuses_startup() {
        assert_eq!(Incomplete.require_complete_services(), Err(VerificationError::Unavailable));
        assert_eq!(Incomplete.require_context_services(&context()), Err(VerificationError::Unavailable));
    }
    #[test]
    fn default_context_readiness_delegates_to_complete_services() {
        struct Ready(std::sync::atomic::AtomicUsize);
        impl Dispatcher for Ready {
            fn require_complete_services(&self) -> Result<(), VerificationError> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst); Ok(())
            }
            fn dispatch<'a>(&'a self, _: RequestContext, _: &'a str, _: Value)
                -> Pin<Box<dyn Future<Output = Result<Option<Value>, VerificationError>> + Send + 'a>> {
                panic!("readiness must not execute a handler")
            }
        }
        let ready = Ready(std::sync::atomic::AtomicUsize::new(0));
        let dispatcher: &dyn Dispatcher = &ready;
        assert!(dispatcher.require_context_services(&context()).is_ok());
        assert_eq!(ready.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    #[test]
    fn forged_identity_and_batch_frames_are_refused() {
        for line in [r#"[]"#, r#"{"jsonrpc":"2.0","method":"tools/list","principal":"admin"}"#,
            r#"{"jsonrpc":"2.0","method":"tools/list","id":{}}"#] {
            assert!(decode(line.as_bytes()).is_err());
        }
        assert!(decode(br#"{"jsonrpc":"2.0","method":"tools/list","id":1}"#).is_ok());
        assert!(decode(&vec![b' '; MAX_REQUEST_BYTES + 1]).is_err());
    }
    #[tokio::test]
    async fn bounded_frames_reject_overflow_and_partial_eof() {
        for bytes in [vec![b'x'; MAX_REQUEST_BYTES + 1], b"partial".to_vec()] {
            assert!(read_frame(&mut bytes.as_slice()).await.is_err());
        }
        let mut input = b"one\ntwo\n".as_slice();
        assert_eq!(read_frame(&mut input).await.unwrap(), Some(b"one".to_vec()));
        assert_eq!(read_frame(&mut input).await.unwrap(), Some(b"two".to_vec()));
        assert_eq!(read_frame(&mut input).await.unwrap(), None);
    }
    #[test]
    fn output_bound_includes_json_expansion() {
        assert!(encode_response(&Value::String("\0".repeat(MAX_RESPONSE_BYTES / 6 + 1))).is_err());
    }
}
