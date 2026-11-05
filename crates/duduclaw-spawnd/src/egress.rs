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
