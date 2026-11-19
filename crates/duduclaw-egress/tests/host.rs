#![cfg(target_os = "linux")]
use std::{collections::VecDeque, net::IpAddr, os::fd::OwnedFd, sync::{Arc, Mutex}, time::Duration};
use duduclaw_egress::{budget::{default_limits, RunCaps, TerminalDetail}, policy::authorize_answers, resolver::Resolver, session::{pinned_addresses, read_frame, serve_run, write_frame}, transport::{CheckedSocket, socket_info}};
use duduclaw_isolation::{bounded::{List, Text}, egress::*, ids::{RunId, Sha256Digest}};
use tokio::io::AsyncWriteExt;

fn profile() -> EgressProfile {
    EgressProfile { id: ProfileId(Sha256Digest([1;32])), allowed_connect_ports: List::new(vec![443.try_into().unwrap()]).unwrap(), dns_route: DnsRoute::ResolvedVarlink, limits: default_limits() }
}
fn target() -> ConnectTarget { ConnectTarget { host: Text::new("example.com".into()).unwrap(), port: 443.try_into().unwrap() } }
struct FakeResolver(Mutex<VecDeque<Vec<IpAddr>>>);
impl Resolver for FakeResolver {
    fn resolve<'a>(&'a self, _: &'a str, _: DnsRoute) -> std::pin::Pin<Box<dyn std::future::Future<Output=Result<Vec<IpAddr>,EgressError>> + Send + 'a>> {
        Box::pin(async move { self.0.lock().unwrap().pop_front().ok_or(EgressError::Unavailable) })
    }
}
fn sockets() -> (CheckedSocket, CheckedSocket) {
    let (a,b) = std::os::unix::net::UnixStream::pair().unwrap();
    (CheckedSocket::new(OwnedFd::from(a)).unwrap(), CheckedSocket::new(OwnedFd::from(b)).unwrap())
}
#[tokio::test]
async fn dns_rebinding_is_checked_each_tunnel_and_public_answer_is_pinned() {
    let fake = FakeResolver(Mutex::new(VecDeque::from([vec!["8.8.8.8".parse().unwrap()], vec!["127.0.0.1".parse().unwrap()]])));
    let first = pinned_addresses(&fake, &target(), &profile()).await.unwrap();
    assert_eq!(first, vec!["8.8.8.8:443".parse().unwrap()]);
    assert_eq!(pinned_addresses(&fake, &target(), &profile()).await, Err(EgressError::DestinationDenied));
    assert_eq!(first[0], "8.8.8.8:443".parse().unwrap());
    assert_eq!(authorize_answers(&target(), &profile(), vec!["8.8.8.8".parse().unwrap(), "fd00:ec2::254".parse().unwrap()]), Err(EgressError::DestinationDenied));
}
#[tokio::test]
async fn malformed_frames_wrong_direction_and_state_close_run() {
    for raw in [b"{\"version\":1,\"run_id\":\"other\",\"message\":{\"type\":\"close\",\"tunnel_id\":1}}".as_slice(),
        b"{\"version\":1,\"message\":{\"type\":\"connected\",\"tunnel_id\":1}}",
        b"{\"version\":1,\"message\":{\"type\":\"data\",\"tunnel_id\":1,\"bytes\":[1]}}"] {
        let (mut client, host) = sockets();
        let task = tokio::spawn(serve_run(host, RunId([1;16]), profile(), RunCaps {total_bytes: 100, total_connections: 2}, Arc::new(FakeResolver(Mutex::new(VecDeque::new())))));
        client.write_u32(raw.len() as u32).await.unwrap(); client.write_all(raw).await.unwrap();
        let audit = tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap();
        assert_eq!(audit.terminal_code, EgressError::InvalidRequest);
        assert_eq!(audit.connection_attempts, 0);
    }
}
#[tokio::test]
async fn metadata_is_refused_without_network_and_audit_has_no_target() {
    let (mut client, host) = sockets();
    let task = tokio::spawn(serve_run(host, RunId([1;16]), profile(), RunCaps {total_bytes: 100, total_connections: 2}, Arc::new(FakeResolver(Mutex::new(VecDeque::new())))));
    let mut t = target(); t.host = Text::new("169.254.169.254".into()).unwrap();
    write_frame(&mut client, EgressMessage::Connect {tunnel_id:TunnelId(1),target:t}, &default_limits()).await.unwrap();
    assert!(matches!(read_frame(&mut client, &default_limits()).await.unwrap(), EgressMessage::Refused {code:EgressError::DestinationDenied,..}));
    drop(client);
    let audit = task.await.unwrap();
    assert_eq!(audit.connection_attempts, 1);
    let json = serde_json::to_string(&audit).unwrap();
    assert!(!json.contains("169.254")); assert!(!json.contains("example.com"));
}
#[tokio::test]
async fn stream_rejects_ancillary_fds_and_bounded_lengths() {
    use std::{io::IoSlice, os::fd::AsRawFd};
    use nix::sys::socket::{sendmsg, ControlMessage, MsgFlags};
    let (a,b) = std::os::unix::net::UnixStream::pair().unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    sendmsg::<()>(a.as_raw_fd(), &[IoSlice::new(&[0,0,0,1,b'x'])], &[ControlMessage::ScmRights(&[file.as_raw_fd()])], MsgFlags::empty(), None).unwrap();
    let mut receiver = CheckedSocket::new(b.into()).unwrap();
    assert!(read_frame(&mut receiver, &default_limits()).await.is_err());
    let (mut a, mut b) = sockets();
    a.write_u32(u32::MAX).await.unwrap();
    assert_eq!(read_frame(&mut b, &default_limits()).await, Err(EgressError::InvalidRequest));
}
#[test]
fn socket_validation_rejects_files_and_named_or_wrong_kind_sockets() {
    let fd: OwnedFd = std::fs::File::open("/dev/null").unwrap().into();
    assert!(socket_info(&fd, libc::SOCK_STREAM, false, true).is_err());
    let (a,_) = std::os::unix::net::UnixStream::pair().unwrap();
    let fd = OwnedFd::from(a);
    assert!(socket_info(&fd, libc::SOCK_STREAM, false, true).is_ok());
    assert!(socket_info(&fd, libc::SOCK_SEQPACKET, false, false).is_err());
}

