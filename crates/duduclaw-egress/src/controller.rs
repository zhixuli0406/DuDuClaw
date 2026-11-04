//! B1 integration seam. A controller connection owns the lifetime of its runs.
use std::{future::Future, os::fd::{AsRawFd, OwnedFd}, path::Path, pin::Pin, time::Duration};
use duduclaw_isolation::{egress::{EgressControl, EgressControlResponse, EgressError, ProfileId}, ids::{RequestId, RunId, EGRESS_CONTROL_SOCKET}, ipc::{self, Envelope}};
use nix::sys::socket::{AddressFamily, SockFlag, SockType};
use crate::transport::{CheckedSocket, socket_info};

pub type ControlFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, EgressError>> + Send + 'a>>;

/// Owned by spawnd, never the gateway or an employee. Keep one instance for the
/// daemon lifetime. Only release the returned fd to B2 after register succeeds.
/// Dropping a pending operation revokes the entire controller, including other
/// runs. B1 must then stop those runs; reconnecting never restores capabilities.
pub trait EgressController: Send {
    fn register(&mut self, run: RunId, profile: ProfileId) -> ControlFuture<'_, OwnedFd>;
    fn unregister(&mut self, run: RunId) -> ControlFuture<'_, ()>;
}

/// Safe default until B1 explicitly wires the Linux controller.
#[derive(Default)]
pub struct UnavailableEgress;
impl EgressController for UnavailableEgress {
    fn register(&mut self, _: RunId, _: ProfileId) -> ControlFuture<'_, OwnedFd> {
        Box::pin(async { Err(EgressError::Unavailable) })
    }
    fn unregister(&mut self, _: RunId) -> ControlFuture<'_, ()> {
        Box::pin(async { Err(EgressError::Unavailable) })
    }
}

pub struct RootEgressController { socket: Option<CheckedSocket>, sequence: u128 }
impl RootEgressController {
    /// Fixed protected path only. A missing service, backlog or bad identity
    /// fails closed; the caller must never select a substitute socket path.
    pub fn connect() -> Result<Self, EgressError> {
        use std::os::unix::fs::{MetadataExt, FileTypeExt};
        if !nix::unistd::getuid().is_root() || !nix::unistd::geteuid().is_root()
            || nix::unistd::getgid().as_raw() != 0 || nix::unistd::getegid().as_raw() != 0 {
            return Err(EgressError::Unavailable);
        }
        let path = Path::new(EGRESS_CONTROL_SOCKET);
        for ancestor in path.ancestors() {
            let meta = std::fs::symlink_metadata(ancestor).map_err(|_| EgressError::Unavailable)?;
            if meta.uid() != 0 || meta.gid() != 0 || meta.file_type().is_symlink()
                || if ancestor == path { !meta.file_type().is_socket() || meta.mode() & 0o777 != 0o600 }
                   else { !meta.is_dir() || meta.mode() & 0o022 != 0 } {
                return Err(EgressError::Unavailable);
            }
        }
        let fd = nix::sys::socket::socket(AddressFamily::Unix, SockType::SeqPacket,
            SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK, None).map_err(|_| EgressError::Unavailable)?;
        let address = nix::sys::socket::UnixAddr::new(path).map_err(|_| EgressError::Unavailable)?;
        nix::sys::socket::connect(fd.as_raw_fd(), &address).map_err(|_| EgressError::Unavailable)?;
        // systemd creates/listens on the socket as root before handing it off.
        socket_info(&fd, libc::SOCK_SEQPACKET, true, false).map_err(|_| EgressError::Unavailable)?;
        Ok(Self { socket: Some(CheckedSocket::new(fd).map_err(|_| EgressError::Unavailable)?), sequence: 0 })
    }

