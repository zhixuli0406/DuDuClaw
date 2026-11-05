//! One host-bound connection. Dropping this future aborts every owned tunnel.
use std::{collections::BTreeMap, net::SocketAddr, sync::Arc, time::{Duration, Instant}};
use duduclaw_isolation::{bounded::List, egress::*, ids::RunId};
use tokio::{io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt}, net::TcpStream, sync::{mpsc, watch}, task::{AbortHandle, JoinSet}};
use crate::{budget::{RunBudget, RunCaps}, policy::{Host, authorize_answers, normalize_host}, resolver::Resolver, transport::CheckedSocket};

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
    let first = reader.read_u8().await.map_err(|_| EgressError::Unavailable)?;
    tokio::time::timeout(Duration::from_millis(limits.idle_timeout_ms.get().into()), async {
        let mut header = [first, 0, 0, 0];
        reader.read_exact(&mut header[1..]).await.map_err(|_| EgressError::Unavailable)?;
        let length = u32::from_be_bytes(header) as usize;
        if length == 0 || length > limits.max_frame_bytes.get() as usize || length > MAX_EGRESS_FRAME_BYTES { return Err(EgressError::InvalidRequest); }
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload).await.map_err(|_| EgressError::Unavailable)?;
        EgressFrame::decode_payload(&payload, limits).map(|f| f.message).map_err(|_| EgressError::InvalidRequest)
    }).await.map_err(|_| EgressError::Timeout)?
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, message: EgressMessage, limits: &EgressLimits) -> Result<(), EgressError> {
    let bytes = EgressFrame { version: EGRESS_PROTOCOL_VERSION, message }.encode_payload(limits).map_err(|_| EgressError::InvalidRequest)?;
    writer.write_u32(bytes.len() as u32).await.map_err(|_| EgressError::Unavailable)?;
    writer.write_all(&bytes).await.map_err(|_| EgressError::Unavailable)
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

#[derive(Debug, serde::Serialize)]
pub struct AuditRecord {
    pub run_id: RunId,
    pub profile_id: ProfileId,
    pub bytes: u64,
    pub connection_attempts: u32,
    pub terminal_code: EgressError,
}

struct Tunnel { input: mpsc::Sender<Vec<u8>>, task: AbortHandle, connected: bool }
enum Event { Input(Result<EgressMessage, EgressError>), Output(EgressMessage) }

pub async fn serve_run(socket: CheckedSocket, run: RunId, profile: EgressProfile, caps: RunCaps, resolver: Arc<dyn Resolver>) -> AuditRecord {
    let (_keepalive, cancel) = watch::channel(false);
    serve_run_cancel(socket, run, profile, caps, resolver, cancel).await
}

pub async fn serve_run_cancel(socket: CheckedSocket, run: RunId, profile: EgressProfile, caps: RunCaps, resolver: Arc<dyn Resolver>, cancel: watch::Receiver<bool>) -> AuditRecord {
    let mut budget = match RunBudget::new(&profile.limits, caps, Instant::now()) {
        Ok(b) => b,
        Err(code) => return AuditRecord { run_id: run, profile_id: profile.id, bytes: 0, connection_attempts: 0, terminal_code: code },
    };
    let result = run_loop(socket, &profile, resolver, &mut budget, cancel).await;
    let (bytes, connection_attempts) = budget.totals();
    AuditRecord { run_id: run, profile_id: profile.id, bytes, connection_attempts,
        terminal_code: result.err().unwrap_or(EgressError::Unavailable) }
}

async fn run_loop(socket: CheckedSocket, profile: &EgressProfile, resolver: Arc<dyn Resolver>, budget: &mut RunBudget, mut cancel: watch::Receiver<bool>) -> Result<(), EgressError> {
    let (mut reader, mut writer) = tokio::io::split(socket);
    let (events, mut incoming) = mpsc::channel(16);
    let mut tasks = JoinSet::new();
    let limits = profile.limits.clone();
    let timeout = Duration::from_millis(limits.idle_timeout_ms.get().into());
    let sender = events.clone();
    tasks.spawn(async move {
        loop {
            let message = read_frame_started(&mut reader, &limits).await;
            let failed = message.is_err();
            if sender.send(Event::Input(message)).await.is_err() || failed { return; }
        }
    });
    let mut tunnels: BTreeMap<u32, Tunnel> = BTreeMap::new();
    let mut highest: Option<u32> = None;
    let mut closed = BTreeMap::new();
    let mut deadline = tokio::time::Instant::now() + timeout;
    let result = tokio::select! {
        _ = cancel.changed() => Err(EgressError::Unavailable),
        result = async { loop {
        let event = tokio::select! {
            event = incoming.recv() => event.ok_or(EgressError::Unavailable)?,
            _ = tokio::time::sleep_until(deadline) => return Err(EgressError::Timeout),
            result = tasks.join_next(), if !tasks.is_empty() => {
                if result.is_some_and(|r| r.is_err_and(|e| !e.is_cancelled())) { return Err(EgressError::Unavailable); }
                continue;
            }
        };
        deadline = tokio::time::Instant::now() + timeout;
        budget.frame(Instant::now())?;
        match event {
            Event::Input(message) => match message? {
                EgressMessage::Connect { tunnel_id, target } => {
                    // Monotonic IDs keep stale task completions from aliasing a new tunnel.
                    if highest.is_some_and(|id| tunnel_id.0 <= id) { return Err(EgressError::InvalidRequest); }
                    highest = Some(tunnel_id.0);
                    budget.open()?;
                    let (tx, rx) = mpsc::channel(4);
                    let events = events.clone();
                    let resolver = resolver.clone();
                    let profile = profile.clone();
                    let task = tasks.spawn(async move { tunnel(tunnel_id, target, profile, resolver, rx, events).await; });
                    tunnels.insert(tunnel_id.0, Tunnel { input: tx, task, connected: false });
                }
                EgressMessage::Data { tunnel_id, bytes } => {
                    if bytes.as_slice().is_empty() { return Err(EgressError::InvalidRequest); }
                    budget.bytes(bytes.as_slice().len(), Instant::now())?;
                    if let Some(tunnel) = tunnels.get(&tunnel_id.0).filter(|t| t.connected) {
                        tunnel.input.try_send(bytes.as_slice().to_vec()).map_err(|_| EgressError::ResourceLimit)?;
                    } else if closed.get(&tunnel_id.0) != Some(&true) {
                        return Err(EgressError::InvalidRequest);
                    }
                    // Remote EOF can cross already-sent client data. Drain only
                    // this closing state; bytes are charged and never forwarded.
                }
                EgressMessage::Close { tunnel_id } => {
                    if let Some(tunnel) = tunnels.remove(&tunnel_id.0) {
                        tunnel.task.abort(); budget.close(); closed.insert(tunnel_id.0, false);
                    } else if let Some(draining) = closed.get_mut(&tunnel_id.0) { *draining = false; }
                    else { return Err(EgressError::InvalidRequest); }
                }
                _ => return Err(EgressError::InvalidRequest),
            },
            Event::Output(message) => {
                let id = match &message {
                    EgressMessage::Connected { tunnel_id } | EgressMessage::Data { tunnel_id, .. }
                    | EgressMessage::Close { tunnel_id } | EgressMessage::Refused { tunnel_id, .. } => tunnel_id.0,
                    _ => return Err(EgressError::InvalidRequest),
                };
                let Some(tunnel) = tunnels.get_mut(&id) else { continue; }; // Cancelled task's queued event.
                match &message {
                    EgressMessage::Connected { .. } => tunnel.connected = true,
                    EgressMessage::Data { bytes, .. } => budget.bytes(bytes.as_slice().len(), Instant::now())?,
                    EgressMessage::Close { .. } => { tunnels.remove(&id); budget.close(); closed.insert(id, true); }
                    EgressMessage::Refused { .. } => { tunnels.remove(&id); budget.close(); closed.insert(id, false); }
                    _ => {}
                }
                tokio::time::timeout(timeout, write_frame(&mut writer, message, &profile.limits)).await.map_err(|_| EgressError::Timeout)??;
            }
        }
    }} => result,
    };
    // Cancellation acknowledgement is sent only after every child future and
    // socket has actually been dropped, including an in-flight DNS/connect.
    tasks.shutdown().await;
    result
}

async fn tunnel(id: TunnelId, target: ConnectTarget, profile: EgressProfile, resolver: Arc<dyn Resolver>, mut input: mpsc::Receiver<Vec<u8>>, events: mpsc::Sender<Event>) {
    let mut socket = match connect(resolver.as_ref(), &target, &profile).await {
        Ok(s) => s,
        Err(code) => { let _ = events.send(Event::Output(EgressMessage::Refused { tunnel_id: id, code })).await; return; }
    };
    if events.send(Event::Output(EgressMessage::Connected { tunnel_id: id })).await.is_err() { return; }
    let size = (profile.limits.max_frame_bytes.get() as usize).saturating_sub(128) / 4;
    let mut buffer = vec![0; size.clamp(1, MAX_TUNNEL_CHUNK_BYTES)];
    let idle = Duration::from_millis(profile.limits.idle_timeout_ms.get().into());
    loop {
        let activity = async {
            tokio::select! {
                bytes = input.recv() => {
                    let bytes = bytes.ok_or(EgressError::Unavailable)?;
                    socket.write_all(&bytes).await.map_err(|_| EgressError::Unavailable)?;
                    Ok::<bool, EgressError>(true)
                }
                read = socket.read(&mut buffer) => {
                    let n = read.map_err(|_| EgressError::Unavailable)?;
                    if n == 0 { return Ok(false); }
                    let bytes = List::new(buffer[..n].to_vec()).map_err(|_| EgressError::ResourceLimit)?;
                    events.send(Event::Output(EgressMessage::Data { tunnel_id: id, bytes })).await.map_err(|_| EgressError::Unavailable)?;
                    Ok(true)
                }
            }
        };
        if !matches!(tokio::time::timeout(idle, activity).await, Ok(Ok(true))) { break; }
    }
    let _ = events.send(Event::Output(EgressMessage::Close { tunnel_id: id })).await;
}