/// Requires root on Linux and an operator-controlled PUBLIC TCP echo server.
/// This exercises real forwarding, not TLS or live product/provider acceptance.
#[tokio::test]
#[ignore = "root Linux VM and ISO_EGRESS_ECHO_ADDR public echo endpoint required"]
async fn iso_root_live_forwarding() {
    assert_eq!(nix::unistd::geteuid().as_raw(), 0);
    let address: std::net::SocketAddr = std::env::var("ISO_EGRESS_ECHO_ADDR").expect("public controlled echo address").parse().unwrap();
    assert!(duduclaw_egress::policy::is_public_ip(address.ip()));
    let mut p = profile(); p.allowed_connect_ports = List::new(vec![address.port().try_into().unwrap()]).unwrap();
    let limits = p.limits.clone();
    let (mut client, host) = sockets();
    let task = tokio::spawn(serve_run(host, RunId([7;16]), p, RunCaps {total_bytes: 1024,total_connections: 1}, Arc::new(FakeResolver(Mutex::new(VecDeque::new())))));
    let target = ConnectTarget {host:Text::new(address.ip().to_string()).unwrap(),port:address.port().try_into().unwrap()};
    write_frame(&mut client,EgressMessage::Connect{tunnel_id:TunnelId(1),target},&limits).await.unwrap();
    assert!(matches!(read_frame(&mut client,&limits).await.unwrap(),EgressMessage::Connected{..}));
    write_frame(&mut client,EgressMessage::Data{tunnel_id:TunnelId(1),bytes:List::new(b"iso-c-echo".to_vec()).unwrap()},&limits).await.unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5),async {
        let mut response=Vec::new();
        while response.len()<b"iso-c-echo".len() {
            match read_frame(&mut client,&limits).await.unwrap() {
                EgressMessage::Data{tunnel_id:TunnelId(1),bytes}=>response.extend_from_slice(bytes.as_slice()),
                other=>panic!("unexpected {other:?}"),
            }
        }
        response
    }).await.unwrap();
    assert_eq!(response,b"iso-c-echo");
    drop(client); task.await.unwrap();
}

