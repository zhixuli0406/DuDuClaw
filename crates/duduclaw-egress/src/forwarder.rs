//! Unprivileged namespace adapter. The host remains the policy authority.
use std::{collections::BTreeMap, net::Ipv4Addr, num::NonZeroU16, time::Duration};
use duduclaw_isolation::{bounded::List, egress::*};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}, sync::mpsc, task::JoinSet};
use crate::{budget::default_limits, http::{parse_connect, HttpError}, session::{read_frame_started, write_frame}, transport::CheckedSocket};

const MAX_CLIENTS: usize = 16;
const MAX_CONNECTIONS: u32 = 4096;
const CONNECTED: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";

enum Event {
    Host(Result<EgressMessage, EgressError>),
    Request { client: u64, target: ConnectTarget },
    Data { client: u64, bytes: Vec<u8> },
    Done { client: u64 },
}
enum Command { Connected, Data(Vec<u8>), Refused(EgressError), Close }
#[derive(Clone, Copy)]
enum State { Header, Pending(u32), Open(u32), Terminal }
struct Client { input: mpsc::Sender<Command>, state: State }
// A local close can cross a response already in the host's output queue.
#[derive(Clone, Copy)]
enum Retired { Pending, Open, Terminal }

pub async fn run(socket: CheckedSocket, port: NonZeroU16) -> Result<(), EgressError> {
    run_with_limits(socket, port, default_limits()).await
}

pub async fn run_with_limits(socket: CheckedSocket, port: NonZeroU16, limits: EgressLimits) -> Result<(), EgressError> {
    limits.validate().map_err(|_| EgressError::InvalidRequest)?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port.get())).await.map_err(|_| EgressError::Unavailable)?;
    run_with_listener(socket, listener, limits).await
}

async fn run_with_listener(socket: CheckedSocket, listener: TcpListener, limits: EgressLimits) -> Result<(), EgressError> {
    let (mut reader, mut writer) = tokio::io::split(socket);
    let (events, mut incoming) = mpsc::channel(32);
    let mut tasks = JoinSet::new();
    let output = events.clone();
    let reader_limits = limits.clone();
    tasks.spawn(async move {
        loop {
            // Never race a partially completed read_exact against actor events.
            // Waiting for the first host byte has no directional idle timer.
            // After it arrives, the helper bounds the whole remaining frame.
            let frame = read_frame_started(&mut reader, &reader_limits).await;
            let failed = frame.is_err();
            if output.send(Event::Host(frame)).await.is_err() || failed { return; }
        }
    });
    let result = forward_loop(listener, &mut writer, &mut incoming, events, &mut tasks, &limits).await;
    tasks.shutdown().await;
    result
}

