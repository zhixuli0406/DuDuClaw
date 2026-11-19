//! One host-bound connection. Dropping this future aborts every owned tunnel.
use std::{collections::BTreeMap, net::SocketAddr, sync::{Arc, atomic::{AtomicU64, Ordering}}, time::{Duration, Instant}};
use duduclaw_isolation::{bounded::List, egress::*, ids::RunId};
use tokio::{io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt}, net::TcpStream, sync::{mpsc, watch, Semaphore, OwnedSemaphorePermit}, task::{AbortHandle, JoinSet}};
use crate::{budget::{RunBudget, RunCaps, Terminal, TerminalDetail}, policy::{Host, authorize_answers, normalize_host}, resolver::Resolver, transport::CheckedSocket};

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R, limits: &EgressLimits) -> Result<EgressMessage, EgressError> {
    let length = reader.read_u32().await.map_err(|_| EgressError::Unavailable)? as usize;
    if length == 0 || length > limits.max_frame_bytes.get() as usize || length > MAX_EGRESS_FRAME_BYTES { return Err(EgressError::InvalidRequest); }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await.map_err(|_| EgressError::Unavailable)?;
    EgressFrame::decode_payload(&bytes, limits).map(|f| f.message).map_err(|_| EgressError::InvalidRequest)
}
/// No deadline while waiting for the first byte. The caller separately tracks
/// bidirectional connection activity; partial frames always have a deadline.
pub async fn read_frame_started<R: AsyncRead + Unpin>(reader: &mut R, limits: &EgressLimits) -> Result<EgressMessage, EgressError> {
    read_frame_detailed(reader, limits).await.map_err(EgressError::from)
}
fn read_failed(error: std::io::Error) -> Terminal {
    use std::io::ErrorKind;
    let detail = match error.kind() {
        ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset | ErrorKind::BrokenPipe => TerminalDetail::PeerClosed,
        // CheckedSocket: a descriptor-bearing or truncated message.
        ErrorKind::InvalidData => TerminalDetail::InvalidFrame,
        _ => TerminalDetail::Internal,
    };
    Terminal::new(EgressError::Unavailable, detail)
}
/// [`read_frame_started`] with the run record's fixed detail (R8z-6).
async fn read_frame_detailed<R: AsyncRead + Unpin>(reader: &mut R, limits: &EgressLimits) -> Result<EgressMessage, Terminal> {
    let first = reader.read_u8().await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof { Terminal::closed() }
        else { read_failed(error) }
    })?;
    tokio::time::timeout(Duration::from_millis(limits.idle_timeout_ms.get().into()), async {
        let mut header = [first, 0, 0, 0];
        reader.read_exact(&mut header[1..]).await.map_err(read_failed)?;
        let length = u32::from_be_bytes(header) as usize;
        if length == 0 { return Err(Terminal::new(EgressError::InvalidRequest, TerminalDetail::InvalidFrame)); }
        if length > limits.max_frame_bytes.get() as usize || length > MAX_EGRESS_FRAME_BYTES {
            return Err(Terminal::new(EgressError::InvalidRequest, TerminalDetail::FrameTooLarge));
        }
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload).await.map_err(read_failed)?;
        EgressFrame::decode_payload(&payload, limits).map(|f| f.message)
            .map_err(|_| Terminal::new(EgressError::InvalidRequest, TerminalDetail::InvalidFrame))
    }).await.map_err(|_| Terminal::new(EgressError::Timeout, TerminalDetail::FrameTimeout))?
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, message: EgressMessage, limits: &EgressLimits) -> Result<(), EgressError> {
    let bytes = EgressFrame { version: EGRESS_PROTOCOL_VERSION, message }.encode_payload(limits).map_err(|_| EgressError::InvalidRequest)?;
    writer.write_u32(bytes.len() as u32).await.map_err(|_| EgressError::Unavailable)?;
    writer.write_all(&bytes).await.map_err(|_| EgressError::Unavailable)
}