#[tokio::test]
async fn cancelling_pending_tunnel_does_not_kill_other_tunnels() {
    struct Waiting;
    impl Resolver for Waiting {
        fn resolve<'a>(&'a self, _: &'a str, _: DnsRoute) -> std::pin::Pin<Box<dyn std::future::Future<Output=Result<Vec<IpAddr>,EgressError>> + Send + 'a>> {
            Box::pin(std::future::pending())
        }
    }
    let (mut client, host) = sockets();
    let task = tokio::spawn(serve_run(host, RunId([1;16]), profile(), RunCaps {total_bytes:100,total_connections:3},Arc::new(Waiting)));
    write_frame(&mut client,EgressMessage::Connect{tunnel_id:TunnelId(1),target:target()},&default_limits()).await.unwrap();
    write_frame(&mut client,EgressMessage::Close{tunnel_id:TunnelId(1)},&default_limits()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let mut t=target(); t.host=Text::new("127.0.0.1".into()).unwrap();
    write_frame(&mut client,EgressMessage::Connect{tunnel_id:TunnelId(2),target:t.clone()},&default_limits()).await.unwrap();
    assert!(matches!(read_frame(&mut client,&default_limits()).await.unwrap(),EgressMessage::Refused{tunnel_id:TunnelId(2),..}));
    // A local timeout/close can cross the host refusal.
    write_frame(&mut client,EgressMessage::Close{tunnel_id:TunnelId(2)},&default_limits()).await.unwrap();
    write_frame(&mut client,EgressMessage::Connect{tunnel_id:TunnelId(3),target:t},&default_limits()).await.unwrap();
    assert!(matches!(read_frame(&mut client,&default_limits()).await.unwrap(),EgressMessage::Refused{tunnel_id:TunnelId(3),..}));
    drop(client); assert_eq!(task.await.unwrap().connection_attempts,3);
}

#[tokio::test]
async fn idle_run_and_partial_frame_time_out_but_idle_direction_does_not() {
    use duduclaw_egress::session::read_frame_started;
    let mut p=profile(); p.limits.idle_timeout_ms=20.try_into().unwrap();
    let (_client,host)=sockets();
    let task=tokio::spawn(serve_run(host,RunId([1;16]),p.clone(),RunCaps{total_bytes:100,total_connections:1},Arc::new(FakeResolver(Mutex::new(VecDeque::new())))));
    assert_eq!(tokio::time::timeout(Duration::from_secs(1),task).await.unwrap().unwrap().terminal_code,EgressError::Timeout);
    let (mut a,mut b)=sockets();
    let limits=p.limits.clone();
    let task=tokio::spawn(async move {read_frame_started(&mut b,&limits).await});
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(!task.is_finished());
    a.write_all(&[0]).await.unwrap();
    assert_eq!(tokio::time::timeout(Duration::from_secs(1),task).await.unwrap().unwrap(),Err(EgressError::Timeout));
}

#[tokio::test]
async fn unregister_cancellation_waits_for_pending_dns_to_drop() {
    use std::sync::atomic::{AtomicBool,Ordering};
    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {fn drop(&mut self){self.0.store(true,Ordering::SeqCst);}}
    struct Waiting {started: Arc<tokio::sync::Notify>, dropped: Arc<AtomicBool>}
    impl Resolver for Waiting {
        fn resolve<'a>(&'a self,_:&'a str,_:DnsRoute)->std::pin::Pin<Box<dyn std::future::Future<Output=Result<Vec<IpAddr>,EgressError>>+Send+'a>> {
            Box::pin(async move {
                let _guard=DropFlag(self.dropped.clone());
                self.started.notify_one();
                std::future::pending().await
            })
        }
    }
    let started=Arc::new(tokio::sync::Notify::new());
    let dropped=Arc::new(AtomicBool::new(false));
    let (mut client,host)=sockets();
    let (cancel,receiver)=tokio::sync::watch::channel(false);
    let task=tokio::spawn(duduclaw_egress::session::serve_run_cancel(host,RunId([1;16]),profile(),RunCaps{total_bytes:100,total_connections:1},Arc::new(Waiting{started:started.clone(),dropped:dropped.clone()}),receiver));
    write_frame(&mut client,EgressMessage::Connect{tunnel_id:TunnelId(1),target:target()},&default_limits()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1),started.notified()).await.unwrap();
    cancel.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1),task).await.unwrap().unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    assert!(read_frame(&mut client,&default_limits()).await.is_err());
}