async fn forward_loop(listener: TcpListener, writer: &mut tokio::io::WriteHalf<CheckedSocket>,
    incoming: &mut mpsc::Receiver<Event>, events: mpsc::Sender<Event>, tasks: &mut JoinSet<()>,
    limits: &EgressLimits) -> Result<(), EgressError> {
    let idle = Duration::from_millis(limits.idle_timeout_ms.get().into());
    let max_clients = usize::from(limits.max_tunnels.get()).min(MAX_CLIENTS);
    let mut deadline = tokio::time::Instant::now() + idle;
    let mut clients: BTreeMap<u64, Client> = BTreeMap::new();
    let mut retired: BTreeMap<u32, Retired> = BTreeMap::new();
    let mut next_client = 0u64;
    let mut next_tunnel = 0u32;
    loop {
        let event = tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return Err(EgressError::Timeout),
            accepted = listener.accept() => {
                let (socket, peer) = accepted.map_err(|_| EgressError::Unavailable)?;
                if peer.ip() != Ipv4Addr::LOCALHOST || clients.len() >= max_clients || tasks.len() > max_clients { continue; }
                deadline = tokio::time::Instant::now() + idle;
                next_client = next_client.checked_add(1).ok_or(EgressError::ResourceLimit)?;
                let client = next_client;
                let (tx, rx) = mpsc::channel(4);
                clients.insert(client, Client { input: tx, state: State::Header });
                let sender = events.clone();
                let limits = limits.clone();
                tasks.spawn(async move {
                    client_session(socket, client, rx, sender.clone(), &limits).await;
                    let _ = sender.send(Event::Done { client }).await;
                });
                continue;
            }
            result = tasks.join_next(), if !tasks.is_empty() => {
                if result.is_some_and(|r| r.is_err()) { return Err(EgressError::Unavailable); }
                continue;
            }
            event = incoming.recv() => event.ok_or(EgressError::Unavailable)?,
        };
        deadline = tokio::time::Instant::now() + idle;
        match event {
            Event::Request { client, target } => {
                let entry = clients.get_mut(&client).ok_or(EgressError::InvalidRequest)?;
                if !matches!(entry.state, State::Header) { return Err(EgressError::InvalidRequest); }
                if next_tunnel == MAX_CONNECTIONS { return Err(EgressError::ResourceLimit); }
                next_tunnel += 1;
                entry.state = State::Pending(next_tunnel);
                send(writer, EgressMessage::Connect { tunnel_id: TunnelId(next_tunnel), target }, limits, idle).await?;
            }
            Event::Data { client, bytes } => {
                let Some(entry) = clients.get(&client) else { continue; };
                let State::Open(id) = entry.state else {
                    if matches!(entry.state, State::Terminal) { continue; }
                    return Err(EgressError::InvalidRequest);
                };
                let bytes = List::new(bytes).map_err(|_| EgressError::ResourceLimit)?;
                send(writer, EgressMessage::Data { tunnel_id: TunnelId(id), bytes }, limits, idle).await?;
            }
            Event::Done { client } => {
                let Some(entry) = clients.remove(&client) else { continue; };
                let (id, state) = match entry.state {
                    State::Pending(id) => (id, Retired::Pending),
                    State::Open(id) => (id, Retired::Open),
                    State::Header | State::Terminal => continue,
                };
                retired.insert(id, state);
                send(writer, EgressMessage::Close { tunnel_id: TunnelId(id) }, limits, idle).await?;
            }
            Event::Host(message) => {
                let message = message?;
                let id = match &message {
                    EgressMessage::Connected { tunnel_id } | EgressMessage::Data { tunnel_id, .. }
                    | EgressMessage::Close { tunnel_id } | EgressMessage::Refused { tunnel_id, .. } => tunnel_id.0,
                    _ => return Err(EgressError::InvalidRequest),
                };
                if let Some(state) = retired.get_mut(&id) {
                    drain_retired(state, &message)?;
                    continue;
                }
                let entry = clients.values_mut().find(|entry| match entry.state {
                    State::Pending(tunnel) | State::Open(tunnel) => tunnel == id,
                    _ => false,
                }).ok_or(EgressError::InvalidRequest)?;
                let command = match message {
                    EgressMessage::Connected { .. } if matches!(entry.state, State::Pending(_)) => {
                        entry.state = State::Open(id);
                        Command::Connected
                    }
                    EgressMessage::Data { bytes, .. } if matches!(entry.state, State::Open(_)) && !bytes.as_slice().is_empty() => {
                        Command::Data(bytes.as_slice().to_vec())
                    }
                    EgressMessage::Close { .. } if matches!(entry.state, State::Open(_)) => {
                        entry.state = State::Terminal;
                        retired.insert(id, Retired::Terminal);
                        Command::Close
                    }
                    EgressMessage::Refused { code, .. } if matches!(entry.state, State::Pending(_)) => {
                        entry.state = State::Terminal;
                        retired.insert(id, Retired::Terminal);
                        Command::Refused(code)
                    }
                    _ => return Err(EgressError::InvalidRequest),
                };
                // A closed receiver's Done event is already queued. Drain host
                // output until the actor processes it and sends Close.
                match entry.input.try_send(command) {
                    Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => return Err(EgressError::ResourceLimit),
                }
            }
        }
    }
}