/// One frame to the forwarder, bounded by the idle timeout, with a fixed detail.
async fn write_bounded<W: AsyncWrite + Unpin>(writer: &mut W, message: EgressMessage, limits: &EgressLimits, timeout: Duration) -> Result<(), Terminal> {
    match tokio::time::timeout(timeout, write_frame(writer, message, limits)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(EgressError::Unavailable)) => Err(Terminal::new(EgressError::Unavailable, TerminalDetail::PeerClosed)),
        // Encoding a host-built frame cannot fail for valid limits.
        Ok(Err(code)) => Err(Terminal::new(code, TerminalDetail::Internal)),
        Err(_) => Err(Terminal::new(EgressError::Timeout, TerminalDetail::WriteTimeout)),
    }
}

/// R8z-6: last activity of a two-direction relay, shared by both directions
/// of one task. A direction that is paused (backpressure) is not activity.
pub struct Activity { start: tokio::time::Instant, last_ms: AtomicU64 }
impl Default for Activity {
    fn default() -> Self { Self { start: tokio::time::Instant::now(), last_ms: AtomicU64::new(0) } }
}
impl Activity {
    pub fn touch(&self) {
        let elapsed = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_ms.store(elapsed, Ordering::Relaxed);
    }
    /// Completes once neither direction moved for `idle`.
    pub async fn expired(&self, idle: Duration) {
        loop {
            let deadline = self.start + Duration::from_millis(self.last_ms.load(Ordering::Relaxed)) + idle;
            if tokio::time::Instant::now() >= deadline { return; }
            tokio::time::sleep_until(deadline).await;
        }
    }
}

pub async fn pinned_addresses(resolver: &dyn Resolver, target: &ConnectTarget, profile: &EgressProfile) -> Result<Vec<SocketAddr>, EgressError> {
    target.validate().map_err(|_| EgressError::InvalidRequest)?;
    if !profile.allowed_connect_ports.as_slice().contains(&target.port) { return Err(EgressError::PortDenied); }
    let answers = match normalize_host(target.host.as_str())? {
        Host::Ip(ip) => vec![ip],
        Host::Dns(name) => resolver.resolve(&name, profile.dns_route).await?,
    };
    authorize_answers(target, profile, answers)
}

async fn connect(resolver: &dyn Resolver, target: &ConnectTarget, profile: &EgressProfile) -> Result<TcpStream, EgressError> {
    tokio::time::timeout(Duration::from_millis(profile.limits.connect_timeout_ms.get().into()), async {
        let addresses = pinned_addresses(resolver, target, profile).await?;
        for address in addresses {
            // Numeric address only: this call cannot resolve or follow DNS again.
            if let Ok(socket) = TcpStream::connect(address).await {
                if socket.peer_addr().map_err(|_| EgressError::Unavailable)? != address { return Err(EgressError::DestinationDenied); }
                return Ok(socket);
            }
        }
        Err(EgressError::Unavailable)
    }).await.map_err(|_| EgressError::Timeout)?
}

/// Journal-only code. Error serialization delegates to the unchanged wire enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalTerminalCode { Closed, Error(EgressError) }
impl From<EgressError> for JournalTerminalCode {
    fn from(code: EgressError) -> Self { Self::Error(code) }
}
impl serde::Serialize for JournalTerminalCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Closed => serializer.serialize_str("closed"),
            Self::Error(code) => serde::Serialize::serialize(code, serializer),
        }
    }
}
impl From<Terminal> for JournalTerminalCode {
    fn from(terminal: Terminal) -> Self {
        if terminal.clean_end { Self::Closed } else { Self::Error(terminal.code) }
    }
}

/// One journal line per run. `terminal_detail` (R8z-6) is a fixed label
/// derived where the run ended; it never carries a host, address or content.
#[derive(Debug, serde::Serialize)]
pub struct AuditRecord {
    pub run_id: RunId,
    pub profile_id: ProfileId,
    pub bytes: u64,
    pub connection_attempts: u32,
    pub terminal_code: JournalTerminalCode,
    pub terminal_detail: TerminalDetail,
}

struct Tunnel { input: mpsc::Sender<Vec<u8>>, task: AbortHandle, connected: bool }
enum Event { Input(Result<EgressMessage, Terminal>), Output(EgressMessage) }