    async fn exchange(&mut self, message: EgressControl, endpoint: Option<&OwnedFd>) -> Result<(), EgressError> {
        // Taking ownership before the first await makes cancellation fail closed.
        let socket = self.socket.take().ok_or(EgressError::Unavailable)?;
        self.sequence = self.sequence.checked_add(1).ok_or(EgressError::ResourceLimit)?;
        let request_id = RequestId(self.sequence.to_be_bytes());
        let registering = matches!(message, EgressControl::Register { .. });
        let bytes = ipc::encode(&Envelope { version: ipc::PROTOCOL_VERSION, request_id, message })
            .map_err(|_| EgressError::InvalidRequest)?;
        let response = tokio::time::timeout(Duration::from_millis(ipc::CONTROL_TIMEOUT_MS), async {
            socket.send_packet_with_fd(&bytes, endpoint).await.map_err(|_| EgressError::Unavailable)?;
            let mut buffer = vec![0; ipc::MAX_FRAME_BYTES + 4];
            let (size, fds) = socket.receive(&mut buffer).await.map_err(|_| EgressError::Unavailable)?;
            if !fds.is_empty() { return Err(EgressError::InvalidRequest); }
            let reply = ipc::decode::<EgressControlResponse>(&buffer[..size]).map_err(|_| EgressError::InvalidRequest)?;
            if reply.request_id != request_id { return Err(EgressError::InvalidRequest); }
            match reply.message {
                EgressControlResponse::Registered if registering => Ok(Ok(())),
                EgressControlResponse::Unregistered if !registering => Ok(Ok(())),
                EgressControlResponse::Refused { code } => Ok(Err(code)),
                _ => Err(EgressError::InvalidRequest),
            }
        }).await.map_err(|_| EgressError::Timeout)??;
        // A correlated refusal is a completed transaction, not a broken stream.
        self.socket = Some(socket);
        response
    }
}
impl EgressController for RootEgressController {
    fn register(&mut self, run_id: RunId, profile_id: ProfileId) -> ControlFuture<'_, OwnedFd> {
        Box::pin(async move {
            if !nix::unistd::geteuid().is_root() || nix::unistd::getegid().as_raw() != 0 { return Err(EgressError::Unavailable); }
            let (forwarder, host) = nix::sys::socket::socketpair(AddressFamily::Unix, SockType::Stream, None,
                SockFlag::SOCK_CLOEXEC).map_err(|_| EgressError::Unavailable)?;
            self.exchange(EgressControl::Register { run_id, profile_id }, Some(&host)).await?;
            Ok(forwarder)
        })
    }
    fn unregister(&mut self, run_id: RunId) -> ControlFuture<'_, ()> {
        Box::pin(self.exchange(EgressControl::Unregister { run_id }, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair() -> (RootEgressController, CheckedSocket) {
        let (a,b) = nix::sys::socket::socketpair(AddressFamily::Unix,SockType::SeqPacket,None,SockFlag::SOCK_CLOEXEC).unwrap();
        (RootEgressController {socket:Some(CheckedSocket::new(a).unwrap()),sequence:0},CheckedSocket::new(b).unwrap())
    }
    #[tokio::test]
    async fn controller_cancellation_poison_closes_socket() {
        let (mut client, server) = pair();
        let mut buffer = vec![0; ipc::MAX_FRAME_BYTES+4];
        {
            let operation = client.unregister(RunId([1;16]));
            tokio::pin!(operation);
            tokio::select! {
                _ = &mut operation => panic!("must wait for reply"),
                result = server.receive(&mut buffer) => { assert!(result.unwrap().0 > 0); }
            }
        }
        assert_eq!(server.receive(&mut buffer).await.unwrap().0,0);
        assert_eq!(client.unregister(RunId([2;16])).await,Err(EgressError::Unavailable));
    }
    #[tokio::test]
    async fn controller_wrong_ack_id_direction_and_fds_poison_connection() {
        for case in 0..4 {
            let (mut client, server) = pair();
            let peer = tokio::spawn(async move {
                let mut buffer = vec![0; ipc::MAX_FRAME_BYTES+4];
                let (size,_) = server.receive(&mut buffer).await.unwrap();
                let request = ipc::decode::<EgressControl>(&buffer[..size]).unwrap();
                let response = Envelope {version:if case==3 {2}else{1}, request_id:if case==0 {RequestId([9;16])}else{request.request_id},
                    message:if case==1 {EgressControlResponse::Registered}else{EgressControlResponse::Unregistered}};
                // Deliberately bypass the version validator to exercise decode.
                let payload=serde_json::to_vec(&response).unwrap();
                let mut bytes=(payload.len() as u32).to_be_bytes().to_vec(); bytes.extend(payload);
                let fd:OwnedFd=std::fs::File::open("/dev/null").unwrap().into();
                server.send_packet_with_fd(&bytes,if case==2 {Some(&fd)}else{None}).await.unwrap();
                assert_eq!(server.receive(&mut buffer).await.unwrap().0,0);
            });
            assert_eq!(client.unregister(RunId([1;16])).await,Err(EgressError::InvalidRequest));
            assert_eq!(client.unregister(RunId([2;16])).await,Err(EgressError::Unavailable));
            peer.await.unwrap();
        }
    }
    #[tokio::test]
    async fn controller_correlated_refusal_keeps_connection_and_default_denies() {
        let (mut client,server)=pair();
        let peer=tokio::spawn(async move {
            let mut buffer=vec![0;ipc::MAX_FRAME_BYTES+4];
            for message in [EgressControlResponse::Refused{code:EgressError::ResourceLimit},EgressControlResponse::Unregistered] {
                let (n,_)=server.receive(&mut buffer).await.unwrap();
                let request=ipc::decode::<EgressControl>(&buffer[..n]).unwrap();
                server.send_packet(&ipc::encode(&Envelope{version:1,request_id:request.request_id,message}).unwrap()).await.unwrap();
            }
        });
        assert_eq!(client.unregister(RunId([1;16])).await,Err(EgressError::ResourceLimit));
        client.unregister(RunId([1;16])).await.unwrap(); peer.await.unwrap();
        let mut unavailable:Box<dyn EgressController>=Box::new(UnavailableEgress);
        assert_eq!(unavailable.unregister(RunId([1;16])).await,Err(EgressError::Unavailable));
        assert!(unavailable.register(RunId([1;16]),ProfileId(duduclaw_isolation::ids::Sha256Digest([1;32]))).await.is_err());
    }
    #[tokio::test]
    async fn controller_timeout_poison_does_not_reuse_late_reply() {
        let (mut client,server)=pair();
        assert_eq!(client.unregister(RunId([1;16])).await,Err(EgressError::Timeout));
        assert_eq!(client.unregister(RunId([1;16])).await,Err(EgressError::Unavailable));
        let mut buffer=vec![0;ipc::MAX_FRAME_BYTES+4];
        assert!(server.receive(&mut buffer).await.unwrap().0>0);
        assert_eq!(server.receive(&mut buffer).await.unwrap().0,0);
    }

    #[tokio::test]
    #[ignore = "root Linux VM for root-created socketpair and controller credentials"]
    async fn iso_root_controller_register_revoke_and_disconnect() {
        use crate::{budget::{default_limits,RunCaps},daemon::{Config,serve_control}};
        use duduclaw_isolation::{bounded::List,egress::{EgressProfile,DnsRoute},ids::Sha256Digest};
        assert_eq!(nix::unistd::geteuid().as_raw(),0);
        let (mut client,server)=pair();
        let profile=EgressProfile{id:ProfileId(Sha256Digest([1;32])),allowed_connect_ports:List::new(vec![443.try_into().unwrap()]).unwrap(),dns_route:DnsRoute::ResolvedVarlink,limits:default_limits()};
        let id=profile.id;
        let config=Config{profiles:List::new(vec![profile]).unwrap(),run_caps:RunCaps{total_bytes:1024,total_connections:2},max_runs:2};
        let task=tokio::spawn(async move {serve_control(server,&config).await});
        let a=CheckedSocket::new(client.register(RunId([1;16]),id).await.unwrap()).unwrap();
        let b=CheckedSocket::new(client.register(RunId([2;16]),id).await.unwrap()).unwrap();
        assert!(matches!(client.register(RunId([1;16]),id).await,Err(EgressError::ResourceLimit)));
        client.unregister(RunId([1;16])).await.unwrap();
        let mut byte=[0];
        assert_eq!(nix::sys::socket::recv(a.raw_fd(),&mut byte,nix::sys::socket::MsgFlags::MSG_DONTWAIT).unwrap(),0);
        assert!(matches!(client.register(RunId([1;16]),id).await,Err(EgressError::ResourceLimit)));
        drop(client);
        assert_eq!(tokio::time::timeout(Duration::from_secs(2),b.receive(&mut byte)).await.unwrap().unwrap().0,0);
        assert!(tokio::time::timeout(Duration::from_secs(2),task).await.unwrap().unwrap().is_err());
    }

}