fn drain_retired(state: &mut Retired, message: &EgressMessage) -> Result<(), EgressError> {
    match (*state, message) {
        (Retired::Pending, EgressMessage::Connected { .. }) => *state = Retired::Open,
        (Retired::Pending, EgressMessage::Refused { .. }) | (Retired::Open, EgressMessage::Close { .. }) => *state = Retired::Terminal,
        (Retired::Open, EgressMessage::Data { bytes, .. }) if !bytes.as_slice().is_empty() => {}
        _ => return Err(EgressError::InvalidRequest),
    }
    Ok(())
}

async fn send<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W, message: EgressMessage,
    limits: &EgressLimits, timeout: Duration) -> Result<(), EgressError> {
    tokio::time::timeout(timeout, write_frame(writer, message, limits)).await.map_err(|_| EgressError::Timeout)?
}

async fn write_client(socket: &mut TcpStream, bytes: &[u8], timeout: Duration) -> Result<(), EgressError> {
    tokio::time::timeout(timeout, socket.write_all(bytes)).await.map_err(|_| EgressError::Timeout)?
        .map_err(|_| EgressError::Unavailable)
}

async fn read_header(socket: &mut TcpStream, limits: &EgressLimits) -> Result<ConnectTarget, HttpError> {
    let mut bytes = Vec::new();
    while bytes.len() < limits.max_header_bytes.get() as usize {
        let byte = socket.read_u8().await.map_err(|_| HttpError::BadRequest)?;
        bytes.push(byte);
        if bytes.ends_with(b"\r\n\r\n") { return parse_connect(&bytes, limits); }
    }
    Err(HttpError::TooLarge)
}

fn refused_response(code: EgressError) -> &'static [u8] {
    match code {
        EgressError::DestinationDenied | EgressError::PortDenied =>
            b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        EgressError::Timeout =>
            b"HTTP/1.1 504 Gateway Timeout\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        EgressError::InvalidRequest => HttpError::BadRequest.response(),
        EgressError::ResourceLimit | EgressError::Unavailable =>
            b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
    }
}