pub async fn serve_run(socket: CheckedSocket, run: RunId, profile: EgressProfile, caps: RunCaps, resolver: Arc<dyn Resolver>) -> AuditRecord {
    let (_keepalive, cancel) = watch::channel(false);
    serve_run_cancel(socket, run, profile, caps, resolver, cancel).await
}

pub async fn serve_run_cancel(socket: CheckedSocket, run: RunId, profile: EgressProfile, caps: RunCaps, resolver: Arc<dyn Resolver>, cancel: watch::Receiver<bool>) -> AuditRecord {
    // Standalone callers share the same process-wide ceiling too. The host
    // daemon passes its own authority-owned pool through serve_run_limited.
    static GLOBAL: std::sync::OnceLock<Arc<Semaphore>> = std::sync::OnceLock::new();
    let global = GLOBAL.get_or_init(|| Arc::new(Semaphore::new(crate::budget::MAX_GLOBAL_TUNNELS))).clone();
    serve_run_limited(socket, run, profile, caps, resolver, cancel, global).await
}

pub async fn serve_run_limited(socket: CheckedSocket, run: RunId, profile: EgressProfile, caps: RunCaps, resolver: Arc<dyn Resolver>, cancel: watch::Receiver<bool>, global: Arc<Semaphore>) -> AuditRecord {
    let mut budget = match RunBudget::new(&profile.limits, caps, Instant::now()) {
        Ok(b) => b,
        Err(code) => {
            // Same prompt end for the forwarder as after a served run.
            crate::transport::shutdown_both(socket.raw_fd());
            return AuditRecord { run_id: run, profile_id: profile.id, bytes: 0, connection_attempts: 0,
                terminal_code: code.into(), terminal_detail: TerminalDetail::InvalidLimits };
        }
    };
    let result = run_loop(socket, &profile, resolver, &mut budget, cancel, global).await;
    let (bytes, connection_attempts) = budget.totals();
    let terminal = result.err().unwrap_or(Terminal::closed());
    AuditRecord { run_id: run, profile_id: profile.id, bytes, connection_attempts,
        terminal_code: terminal.into(), terminal_detail: terminal.detail }
}

const fn protocol_state() -> Terminal { Terminal::new(EgressError::InvalidRequest, TerminalDetail::ProtocolState) }

