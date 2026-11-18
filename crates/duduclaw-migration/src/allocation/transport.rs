//! Fixed root-owned seqpacket endpoint; bounded frames, no passed descriptors.
use std::{os::fd::{AsRawFd, FromRawFd, OwnedFd}, os::unix::fs::MetadataExt, path::Path};
use serde::{Serialize, de::DeserializeOwned};
use super::*;
fn invalid<T>() -> Result<T, Error> { Err(Error::Authentication) }
fn directory() -> Result<(), Error> {
    let path=Path::new("/run/duduclaw-spawnd");
    for part in path.ancestors() {
        let m=std::fs::symlink_metadata(part)?;
        if !m.is_dir() || m.uid()!=0 || m.mode()&0o022!=0 { return invalid(); }
    }
    Ok(())
}
fn socket() -> Result<OwnedFd, Error> {
    let raw=unsafe {libc::socket(libc::AF_UNIX,libc::SOCK_SEQPACKET|libc::SOCK_CLOEXEC,0)};
    if raw<0 {return Err(Error::Unavailable)}
    let fd=unsafe{OwnedFd::from_raw_fd(raw)};
    timeout(&fd)?; Ok(fd)
}
fn timeout(fd:&OwnedFd)->Result<(),Error>{
    let tv=libc::timeval{tv_sec:5,tv_usec:0};
    for opt in [libc::SO_RCVTIMEO,libc::SO_SNDTIMEO] {
        if unsafe{libc::setsockopt(fd.as_raw_fd(),libc::SOL_SOCKET,opt,(&tv as *const libc::timeval).cast(),std::mem::size_of_val(&tv) as _)}!=0 {return Err(Error::Unavailable)}
    } Ok(())
}
fn address()->libc::sockaddr_un{
    let mut addr:libc::sockaddr_un=unsafe{std::mem::zeroed()};addr.sun_family=libc::AF_UNIX as _;
    for (dst,src) in addr.sun_path.iter_mut().zip(SOCKET.bytes()){*dst=src as _;}addr
}
fn peer(fd:&OwnedFd)->Result<(u32,u32),Error>{
    let mut cred:libc::ucred=unsafe{std::mem::zeroed()};let mut len=std::mem::size_of_val(&cred) as libc::socklen_t;
    if unsafe{libc::getsockopt(fd.as_raw_fd(),libc::SOL_SOCKET,libc::SO_PEERCRED,(&mut cred as *mut libc::ucred).cast(),&mut len)}!=0 || len as usize!=std::mem::size_of_val(&cred){return invalid()}
    Ok((cred.uid,cred.gid))
}
pub fn send<T:Serialize>(fd:&OwnedFd,value:&T)->Result<(),Error>{
    let bytes=serde_json::to_vec(value)?;if bytes.len()>MAX_BYTES{return Err(Error::Limit)}
    let n=unsafe{libc::send(fd.as_raw_fd(),bytes.as_ptr().cast(),bytes.len(),libc::MSG_NOSIGNAL)};
    if n<0 || n as usize!=bytes.len(){return Err(Error::Unavailable)}Ok(())
}
pub fn receive<T:DeserializeOwned>(fd:&OwnedFd)->Result<T,Error>{
    let mut bytes=vec![0u8;MAX_BYTES];let mut iov=libc::iovec{iov_base:bytes.as_mut_ptr().cast(),iov_len:bytes.len()};
    let mut msg:libc::msghdr=unsafe{std::mem::zeroed()};msg.msg_iov=&mut iov;msg.msg_iovlen=1;
    // No ancillary buffer: the kernel closes rejected rights and sets MSG_CTRUNC.
    let n=unsafe{libc::recvmsg(fd.as_raw_fd(),&mut msg,libc::MSG_CMSG_CLOEXEC)};
    if n<=0 || msg.msg_flags&(libc::MSG_TRUNC|libc::MSG_CTRUNC)!=0{return Err(Error::Invalid)}
    Ok(serde_json::from_slice(&bytes[..n as usize])?)
}
fn connect()->Result<OwnedFd,Error>{
    directory()?;let fd=socket()?;let addr=address();
    if unsafe{libc::connect(fd.as_raw_fd(),(&addr as *const libc::sockaddr_un).cast(),std::mem::size_of_val(&addr) as _)}!=0{return Err(Error::Unavailable)}
    if peer(&fd)?!=(0,0){return invalid()}
    Ok(fd)
}
/// Schema-1 exchange (v1.80 semantics): a receipt or `Unavailable`.
pub fn request(request:&Request)->Result<Receipt,Error>{
    if request.schema!=1{return Err(Error::Invalid)}
    request.validate()?;let fd=connect()?;
    send(&fd,request)?;let response:Response=receive(&fd)?;
    if response.schema!=1 || response.nonce!=request.nonce{return invalid()}
    let receipt=response.receipt.ok_or(Error::Unavailable)?;
    let spec=request.validate()?;
    if receipt.binding!=request.item.binding || receipt.label!=spec.label || receipt.employee.0==[0;16]
        || receipt.bytes!=spec.bytes || receipt.inodes!=spec.inodes || receipt.principal_binding!=spec.binding
        || (receipt.uid<1000 && !(receipt.disabled && receipt.uid==0))
        || receipt.disabled!=(request.action==Action::Disable){return invalid()}
    Ok(receipt)
}
/// Schema-2 exchange (F6l), the schema-3 `Allocate` (R7d) or the schema-4
/// `Entitle` (R8d): the checked answer, a refusal code included. Any
/// transport failure or missing/garbled reply (an unknown refusal kind
/// included) is an `Err` (the caller reports `ROOT_UNAVAILABLE`, never
/// `NotFound`, S5d).
pub fn exchange(request:&Request)->Result<Answer,Error>{
    check_response(request,round_trip(request)?)
}
/// The unchecked reply to a schema-2/3/4 request; callers must pass it
/// through [`check_response`] ([`exchange`] does).
pub fn round_trip(request:&Request)->Result<Response,Error>{
    if !matches!(request.schema,SCHEMA_V2|SCHEMA_V3|SCHEMA_V4){return Err(Error::Invalid)}
    request.check()?;let fd=connect()?;
    send(&fd,request)?;receive(&fd)
}
/// Caller is the root daemon holding the registry lock. This function never
/// accepts a socket path from a request or an environment variable.
pub fn bind(gateway_gid:u32)->Result<OwnedFd,Error>{
    directory()?;if unsafe{libc::geteuid()}!=0 || gateway_gid==0{return invalid()}
    match std::fs::symlink_metadata(SOCKET){
        Ok(m)=>{use std::os::unix::fs::FileTypeExt;if !m.file_type().is_socket() || m.uid()!=0{return invalid()}std::fs::remove_file(SOCKET)?;},
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},Err(e)=>return Err(e.into()),
    }
    let fd=socket()?;let addr=address();
    if unsafe{libc::bind(fd.as_raw_fd(),(&addr as *const libc::sockaddr_un).cast(),std::mem::size_of_val(&addr) as _)}!=0{return Err(Error::Unavailable)}
    let path=std::ffi::CString::new(SOCKET).map_err(|_|Error::Invalid)?;
    if unsafe{libc::chown(path.as_ptr(),0,gateway_gid)}!=0 || unsafe{libc::chmod(path.as_ptr(),0o660)}!=0 || unsafe{libc::listen(fd.as_raw_fd(),8)}!=0{return Err(Error::Unavailable)}
    Ok(fd)
}
pub fn accept(listener:&OwnedFd,gateway:(u32,u32))->Result<OwnedFd,Error>{
    let raw=unsafe{libc::accept4(listener.as_raw_fd(),std::ptr::null_mut(),std::ptr::null_mut(),libc::SOCK_CLOEXEC)};
    if raw<0{return Err(Error::Unavailable)}let fd=unsafe{OwnedFd::from_raw_fd(raw)};timeout(&fd)?;
    if gateway.0==0 || gateway.1==0 || peer(&fd)?!=gateway{return invalid()}Ok(fd)
}