#[tokio::test]
#[ignore = "root Linux VM required for root-created SCM_RIGHTS registration"]
async fn iso_root_unregister_ack_follows_data_socket_eof() {
    use std::io::IoSlice;
    use nix::sys::socket::{socketpair, AddressFamily, SockType, SockFlag, sendmsg, ControlMessage, MsgFlags};
    use duduclaw_isolation::{ipc::{self, Envelope}, ids::RequestId};
    assert_eq!(nix::unistd::geteuid().as_raw(),0);
    let (a,b)=socketpair(AddressFamily::Unix,SockType::SeqPacket,None,SockFlag::SOCK_CLOEXEC).unwrap();
    let control=CheckedSocket::new(a).unwrap();
    let server=CheckedSocket::new(b).unwrap();
    let config=duduclaw_egress::daemon::Config{profiles:List::new(vec![profile()]).unwrap(),run_caps:RunCaps{total_bytes:1024,total_connections:2},max_runs:1,host_public_addresses:Vec::new()};
    let task=tokio::spawn(async move { duduclaw_egress::daemon::serve_control(server,&config).await });
    let (data,endpoint)=sockets();
    let packet=ipc::encode(&Envelope{version:1,request_id:RequestId([1;16]),message:EgressControl::Register{run_id:RunId([1;16]),profile_id:profile().id}}).unwrap();
    sendmsg::<()>(control.raw_fd(),&[IoSlice::new(&packet)],&[ControlMessage::ScmRights(&[endpoint.raw_fd()])],MsgFlags::empty(),None).unwrap();
    drop(endpoint);
    let mut buffer=vec![0;ipc::MAX_FRAME_BYTES+4];
    let (n,fds)=control.receive(&mut buffer).await.unwrap(); assert!(fds.is_empty());
    assert!(matches!(ipc::decode::<EgressControlResponse>(&buffer[..n]).unwrap().message,EgressControlResponse::Registered));
    let packet=ipc::encode(&Envelope{version:1,request_id:RequestId([2;16]),message:EgressControl::Unregister{run_id:RunId([1;16])}}).unwrap();
    control.send_packet(&packet).await.unwrap();
    let (n,fds)=tokio::time::timeout(Duration::from_secs(1),control.receive(&mut buffer)).await.unwrap().unwrap(); assert!(fds.is_empty());
    assert!(matches!(ipc::decode::<EgressControlResponse>(&buffer[..n]).unwrap().message,EgressControlResponse::Unregistered));
    // No grace period: after the ACK the peer is already closed.
    let mut byte=[0];
    assert_eq!(nix::sys::socket::recv(data.raw_fd(), &mut byte, MsgFlags::MSG_DONTWAIT).unwrap(), 0);
    drop(control);
    assert!(tokio::time::timeout(Duration::from_secs(1),task).await.unwrap().unwrap().is_err());
}