async fn run_loop(socket: CheckedSocket, profile: &EgressProfile, resolver: Arc<dyn Resolver>, budget: &mut RunBudget, mut cancel: watch::Receiver<bool>, global: Arc<Semaphore>) -> Result<(), Terminal> {
    let fd = socket.raw_fd();
    let (mut reader, mut writer) = tokio::io::split(socket);
    let (events, mut incoming) = mpsc::channel(16);
    let mut tasks = JoinSet::new();
    let limits = profile.limits.clone();
    let timeout = Duration::from_millis(limits.idle_timeout_ms.get().into());
    let sender = events.clone();
    tasks.spawn(async move {
        loop {
            let message = read_frame_detailed(&mut reader, &limits).await;
            let failed = message.is_err();
            if sender.send(Event::Input(message)).await.is_err() || failed { return; }
        }
    });
    let mut tunnels: BTreeMap<u32, Tunnel> = BTreeMap::new();
    let mut highest: Option<u32> = None;
    let mut closed = BTreeMap::new();
    let mut deadline = tokio::time::Instant::now() + timeout;
    let result = tokio::select! {
        _ = cancel.changed() => Err(Terminal::new(EgressError::Unavailable, TerminalDetail::Revoked)),
        result = async { loop {
        let event = tokio::select! {
            event = incoming.recv() => event.ok_or(Terminal::new(EgressError::Unavailable, TerminalDetail::Internal))?,
            _ = tokio::time::sleep_until(deadline) => return Err(Terminal::new(EgressError::Timeout, TerminalDetail::IdleTimeout)),
            result = tasks.join_next(), if !tasks.is_empty() => {
                if result.is_some_and(|r| r.is_err_and(|e| !e.is_cancelled())) { return Err(Terminal::new(EgressError::Unavailable, TerminalDetail::Internal)); }
                continue;
            }
        };
        let event = match event {
            Event::Input(message) => Event::Input(Ok(message?)),
            output => output,
        };
        // R8z-6 relay backpressure: every frame takes a frame token and data
        // frames their bytes (both directions, before forwarding). An empty
        // bucket pauses this loop: the forwarder's frames wait in the bounded
        // 16-event queue and its socket, tunnels wait on `events`. The pause
        // is bounded by the idle timeout; only hard caps end the run at once.
        let payload = match &event {
            Event::Input(Ok(EgressMessage::Data { bytes, .. })) => bytes.as_slice().len(),
            Event::Output(EgressMessage::Data { tunnel_id, bytes }) if tunnels.contains_key(&tunnel_id.0) => bytes.as_slice().len(),
            _ => 0,
        };
        budget.throttle(payload, timeout).await?;
        deadline = tokio::time::Instant::now() + timeout;
        match event {
            Event::Input(message) => match message? {
                EgressMessage::Connect { tunnel_id, target } => {
                    // Monotonic IDs keep stale task completions from aliasing a new tunnel.
                    if highest.is_some_and(|id| tunnel_id.0 <= id) { return Err(protocol_state()); }
                    highest = Some(tunnel_id.0);
                    budget.open()?;
                    // Acquire before allocating/spawning the DNS/connect future.
                    // Refuse immediately instead of making an unbounded waiter queue.
                    let permit = match global.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            budget.close();
                            closed.insert(tunnel_id.0, false);
                            write_bounded(&mut writer, EgressMessage::Refused { tunnel_id, code: EgressError::ResourceLimit },
                                &profile.limits, timeout).await?;
                            continue;
                        }
                    };
                    let (tx, rx) = mpsc::channel(4);
                    let events = events.clone();
                    let resolver = resolver.clone();
                    let profile = profile.clone();
                    let task = tasks.spawn(async move { tunnel(tunnel_id, target, profile, resolver, rx, events, permit).await; });
                    tunnels.insert(tunnel_id.0, Tunnel { input: tx, task, connected: false });
                }
                EgressMessage::Data { tunnel_id, bytes } => {
                    if bytes.as_slice().is_empty() { return Err(Terminal::new(EgressError::InvalidRequest, TerminalDetail::InvalidFrame)); }
                    // Bytes were charged by `throttle` above.
                    if let Some(input) = tunnels.get(&tunnel_id.0).filter(|t| t.connected).map(|t| t.input.clone()) {
                        // R8z-6: wait for room in this tunnel's 4-chunk queue
                        // (its upload half drains it into the destination
                        // socket) instead of failing the run when it is full.
                        let sent = tokio::time::timeout(timeout, input.send(bytes.as_slice().to_vec())).await;
                        drop(input);
                        match sent {
                            // Closed: the tunnel task ended; its Close output follows.
                            Ok(_) => {}
                            // The destination accepted nothing for the idle
                            // timeout: end this tunnel alone, like its own Close.
                            Err(_) => {
                                if let Some(tunnel) = tunnels.remove(&tunnel_id.0) { tunnel.task.abort(); }
                                budget.close(); closed.insert(tunnel_id.0, true);
                                write_bounded(&mut writer, EgressMessage::Close { tunnel_id }, &profile.limits, timeout).await?;
                            }
                        }
                    } else if closed.get(&tunnel_id.0) != Some(&true) {
                        return Err(protocol_state());
                    }
                    // Remote EOF can cross already-sent client data. Drain only
                    // this closing state; bytes are charged and never forwarded.
                }
                EgressMessage::Close { tunnel_id } => {
                    if let Some(tunnel) = tunnels.remove(&tunnel_id.0) {
                        tunnel.task.abort(); budget.close(); closed.insert(tunnel_id.0, false);
                    } else if let Some(draining) = closed.get_mut(&tunnel_id.0) { *draining = false; }
                    else { return Err(protocol_state()); }
                }
                _ => return Err(protocol_state()),
            },
            Event::Output(message) => {
                let id = match &message {
                    EgressMessage::Connected { tunnel_id } | EgressMessage::Data { tunnel_id, .. }
                    | EgressMessage::Close { tunnel_id } | EgressMessage::Refused { tunnel_id, .. } => tunnel_id.0,
                    _ => return Err(Terminal::new(EgressError::InvalidRequest, TerminalDetail::Internal)),
                };
                let Some(tunnel) = tunnels.get_mut(&id) else { continue; }; // Cancelled task's queued event.
                match &message {
                    EgressMessage::Connected { .. } => tunnel.connected = true,
                    EgressMessage::Close { .. } => { tunnels.remove(&id); budget.close(); closed.insert(id, true); }
                    EgressMessage::Refused { .. } => { tunnels.remove(&id); budget.close(); closed.insert(id, false); }
                    // Data bytes were charged by `throttle` above.
                    _ => {}
                }
                write_bounded(&mut writer, message, &profile.limits, timeout).await?;
            }
        }
    }} => result,
    };
    // Cancellation acknowledgement is sent only after every child future and
    // socket has actually been dropped, including an in-flight DNS/connect.
    tasks.shutdown().await;
    // R8z-6: spawnd keeps a revoker duplicate of this socketpair end for the
    // run's lifetime (spawnd `channels.rs` RevocablePair), so dropping our
    // descriptor shows the forwarder no EOF; it used to wait for its own
    // 30 s idle timeout. Shut the socket down while `writer` still owns it.
    crate::transport::shutdown_both(fd);
    drop(writer);
    result
}