// ---- R8r-2: schema 5 (`provision_providers`) ----

/// R8r-2: [`accept`] that also admits host root (uid 0 AND gid 0) and says
/// which peer connected ([`providers::classify`]); the caller applies
/// [`providers::admits`] (root may send `provision_providers` only).
pub fn accept_peer(listener:&OwnedFd,gateway:(u32,u32))->Result<(OwnedFd,providers::Peer),Error>{
    let raw=unsafe{libc::accept4(listener.as_raw_fd(),std::ptr::null_mut(),std::ptr::null_mut(),libc::SOCK_CLOEXEC)};
    if raw<0{return Err(Error::Unavailable)}let fd=unsafe{OwnedFd::from_raw_fd(raw)};timeout(&fd)?;
    let who=providers::classify(peer(&fd)?,gateway).ok_or(Error::Authentication)?;
    Ok((fd,who))
}
/// A frame buffer that never grows past the capacity reserved up front, so
/// serialising a secret never reallocates (no unzeroized copy is left).
struct Bounded<'a>(&'a mut Vec<u8>);
impl std::io::Write for Bounded<'_>{
    fn write(&mut self,buf:&[u8])->std::io::Result<usize>{
        let end=self.0.len().checked_add(buf.len()).ok_or_else(||std::io::Error::other("frame limit"))?;
        if end>MAX_BYTES || end>self.0.capacity(){return Err(std::io::Error::other("frame limit"))}
        self.0.extend_from_slice(buf);Ok(buf.len())
    }
    fn flush(&mut self)->std::io::Result<()>{Ok(())}
}
/// [`send`] for a frame carrying a secret: serialised into a zeroizing
/// buffer that is wiped when this returns.
pub fn send_secret<T:Serialize>(fd:&OwnedFd,value:&T)->Result<(),Error>{
    let mut bytes=zeroize::Zeroizing::new(Vec::with_capacity(MAX_BYTES));
    serde_json::to_writer(Bounded(&mut bytes),value).map_err(|_|Error::Limit)?;
    let n=unsafe{libc::send(fd.as_raw_fd(),bytes.as_ptr().cast(),bytes.len(),libc::MSG_NOSIGNAL)};
    if n<0 || n as usize!=bytes.len(){return Err(Error::Unavailable)}Ok(())
}
/// Server side: one frame (schema 1–4 [`Request`] or schema-5
/// [`providers::ProviderRequest`]) from a receive buffer that is wiped when
/// this returns (a payload is moved into its own zeroizing buffer).
pub fn receive_frame(fd:&OwnedFd)->Result<providers::Frame,Error>{
    let mut bytes=zeroize::Zeroizing::new(vec![0u8;MAX_BYTES]);
    let mut iov=libc::iovec{iov_base:bytes.as_mut_ptr().cast(),iov_len:bytes.len()};
    let mut msg:libc::msghdr=unsafe{std::mem::zeroed()};msg.msg_iov=&mut iov;msg.msg_iovlen=1;
    let n=unsafe{libc::recvmsg(fd.as_raw_fd(),&mut msg,libc::MSG_CMSG_CLOEXEC)};
    if n<=0 || msg.msg_flags&(libc::MSG_TRUNC|libc::MSG_CTRUNC)!=0{return Err(Error::Invalid)}
    providers::decode_frame(&bytes[..n as usize])
}
/// R8r-2 review round 1 (F3): where a schema-5 exchange failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvideFailure {
    /// Before the request was fully sent (request check, connect, peer
    /// check, serialisation, send): root never received it; nothing changed.
    NotSent(Error),
    /// After the request was sent (no reply, receive timeout, a garbled or
    /// mismatched reply): root may have completed the change.
    Unconfirmed(Error),
}
/// The schema-5 exchange. Takes the request by value: it is serialised into
/// a zeroizing buffer and dropped (payload wiped) before the reply is read.
/// `Ok(Err(code))` is root's refusal; `Err` is a transport failure or a
/// reply that does not match the request (never a refusal), classified by
/// whether the request had been sent ([`ProvideFailure`]).
pub fn provide(request:providers::ProviderRequest)->Result<Result<providers::Provisioned,providers::ProviderErrorCode>,ProvideFailure>{
    request.check().map_err(|_|ProvideFailure::NotSent(Error::Invalid))?;
    let fd=connect().map_err(ProvideFailure::NotSent)?;
    provide_on(&fd,request)
}
/// [`provide`] over an already connected, authenticated socket.
fn provide_on(fd:&OwnedFd,request:providers::ProviderRequest)->Result<Result<providers::Provisioned,providers::ProviderErrorCode>,ProvideFailure>{
    let (nonce,clear)=(request.nonce,request.clear);
    // A seqpacket send is all or nothing: a failed send queued no frame.
    let sent=send_secret(fd,&request);
    drop(request);
    sent.map_err(ProvideFailure::NotSent)?;
    let response:providers::ProviderResponse=receive(fd).map_err(ProvideFailure::Unconfirmed)?;
    providers::check_provider_response(nonce,clear,response).map_err(ProvideFailure::Unconfirmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::ids::RequestId;
    use providers::{Payload, ProviderAction, ProviderErrorCode, ProviderRequest, ProviderResponse, SCHEMA_V5};
    fn pair() -> (OwnedFd, OwnedFd) {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr()) }, 0);
        let (a, b) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        timeout(&a).unwrap();
        (a, b)
    }
    fn request() -> ProviderRequest {
        ProviderRequest { schema: SCHEMA_V5, nonce: RequestId([3; 16]), action: ProviderAction::ProvisionProviders, item: None,
            employee_id: None, label: Some("sales".into()),
            payload: Some(Payload::new(zeroize::Zeroizing::new(br#"{"providers":[]}"#.to_vec())).unwrap()), clear: false }
    }

    #[test]
    fn provide_failure_before_the_send_is_not_sent() {
        let (client, server) = pair();
        drop(server);
        assert!(matches!(provide_on(&client, request()), Err(ProvideFailure::NotSent(_))));
    }

    #[test]
    fn provide_failure_after_the_send_is_unconfirmed() {
        // The peer takes the request and closes without a reply (a lost reply).
        let (client, server) = pair();
        let peer = std::thread::spawn(move || { let received = receive_frame(&server).is_ok(); drop(server); received });
        assert!(matches!(provide_on(&client, request()), Err(ProvideFailure::Unconfirmed(_))));
        assert!(peer.join().unwrap(), "the request was delivered");
        // A reply that does not match the request (another nonce).
        let (client, server) = pair();
        let peer = std::thread::spawn(move || {
            let _ = receive_frame(&server);
            send(&server, &ProviderResponse::answer(RequestId([9; 16]), Err(ProviderErrorCode::NotReady))).unwrap();
        });
        assert!(matches!(provide_on(&client, request()), Err(ProvideFailure::Unconfirmed(_))));
        peer.join().unwrap();
    }

    #[test]
    fn provide_isolation_unavailable_reply_is_a_refusal_code() {
        // Root's 4 s bound answers `isolation_unavailable`; the caller reports it as unconfirmed.
        let (client, server) = pair();
        let peer = std::thread::spawn(move || {
            let _ = receive_frame(&server);
            send(&server, &ProviderResponse::answer(RequestId([3; 16]), Err(ProviderErrorCode::IsolationUnavailable))).unwrap();
        });
        assert_eq!(provide_on(&client, request()), Ok(Err(ProviderErrorCode::IsolationUnavailable)));
        peer.join().unwrap();
    }
}