async fn client_session(mut socket: TcpStream, client: u64, mut input: mpsc::Receiver<Command>,
    events: mpsc::Sender<Event>, limits: &EgressLimits) {
    let idle = Duration::from_millis(limits.idle_timeout_ms.get().into());
    let connect_timeout = Duration::from_millis(limits.connect_timeout_ms.get().into());
    let target = match tokio::time::timeout(connect_timeout, read_header(&mut socket, limits)).await {
        Ok(Ok(target)) => target,
        Ok(Err(error)) => { let _ = write_client(&mut socket, error.response(), idle).await; return; }
        Err(_) => { let _ = write_client(&mut socket, refused_response(EgressError::Timeout), idle).await; return; }
    };
    if events.send(Event::Request { client, target }).await.is_err() { return; }
    match tokio::time::timeout(connect_timeout, input.recv()).await {
        Ok(Some(Command::Connected)) => {
            if write_client(&mut socket, CONNECTED, idle).await.is_err() { return; }
        }
        Ok(Some(Command::Refused(code))) => { let _ = write_client(&mut socket, refused_response(code), idle).await; return; }
        _ => { let _ = write_client(&mut socket, refused_response(EgressError::Unavailable), idle).await; return; }
    }
    let chunk = (limits.max_frame_bytes.get() as usize).saturating_sub(128) / 4;
    let mut buffer = vec![0; chunk.clamp(1, MAX_TUNNEL_CHUNK_BYTES)];
    loop {
        let activity = async {
            tokio::select! {
                command = input.recv() => match command {
                    Some(Command::Data(bytes)) => {
                        socket.write_all(&bytes).await.map_err(|_| EgressError::Unavailable)?;
                        Ok(true)
                    }
                    Some(Command::Close) | None => Ok(false),
                    _ => Err(EgressError::InvalidRequest),
                },
                read = socket.read(&mut buffer) => {
                    let size = read.map_err(|_| EgressError::Unavailable)?;
                    if size == 0 { return Ok(false); }
                    events.send(Event::Data { client, bytes: buffer[..size].to_vec() }).await.map_err(|_| EgressError::Unavailable)?;
                    Ok(true)
                }
            }
        };
        if !matches!(tokio::time::timeout(idle, activity).await, Ok(Ok(true))) { return; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::read_frame;

    async fn fixture() -> (std::net::SocketAddr, tokio::net::UnixStream, tokio::task::JoinHandle<Result<(), EgressError>>) {
        fixture_with_limits(default_limits()).await
    }

    async fn fixture_with_limits(limits: EgressLimits) -> (std::net::SocketAddr, tokio::net::UnixStream, tokio::task::JoinHandle<Result<(), EgressError>>) {
        let (forwarder, host) = std::os::unix::net::UnixStream::pair().unwrap();
        host.set_nonblocking(true).unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(run_with_listener(CheckedSocket::new(forwarder.into()).unwrap(), listener, limits));
        (address, tokio::net::UnixStream::from_std(host).unwrap(), task)
    }

    #[tokio::test]
    async fn forwarder_get_is_405_without_host_connect() {
        let (address, mut host, task) = fixture().await;
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(b"GET http://example.com/ HTTP/1.1\r\n\r\n").await.unwrap();
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut bytes)).await.unwrap().unwrap();
        assert_eq!(bytes, HttpError::MethodNotAllowed.response());
        assert!(tokio::time::timeout(Duration::from_millis(30), read_frame(&mut host, &default_limits())).await.is_err());
        task.abort();
    }

    #[tokio::test]
    async fn forwarder_connect_and_bidirectional_tcp_bytes() {
        let (address, mut host, task) = fixture().await;
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\nhello").await.unwrap();
        let tunnel_id = match read_frame(&mut host, &default_limits()).await.unwrap() {
            EgressMessage::Connect { tunnel_id, .. } => tunnel_id,
            _ => panic!("expected CONNECT"),
        };
        assert_eq!(tunnel_id.0, 1);
        write_frame(&mut host, EgressMessage::Connected { tunnel_id }, &default_limits()).await.unwrap();
        let mut response = vec![0; CONNECTED.len()];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(response, CONNECTED);
        match read_frame(&mut host, &default_limits()).await.unwrap() {
            EgressMessage::Data { bytes, .. } => assert_eq!(bytes.as_slice(), b"hello"),
            _ => panic!("expected tunnel data"),
        }
        write_frame(&mut host, EgressMessage::Data { tunnel_id, bytes: List::new(b"world".to_vec()).unwrap() }, &default_limits()).await.unwrap();
        let mut response = [0; 5];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(&response, b"world");
        write_frame(&mut host, EgressMessage::Close { tunnel_id }, &default_limits()).await.unwrap();
        assert_eq!(client.read_u8().await.unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
        task.abort();
    }

    #[tokio::test]
    async fn forwarder_unknown_host_tunnel_closes_connection() {
        let (address, mut host, task) = fixture().await;
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(b"CONNECT example.com:443 HTTP/1.1\r\n\r\n").await.unwrap();
        assert!(matches!(read_frame(&mut host, &default_limits()).await.unwrap(), EgressMessage::Connect { .. }));
        write_frame(&mut host, EgressMessage::Connected { tunnel_id: TunnelId(42) }, &default_limits()).await.unwrap();
        assert_eq!(tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap(), Err(EgressError::InvalidRequest));
        assert_eq!(client.read_u8().await.unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn forwarder_upload_resets_shared_idle_and_honors_small_frames() {
        let mut limits = default_limits();
        limits.idle_timeout_ms = 500.try_into().unwrap();
        limits.max_frame_bytes = 256.try_into().unwrap();
        let (address, mut host, task) = fixture_with_limits(limits.clone()).await;
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(b"CONNECT example.com:443 HTTP/1.1\r\n\r\n").await.unwrap();
        let tunnel_id = match read_frame(&mut host, &limits).await.unwrap() {
            EgressMessage::Connect { tunnel_id, .. } => tunnel_id,
            _ => panic!("expected CONNECT"),
        };
        write_frame(&mut host, EgressMessage::Connected { tunnel_id }, &limits).await.unwrap();
        client.read_exact(&mut vec![0; CONNECTED.len()]).await.unwrap();
        // Upload lasts longer than the idle timeout, with no host responses.
        for _ in 0..7 {
            client.write_all(&[255; 100]).await.unwrap();
            let mut received = 0;
            while received < 100 {
                match read_frame(&mut host, &limits).await.unwrap() {
                    EgressMessage::Data { bytes, .. } => {
                        assert!(bytes.as_slice().len() <= 32);
                        received += bytes.as_slice().len();
                    }
                    _ => panic!("expected upload"),
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!task.is_finished());
        drop(host);
        assert_eq!(tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap(), Err(EgressError::Unavailable));
        assert_eq!(client.read_u8().await.unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn forwarder_custom_header_limit_is_enforced() {
        let mut limits = default_limits();
        limits.max_header_bytes = 32.try_into().unwrap();
        let (address, _host, task) = fixture_with_limits(limits).await;
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(b"CONNECT example.com:443 HTTP/1.1\r\n\r\n").await.unwrap();
        let mut bytes = vec![0; HttpError::TooLarge.response().len()];
        tokio::time::timeout(Duration::from_secs(1), client.read_exact(&mut bytes)).await.unwrap().unwrap();
        assert_eq!(bytes, HttpError::TooLarge.response());
        task.abort();
    }

    #[tokio::test]
    async fn forwarder_ids_follow_completed_headers_and_partial_host_reads_survive() {
        let (address, mut host, task) = fixture().await;
        let mut slow = TcpStream::connect(address).await.unwrap();
        slow.write_all(b"CONNECT slow.example:443 HTTP/1.1\r\n").await.unwrap();
        let mut fast = TcpStream::connect(address).await.unwrap();
        fast.write_all(b"CONNECT fast.example:443 HTTP/1.1\r\n\r\n").await.unwrap();
        match read_frame(&mut host, &default_limits()).await.unwrap() {
            EgressMessage::Connect { tunnel_id, target } => {
                assert_eq!(tunnel_id.0, 1); assert_eq!(target.host.as_str(), "fast.example");
            }
            _ => panic!("expected first CONNECT"),
        }
        let payload = EgressFrame { version: 1, message: EgressMessage::Connected { tunnel_id: TunnelId(1) } }
            .encode_payload(&default_limits()).unwrap();
        let prefix = (payload.len() as u32).to_be_bytes();
        host.write_all(&prefix[..2]).await.unwrap();
        slow.write_all(b"\r\n").await.unwrap();
        match read_frame(&mut host, &default_limits()).await.unwrap() {
            EgressMessage::Connect { tunnel_id, target } => {
                assert_eq!(tunnel_id.0, 2); assert_eq!(target.host.as_str(), "slow.example");
            }
            _ => panic!("expected second CONNECT"),
        }
        host.write_all(&prefix[2..]).await.unwrap();
        host.write_all(&payload).await.unwrap();
        let mut response = vec![0; CONNECTED.len()];
        tokio::time::timeout(Duration::from_secs(1), fast.read_exact(&mut response)).await.unwrap().unwrap();
        assert_eq!(response, CONNECTED);
        task.abort();
    }

    #[test]
    fn forwarder_cancellation_tail_is_bounded_by_state() {
        let mut state = Retired::Pending;
        let connected = EgressMessage::Connected { tunnel_id: TunnelId(1) };
        drain_retired(&mut state, &connected).unwrap();
        assert!(drain_retired(&mut state, &connected).is_err());
        drain_retired(&mut state, &EgressMessage::Close { tunnel_id: TunnelId(1) }).unwrap();
        assert!(drain_retired(&mut state, &EgressMessage::Close { tunnel_id: TunnelId(1) }).is_err());
    }
}
