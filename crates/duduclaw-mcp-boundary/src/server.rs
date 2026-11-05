use std::{future::Future, pin::Pin};
use serde_json::Value;
use crate::{RequestContext, VerificationError};

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
    use tokio::io::AsyncBufReadExt;
    let mut frame = Vec::new();
    loop {
        let bytes = reader.fill_buf().await.map_err(|_| VerificationError::Unavailable)?;
        if bytes.is_empty() {
            return if frame.is_empty() { Ok(None) } else { Err(VerificationError::Invalid) };
        }
        let end = bytes.iter().position(|byte| *byte == b'\n');
        let count = end.unwrap_or(bytes.len());
        if count > MAX_REQUEST_BYTES.saturating_sub(frame.len()) { return Err(VerificationError::Invalid); }
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
pub async fn serve(mut connection: crate::Connection, dispatcher: &dyn Dispatcher)
    -> Result<(), VerificationError> {
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let context = connection.context(crate::authority::now_seconds()?)?;
    dispatcher.require_context_services(&context)?;
    let socket = connection.socket.take().ok_or(VerificationError::Invalid)?;
    socket.set_nonblocking(true).map_err(|_| VerificationError::Unavailable)?;
    let socket = tokio::net::UnixStream::from_std(socket).map_err(|_| VerificationError::Unavailable)?;
    let (reader, mut writer) = socket.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    let agent_slug = connection.agent_slug()?;
    let mut revoked = connection.revocation()?;
    let mut watch = tokio::time::interval(Duration::from_millis(100));
    watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut window = tokio::time::Instant::now();
    let mut requests = 0u16;
    loop {
        let frame = {
            let read = read_frame(&mut reader);
            tokio::pin!(read);
            loop {
                tokio::select! {
                    biased;
                    _ = wait_revoked(&mut revoked) => return Err(VerificationError::Revoked),
                    result = &mut read => break result?,
                    _ = watch.tick() => { connection.context(crate::authority::now_seconds()?)?; }
                }
            }
        };
        let Some(frame) = frame else { return Ok(()); };
        if window.elapsed() >= Duration::from_secs(60) {
            window = tokio::time::Instant::now();
            requests = 0;
        }
        if requests == 120 { return Err(VerificationError::Unavailable); }
        requests += 1;
        let request = decode(&frame)?;
        let context = connection.context(crate::authority::now_seconds()?)?;
        let call = dispatcher.dispatch(context, &agent_slug, request);
        tokio::pin!(call);
        let deadline = tokio::time::sleep(Duration::from_secs(300));
        tokio::pin!(deadline);
        let response = loop {
            tokio::select! {
                biased;
                _ = wait_revoked(&mut revoked) => return Err(VerificationError::Revoked),
                _ = watch.tick() => { connection.context(crate::authority::now_seconds()?)?; }
                result = &mut call => break result?,
                next = reader.fill_buf() => {
                    return if next.map_err(|_| VerificationError::Unavailable)?.is_empty() {
                        Ok(())
                    } else { Err(VerificationError::Invalid) };
                }
                _ = &mut deadline => return Err(VerificationError::Unavailable),
            }
        };
        connection.context(crate::authority::now_seconds()?)?;
        if let Some(response) = response {
            let bytes = encode_response(&response)?;
            let write = tokio::time::timeout(Duration::from_secs(5), writer.write_all(&bytes));
            tokio::pin!(write);
            loop {
                tokio::select! {
                    biased;
                    _ = wait_revoked(&mut revoked) => return Err(VerificationError::Revoked),
                    _ = watch.tick() => { connection.context(crate::authority::now_seconds()?)?; }
                    result = &mut write => {
                        result.map_err(|_| VerificationError::Unavailable)?
                            .map_err(|_| VerificationError::Unavailable)?;
                        break;
                    }
                }
            }
        }
    }
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
