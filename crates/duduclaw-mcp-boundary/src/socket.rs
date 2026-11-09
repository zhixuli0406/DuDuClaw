use std::os::unix::net::UnixStream;
use duduclaw_isolation::capability::VerificationError;
use duduclaw_isolation::{execution::RunHandle, ipc::{ControlConnectionId, SpawnResponse}};

/// Typed handoff from package A, after peer credentials, envelope correlation,
/// truncation/cmsg and exact fd-plan validation on the spawnd control connection.
/// Metadata is taken from the Io reply, never reconstructed from the request.
pub struct McpDelivery {
    pub(crate) run: RunHandle,
    pub(crate) control: ControlConnectionId,
    pub(crate) socket: UnixStream,
}
impl McpDelivery {
    pub fn from_take_io(response: &SpawnResponse, control: ControlConnectionId, socket: UnixStream)
        -> Result<Self, VerificationError> {
        let SpawnResponse::Io { run, delivery } = response else { return Err(VerificationError::Invalid); };
        if !delivery.mcp { return Err(VerificationError::Invalid); }
        validate(&socket)?;
        Ok(Self { run: *run, control, socket })
    }
}

/// No pathname reconnect, TCP socket, listener or unconnected fd is accepted.
pub fn validate(socket: &UnixStream) -> Result<(), VerificationError> {
    use rustix::net::{sockopt::{socket_domain, socket_type}, AddressFamily, SocketType};
    if socket_domain(socket).map_err(|_| VerificationError::Invalid)? != AddressFamily::UNIX
        || socket_type(socket).map_err(|_| VerificationError::Invalid)? != SocketType::STREAM
        || !socket.local_addr().map_err(|_| VerificationError::Invalid)?.is_unnamed()
        || !socket.peer_addr().map_err(|_| VerificationError::Invalid)?.is_unnamed() {
        return Err(VerificationError::Invalid);
    }
    Ok(())
}