async fn tunnel(id: TunnelId, target: ConnectTarget, profile: EgressProfile, resolver: Arc<dyn Resolver>, input: mpsc::Receiver<Vec<u8>>, events: mpsc::Sender<Event>, _permit: OwnedSemaphorePermit) {
    let socket = match connect(resolver.as_ref(), &target, &profile).await {
        Ok(s) => s,
        Err(code) => { let _ = events.send(Event::Output(EgressMessage::Refused { tunnel_id: id, code })).await; return; }
    };
    if events.send(Event::Output(EgressMessage::Connected { tunnel_id: id })).await.is_err() { return; }
    relay(socket, id, &profile.limits, input, |message| {
        let events = events.clone();
        async move { events.send(Event::Output(message)).await.is_ok() }
    }).await;
}

/// Raw bytes per data frame: the JSON form (≤ 4 characters per byte plus a
/// fixed envelope under 128) fits `max_frame_bytes`; never above the
/// protocol's [`MAX_TUNNEL_CHUNK_BYTES`].
pub fn chunk_bytes(limits: &EgressLimits) -> usize {
    ((limits.max_frame_bytes.get() as usize).saturating_sub(128) / 4).clamp(1, MAX_TUNNEL_CHUNK_BYTES)
}

/// One connected tunnel, both directions independent (R8z-6): the upload half
/// drains `input` into the destination; the download half reads at most one
/// chunk, then waits until `output` accepted it before reading again. Neither
/// half can stall the other, so the run loop waiting on `input` and this task
/// waiting on the run loop cannot deadlock. Ends on EOF, an error, the input
/// closing, or `idle_timeout_ms` without progress in either direction; then
/// closes `input` (a waiting sender fails at once) and reports Close.
async fn relay<F, Fut>(mut socket: TcpStream, id: TunnelId, limits: &EgressLimits, mut input: mpsc::Receiver<Vec<u8>>, output: F)
where F: Fn(EgressMessage) -> Fut, Fut: std::future::Future<Output = bool> {
    let idle = Duration::from_millis(limits.idle_timeout_ms.get().into());
    let mut buffer = vec![0; chunk_bytes(limits)];
    let activity = Activity::default();
    {
        let (mut reader, mut writer) = socket.split();
        let upload = async {
            while let Some(bytes) = input.recv().await {
                if writer.write_all(&bytes).await.is_err() { return; }
                activity.touch();
            }
        };
        let download = async {
            loop {
                let n = match reader.read(&mut buffer).await { Ok(0) | Err(_) => return, Ok(n) => n };
                activity.touch();
                let Ok(bytes) = List::new(buffer[..n].to_vec()) else { return; };
                if !output(EgressMessage::Data { tunnel_id: id, bytes }).await { return; }
                activity.touch();
            }
        };
        tokio::select! {
            _ = upload => {}
            _ = download => {}
            _ = activity.expired(idle) => {}
        }
    }
    input.close();
    drop(input);
    drop(socket);
    let _ = output(EgressMessage::Close { tunnel_id: id }).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::default_limits;
    use tokio::net::{TcpListener, TcpSocket};

    async fn pair(small_receive_buffer: bool) -> (TcpStream, TcpStream) {
        let socket = TcpSocket::new_v4().unwrap();
        if small_receive_buffer { socket.set_recv_buffer_size(4096).unwrap(); }
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let listener: TcpListener = socket.listen(1).unwrap();
        let client = TcpSocket::new_v4().unwrap();
        if small_receive_buffer { client.set_send_buffer_size(4096).unwrap(); }
        let (client, accepted) = tokio::join!(client.connect(listener.local_addr().unwrap()), listener.accept());
        (client.unwrap(), accepted.unwrap().0)
    }

    /// A 256 KiB upload through one tunnel's bounded queue, with a slow
    /// destination: the sender waits (as the run loop does), nothing fails.
    #[tokio::test]
    async fn relay_upload_of_256_kib_waits_for_a_slow_destination() {
        let (near, mut far) = pair(true).await;
        let (tx, rx) = mpsc::channel(4);
        let (out, mut outputs) = mpsc::channel(16);
        let limits = default_limits();
        let relay = tokio::spawn(async move {
            relay(near, TunnelId(1), &limits, rx, |m| { let out = out.clone(); async move { out.send(m).await.is_ok() } }).await;
        });
        let reader = tokio::spawn(async move {
            let mut total = 0usize; let mut buffer = vec![0; 4096];
            while total < 256 * 1024 {
                let n = far.read(&mut buffer).await.unwrap();
                assert!(n > 0 && buffer[..n].iter().all(|b| *b == 7));
                total += n;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            (total, far)
        });
        for _ in 0..32 {
            tokio::time::timeout(Duration::from_secs(10), tx.send(vec![7; 8192])).await.unwrap().unwrap();
        }
        let (total, far) = tokio::time::timeout(Duration::from_secs(20), reader).await.unwrap().unwrap();
        assert_eq!(total, 256 * 1024);
        drop(far);
        // Destination EOF ends the tunnel with Close.
        let close = tokio::time::timeout(Duration::from_secs(5), async {
            loop { if let Some(EgressMessage::Close { tunnel_id }) = outputs.recv().await { return tunnel_id; } }
        }).await.unwrap();
        assert_eq!(close, TunnelId(1));
        relay.await.unwrap();
    }

    /// A tunnel paused by a destination that never reads is closed by the idle
    /// timeout; the waiting sender is released at once, not left hanging.
    #[tokio::test]
    async fn relay_paused_by_backpressure_is_closed_by_idle_timeout() {
        let (near, far) = pair(true).await;
        let (tx, rx) = mpsc::channel(4);
        let (out, mut outputs) = mpsc::channel(16);
        let mut limits = default_limits();
        limits.idle_timeout_ms = 300.try_into().unwrap();
        let started = tokio::time::Instant::now();
        let relay = tokio::spawn(async move {
            relay(near, TunnelId(3), &limits, rx, |m| { let out = out.clone(); async move { out.send(m).await.is_ok() } }).await;
        });
        let producer = tokio::spawn(async move {
            let mut sent = 0usize;
            while tx.send(vec![1; 8192]).await.is_ok() { sent += 1; if sent > 100_000 { break; } }
            sent
        });
        let sent = tokio::time::timeout(Duration::from_secs(5), producer).await.unwrap().unwrap();
        assert!(sent < 100_000, "the destination never reads: the producer must have been paused");
        assert!(started.elapsed() >= Duration::from_millis(250));
        assert!(matches!(tokio::time::timeout(Duration::from_secs(1), outputs.recv()).await.unwrap(), Some(EgressMessage::Close { tunnel_id: TunnelId(3) })));
        relay.await.unwrap();
        drop(far);
    }

    #[test]
    fn data_frames_never_exceed_max_frame_bytes() {
        for size in [256u32, 1024, 4096, 65536] {
            let mut limits = default_limits();
            limits.max_frame_bytes = size.try_into().unwrap();
            let chunk = chunk_bytes(&limits);
            let frame = EgressFrame { version: EGRESS_PROTOCOL_VERSION, message: EgressMessage::Data {
                tunnel_id: TunnelId(u32::MAX), bytes: List::new(vec![255; chunk]).unwrap() } };
            let payload = frame.encode_payload(&limits).unwrap();
            assert!(payload.len() <= size as usize, "{size}: {}", payload.len());
        }
        assert_eq!(chunk_bytes(&default_limits()), MAX_TUNNEL_CHUNK_BYTES);
    }

    #[tokio::test]
    async fn clean_eof_is_closed_but_interrupted_frame_keeps_error_code() {
        let limits = default_limits();
        for (input, code) in [
            (Vec::new(), JournalTerminalCode::Closed),
            (vec![0], JournalTerminalCode::Error(EgressError::Unavailable)),
            (vec![0, 0, 0, 2, 1], JournalTerminalCode::Error(EgressError::Unavailable)),
        ] {
            let mut reader = input.as_slice();
            let terminal = read_frame_detailed(&mut reader, &limits).await.unwrap_err();
            assert_eq!(terminal.code, EgressError::Unavailable);
            assert_eq!(terminal.detail, TerminalDetail::PeerClosed);
            assert_eq!(JournalTerminalCode::from(terminal), code);
            let record = AuditRecord { run_id: RunId([1; 16]),
                profile_id: ProfileId(duduclaw_isolation::ids::Sha256Digest([2; 32])),
                bytes: 0, connection_attempts: 0, terminal_code: terminal.into(), terminal_detail: terminal.detail };
            let json = serde_json::to_value(record).unwrap();
            assert_eq!(json["terminal_code"], if input.is_empty() { "closed" } else { "unavailable" });
            assert_eq!(json["terminal_detail"], "peer_closed");
        }
    }

    #[test]
    fn journal_error_termination_codes_are_unchanged() {
        for code in [EgressError::InvalidRequest, EgressError::Timeout, EgressError::Unavailable,
            EgressError::ResourceLimit, EgressError::DestinationDenied, EgressError::PortDenied] {
            assert_eq!(serde_json::to_value(JournalTerminalCode::from(code)).unwrap(), serde_json::to_value(code).unwrap());
        }
        let reset = read_failed(std::io::Error::from(std::io::ErrorKind::ConnectionReset));
        assert_eq!(JournalTerminalCode::from(reset), JournalTerminalCode::Error(EgressError::Unavailable));
        assert_eq!(reset.detail, TerminalDetail::PeerClosed);
    }

    /// The run record's detail is a fixed label next to the code.
    #[test]
    fn audit_record_carries_the_fixed_detail() {
        let record = AuditRecord { run_id: RunId([1; 16]), profile_id: ProfileId(duduclaw_isolation::ids::Sha256Digest([2; 32])),
            bytes: 41131, connection_attempts: 1, terminal_code: EgressError::ResourceLimit.into(), terminal_detail: TerminalDetail::TotalBytes };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["terminal_code"], "resource_limit");
        assert_eq!(json["terminal_detail"], "total_bytes");
        for (detail, label) in [(TerminalDetail::FrameRate, "frame_rate"), (TerminalDetail::ByteRate, "byte_rate"),
            (TerminalDetail::TotalConnections, "total_connections"), (TerminalDetail::MaxTunnels, "max_tunnels"),
            (TerminalDetail::FrameTooLarge, "frame_too_large"), (TerminalDetail::IdleTimeout, "idle_timeout"),
            (TerminalDetail::WriteTimeout, "write_timeout"), (TerminalDetail::PeerClosed, "peer_closed"),
            (TerminalDetail::Revoked, "revoked")] {
            assert_eq!(serde_json::to_value(detail).unwrap(), label);
        }
    }
}
