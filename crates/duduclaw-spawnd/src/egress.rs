//! Root-to-egress control. Acknowledgement precedes release of the forwarder fd.
use std::{io, os::fd::{BorrowedFd, AsFd, OwnedFd}};
use duduclaw_isolation::{egress::{EgressControl, EgressControlResponse, ProfileId},
    ids::{RunId, RequestId}, ipc::{self, Envelope}};
use crate::transport::Seqpacket;

fn unavailable() -> io::Error { io::Error::other("egress unavailable") }
pub struct EgressControlClient { socket: Seqpacket }
impl EgressControlClient {
    pub fn connect(uid: u32, gid: u32) -> io::Result<Self> {
        Ok(Self { socket: Seqpacket::connect_fixed_egress(uid, gid)? })
    }
    pub fn register(&mut self, request: RequestId, run: RunId, profile: ProfileId, fd: BorrowedFd<'_>) -> io::Result<()> {
        self.exchange(request, EgressControl::Register { run_id: run, profile_id: profile }, &[fd], true)
    }
    pub fn unregister(&mut self, request: RequestId, run: RunId) -> io::Result<()> {
        self.exchange(request, EgressControl::Unregister { run_id: run }, &[], false)
    }
    fn exchange(&mut self, request_id: RequestId, message: EgressControl, descriptors: &[BorrowedFd<'_>], register: bool) -> io::Result<()> {
        let bytes = ipc::encode(&Envelope { version: ipc::PROTOCOL_VERSION, request_id, message }).map_err(|_| unavailable())?;
        self.socket.send(&bytes, descriptors)?;
        let packet = self.socket.recv()?;
        if !packet.fds.is_empty() { return Err(unavailable()); }
        let response = ipc::decode::<EgressControlResponse>(&packet.bytes).map_err(|_| unavailable())?;
        if response.request_id != request_id { return Err(unavailable()); }
        match (register, response.message) {
            (true, EgressControlResponse::Registered) | (false, EgressControlResponse::Unregistered) => Ok(()),
            _ => Err(unavailable()),
        }
    }
    pub fn register_owned(&mut self, request: RequestId, run: RunId, profile: ProfileId, fd: OwnedFd) -> io::Result<()> {
        self.register(request, run, profile, fd.as_fd())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::ids::Sha256Digest;

    #[test]
    fn run_profile_registration_and_revocation_use_exact_control_binding() {
        let (client, server) = Seqpacket::pair().unwrap();
        let run = RunId([1; 16]);
        let profile = ProfileId(Sha256Digest([2; 32]));
        let peer = std::thread::spawn(move || {
            let packet = server.recv().unwrap();
            assert_eq!(packet.fds.len(), 1);
            let request = ipc::decode::<EgressControl>(&packet.bytes).unwrap();
            assert_eq!(request.message, EgressControl::Register { run_id: run, profile_id: profile });
            // The root endpoint receives the registered socket, not a path or
            // authority supplied in a data frame by the employee forwarder.
            let received = packet.fds.into_iter().next().unwrap();
            assert_eq!(nix::sys::socket::getsockopt(&received, nix::sys::socket::sockopt::SockType).unwrap(),
                nix::sys::socket::SockType::Stream);
            server.send(&ipc::encode(&Envelope { version: ipc::PROTOCOL_VERSION,
                request_id: request.request_id, message: EgressControlResponse::Registered }).unwrap(), &[]).unwrap();
            let packet = server.recv().unwrap();
            assert!(packet.fds.is_empty());
            let request = ipc::decode::<EgressControl>(&packet.bytes).unwrap();
            assert_eq!(request.message, EgressControl::Unregister { run_id: run });
            drop(received);
            server.send(&ipc::encode(&Envelope { version: ipc::PROTOCOL_VERSION,
                request_id: request.request_id, message: EgressControlResponse::Unregistered }).unwrap(), &[]).unwrap();
        });
        let pair = crate::channels::RevocablePair::new().unwrap();
        let mut client = EgressControlClient { socket: client };
        client.register_owned(RequestId([3; 16]), run, profile, pair.gateway).unwrap();
        client.unregister(RequestId([4; 16]), run).unwrap();
        peer.join().unwrap();
    }

    #[test]
    fn wrong_or_descriptor_bearing_acknowledgement_refuses_egress_lifecycle() {
        for case in 0..4 {
            let (client, server) = Seqpacket::pair().unwrap();
            let peer = std::thread::spawn(move || {
                let packet = server.recv().unwrap();
                let request = ipc::decode::<EgressControl>(&packet.bytes).unwrap();
                let response = Envelope { version: ipc::PROTOCOL_VERSION,
                    request_id: if case == 0 { RequestId([9; 16]) } else { request.request_id },
                    message: if case == 1 { EgressControlResponse::Registered }
                        else { EgressControlResponse::Unregistered } };
                if case == 3 { return; }
                let extra = crate::channels::RevocablePair::new().unwrap();
                let descriptors = if case == 2 { vec![extra.gateway.as_fd()] } else { vec![] };
                server.send(&ipc::encode(&response).unwrap(), &descriptors).unwrap();
            });
            let mut client = EgressControlClient { socket: client };
            assert!(client.unregister(RequestId([4; 16]), RunId([1; 16])).is_err());
            peer.join().unwrap();
        }
    }
}