/// R7b on a real host (Ubuntu 24.04 VM, systemd-resolved, outbound DNS): the
/// real control loop (`serve_control`, as for the unregister test above) and
/// the real resolver through the `resolved_varlink` route answer `ProbeDns`.
/// The spawnd side of the exchange (credential-checked receive of the egress
/// account) is the R7e harness in spawnd's `transport.rs`; spawnd cannot run
/// the egress code itself, so the spawnd half is covered there and by the
/// non-root `probe_dns` client tests.
#[tokio::test]
#[ignore = "root Linux VM with systemd-resolved and outbound DNS required; takes about 11 s"]
async fn iso_root_egress_dns_probe_through_control_socket() {
    use nix::sys::socket::{socketpair, AddressFamily, SockType, SockFlag};
    use duduclaw_isolation::{ipc::{self, Envelope}, ids::RequestId};
    async fn probe(control: &CheckedSocket, id: u8, name: &str) -> EgressControlResponse {
        let packet = ipc::encode(&Envelope { version: ipc::PROTOCOL_VERSION, request_id: RequestId([id; 16]),
            message: EgressControl::ProbeDns { name: name.into() } }).unwrap();
        control.send_packet(&packet).await.unwrap();
        let mut buffer = vec![0; ipc::MAX_FRAME_BYTES + 4];
        // The probe deadline is 3 s; the reply follows well inside the control timeout.
        let (n, fds) = tokio::time::timeout(Duration::from_secs(5), control.receive(&mut buffer)).await.unwrap().unwrap();
        assert!(fds.is_empty());
        let reply = ipc::decode::<EgressControlResponse>(&buffer[..n]).unwrap();
        assert_eq!(reply.request_id, RequestId([id; 16]));
        reply.message
    }
    assert_eq!(nix::unistd::geteuid().as_raw(), 0);
    let (a, b) = socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).unwrap();
    let control = CheckedSocket::new(a).unwrap();
    let server = CheckedSocket::new(b).unwrap();
    let config = duduclaw_egress::daemon::Config { profiles: List::new(vec![profile()]).unwrap(),
        run_caps: RunCaps { total_bytes: 1024, total_connections: 2 }, max_runs: 1, host_public_addresses: Vec::new() };
    let task = tokio::spawn(async move { duduclaw_egress::daemon::serve_control(server, &config).await });
    let started = std::time::Instant::now();
    let first = probe(&control, 1, "example.com").await;
    assert!(started.elapsed() < Duration::from_secs(4), "probe deadline");
    match &first {
        EgressControlResponse::DnsProbed { route: DnsRoute::ResolvedVarlink, resolved: true, public_addresses } =>
            assert!(*public_addresses >= 1),
        other => panic!("unexpected {other:?}"),
    }
    // Inside the 10 s window: the cached result, and another name is refused.
    assert_eq!(probe(&control, 2, "example.com").await, first);
    assert_eq!(probe(&control, 3, "does-not-exist.example.com").await,
        EgressControlResponse::Refused { code: EgressError::ResourceLimit });
    // An invalid name is refused; the control connection stays usable.
    assert_eq!(probe(&control, 4, "localhost").await, EgressControlResponse::Refused { code: EgressError::InvalidRequest });
    // After the window: a name that passes the syntax rule but does not resolve.
    tokio::time::sleep(Duration::from_millis(10_500).saturating_sub(started.elapsed())).await;
    let missing = probe(&control, 5, "does-not-exist.example.com").await;
    assert_eq!(missing, EgressControlResponse::DnsProbed { route: DnsRoute::ResolvedVarlink, resolved: false, public_addresses: 0 });
    assert_eq!(probe(&control, 6, "does-not-exist.example.com").await, missing);
    drop(control);
    assert!(tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap().is_err());
}

/// R8z-6: spawnd keeps a revoker duplicate of the run socket's host end. A run
/// that ends must still show the forwarder EOF at once (socket shutdown), not
/// leave it waiting for its own idle timeout.
#[tokio::test]
async fn run_end_shuts_the_socket_down_despite_a_duplicate_descriptor() {
    let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
    let revoker = b.try_clone().unwrap();
    let mut client = CheckedSocket::new(OwnedFd::from(a)).unwrap();
    let host = CheckedSocket::new(OwnedFd::from(b)).unwrap();
    let task = tokio::spawn(serve_run(host, RunId([1;16]), profile(), RunCaps {total_bytes: 100, total_connections: 1}, Arc::new(FakeResolver(Mutex::new(VecDeque::new())))));
    // Data for a tunnel that was never opened ends the run.
    write_frame(&mut client, EgressMessage::Data {tunnel_id: TunnelId(1), bytes: List::new(vec![1]).unwrap()}, &default_limits()).await.unwrap();
    let audit = tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap();
    assert_eq!((audit.terminal_code, audit.terminal_detail), (EgressError::InvalidRequest, TerminalDetail::ProtocolState));
    assert!(tokio::time::timeout(Duration::from_secs(1), read_frame(&mut client, &default_limits())).await
        .expect("EOF at once, not after the idle timeout").is_err());
    drop(revoker);
}

/// R8z-6: the hard caps still terminate, each with its fixed detail.
#[tokio::test]
async fn hard_caps_terminate_with_their_fixed_detail() {
    struct Waiting;
    impl Resolver for Waiting {
        fn resolve<'a>(&'a self, _: &'a str, _: DnsRoute) -> std::pin::Pin<Box<dyn std::future::Future<Output=Result<Vec<IpAddr>,EgressError>> + Send + 'a>> {
            Box::pin(std::future::pending())
        }
    }
    // (max_tunnels, total_connections) ⇒ the second pending Connect's detail.
    for (max_tunnels, total, detail) in [(1u16, 3u32, TerminalDetail::MaxTunnels), (4, 1, TerminalDetail::TotalConnections)] {
        let mut p = profile(); p.limits.max_tunnels = max_tunnels.try_into().unwrap();
        let (mut client, host) = sockets();
        let task = tokio::spawn(serve_run(host, RunId([1;16]), p, RunCaps {total_bytes: 100, total_connections: total}, Arc::new(Waiting)));
        for id in 1..=2 {
            write_frame(&mut client, EgressMessage::Connect {tunnel_id: TunnelId(id), target: target()}, &default_limits()).await.unwrap();
        }
        let audit = tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap();
        assert_eq!((audit.terminal_code, audit.terminal_detail), (EgressError::ResourceLimit, detail));
        let json = serde_json::to_string(&audit).unwrap();
        assert!(!json.contains("example.com"));
    }
    // A frame length above max_frame_bytes.
    let (mut client, host) = sockets();
    let task = tokio::spawn(serve_run(host, RunId([1;16]), profile(), RunCaps {total_bytes: 100, total_connections: 1}, Arc::new(Waiting)));
    client.write_u32(65537).await.unwrap();
    let audit = tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap();
    assert_eq!((audit.terminal_code, audit.terminal_detail), (EgressError::InvalidRequest, TerminalDetail::FrameTooLarge));
}

/// Requires root on Linux and an operator-controlled PUBLIC TCP echo server.
/// R8z-6: 256 KiB written back-to-back through one tunnel under the default
/// rates (1024 frames/s, 4 MiB/s) completes; the run ends only when the
/// client closes it.
#[tokio::test]
#[ignore = "root Linux VM and ISO_EGRESS_ECHO_ADDR public echo endpoint required"]
async fn iso_root_live_256_kib_upload_under_default_rates() {
    assert_eq!(nix::unistd::geteuid().as_raw(), 0);
    let address: std::net::SocketAddr = std::env::var("ISO_EGRESS_ECHO_ADDR").expect("public controlled echo address").parse().unwrap();
    assert!(duduclaw_egress::policy::is_public_ip(address.ip()));
    let mut p = profile(); p.allowed_connect_ports = List::new(vec![address.port().try_into().unwrap()]).unwrap();
    let limits = p.limits.clone();
    let (client, host) = sockets();
    let task = tokio::spawn(serve_run(host, RunId([8;16]), p, RunCaps {total_bytes: 1 << 32, total_connections: 1}, Arc::new(FakeResolver(Mutex::new(VecDeque::new())))));
    let (mut reader, mut writer) = tokio::io::split(client);
    let target = ConnectTarget {host:Text::new(address.ip().to_string()).unwrap(),port:address.port().try_into().unwrap()};
    write_frame(&mut writer,EgressMessage::Connect{tunnel_id:TunnelId(1),target},&limits).await.unwrap();
    assert!(matches!(read_frame(&mut reader,&limits).await.unwrap(),EgressMessage::Connected{..}));
    let upload_limits = limits.clone();
    let upload = tokio::spawn(async move {
        for index in 0..32u8 {
            write_frame(&mut writer,EgressMessage::Data{tunnel_id:TunnelId(1),bytes:List::new(vec![index;8192]).unwrap()},&upload_limits).await.unwrap();
        }
        writer
    });
    let echoed = tokio::time::timeout(Duration::from_secs(30), async {
        let mut echoed = Vec::new();
        while echoed.len() < 256 * 1024 {
            match read_frame(&mut reader,&limits).await.unwrap() {
                EgressMessage::Data{tunnel_id:TunnelId(1),bytes}=>echoed.extend_from_slice(bytes.as_slice()),
                other=>panic!("unexpected {other:?}"),
            }
        }
        echoed
    }).await.unwrap();
    for (index, chunk) in echoed.chunks(8192).enumerate() { assert!(chunk.iter().all(|b| *b == index as u8)); }
    let writer = upload.await.unwrap();
    assert!(!task.is_finished());
    drop(reader); drop(writer);
    let audit = task.await.unwrap();
    assert_eq!(audit.bytes, 2 * 256 * 1024);
    assert_eq!((audit.terminal_code, audit.terminal_detail), (EgressError::Unavailable, TerminalDetail::PeerClosed));
}

#[test]
fn config_bounds_aggregate_tunnel_reservations() {
    let mut config = duduclaw_egress::daemon::Config {
        profiles: List::new(vec![profile()]).unwrap(),
        run_caps: RunCaps { total_bytes: 1024, total_connections: 2 }, max_runs: 64,
        host_public_addresses: Vec::new(),
    };
    assert!(config.validate().is_ok()); // 64 * 16 = 1024
    // SR5 round 3 N7b: the configured NAT address list is bounded.
    config.host_public_addresses = vec!["203.0.114.50".parse().unwrap(); 65];
    assert_eq!(config.validate(), Err(EgressError::ResourceLimit));
    config.host_public_addresses.truncate(64);
    assert!(config.validate().is_ok());
    config.host_public_addresses.clear();
    config.max_runs = 65;
    assert_eq!(config.validate(), Err(EgressError::ResourceLimit));
    let mut large = profile();
    large.id = ProfileId(Sha256Digest([2; 32]));
    large.limits.max_tunnels = 256.try_into().unwrap();
    config.max_runs = 5;
    config.profiles = List::new(vec![profile(), large]).unwrap();
    assert_eq!(config.validate(), Err(EgressError::ResourceLimit));
    config.max_runs = 4;
    assert!(config.validate().is_ok());
}

#[tokio::test]
async fn global_tunnel_limit_precedes_dns_across_runs_and_cancel_returns_permit() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Waiting { started: Arc<tokio::sync::Notify>, calls: Arc<AtomicUsize> }
    impl Resolver for Waiting {
        fn resolve<'a>(&'a self, _: &'a str, _: DnsRoute) -> std::pin::Pin<Box<dyn std::future::Future<Output=Result<Vec<IpAddr>,EgressError>> + Send + 'a>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.started.notify_one();
                std::future::pending().await
            })
        }
    }
    let global = Arc::new(tokio::sync::Semaphore::new(1));
    let started = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let resolver = Arc::new(Waiting { started: started.clone(), calls: calls.clone() });
    let mut clients = Vec::new();
    let mut runs = Vec::new();
    for id in 1..=8 {
        let (client, host) = sockets();
        let (cancel, receiver) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(duduclaw_egress::session::serve_run_limited(host, RunId([id;16]),
            profile(), RunCaps { total_bytes: 1024, total_connections: 8 }, resolver.clone(), receiver, global.clone()));
        clients.push(client);
        runs.push((cancel, task));
    }
    write_frame(&mut clients[0], EgressMessage::Connect { tunnel_id: TunnelId(1), target: target() }, &default_limits()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), started.notified()).await.unwrap();
    assert_eq!(global.available_permits(), 0);
    for client in clients.iter_mut().skip(1) {
        write_frame(client, EgressMessage::Connect { tunnel_id: TunnelId(1), target: target() }, &default_limits()).await.unwrap();
        assert!(matches!(tokio::time::timeout(Duration::from_secs(1), read_frame(client, &default_limits())).await.unwrap().unwrap(),
            EgressMessage::Refused { code: EgressError::ResourceLimit, .. }));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "refused runs must not start DNS");
    let (cancel, task) = runs.remove(0);
    cancel.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap();
    assert_eq!(global.available_permits(), 1, "cancellation ACK must follow permit release");
    write_frame(&mut clients[1], EgressMessage::Connect { tunnel_id: TunnelId(2), target: target() }, &default_limits()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), started.notified()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(global.available_permits(), 0);
    // Client Close aborts a pending DNS future and also returns its permit.
    write_frame(&mut clients[1], EgressMessage::Close { tunnel_id: TunnelId(2) }, &default_limits()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while global.available_permits() != 1 { tokio::task::yield_now().await; }
    }).await.unwrap();
    for (cancel, task) in runs {
        cancel.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap();
    }
    assert_eq!(global.available_permits(), 1);
}
