//! Gateway-side validation of a complete worker stream. This module never opens legacy files.
use std::{collections::BTreeSet, io::{BufRead, Read}};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::Error;
pub use duduclaw_isolation::{ids::{EmployeeId, RequestId, Sha256Digest}, migration::{ImportRecord, ImportedContent, ImportedBlob}};
use duduclaw_isolation::migration::{ImportBudget, MAX_IMPORT_RECORD_BYTES, MAX_IMPORT_STREAM_BYTES, MAX_IMPORT_RECORDS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentClass { Conversation, ChannelHistory, Memory, WikiPage, TaskHistory, LegacyAuditArchive, Attachment, WorkProduct }
impl ContentClass {
    fn of(content: &ImportedContent) -> Self {
        match content {
            ImportedContent::Conversation { .. } => Self::Conversation,
            ImportedContent::ChannelHistory { .. } => Self::ChannelHistory,
            ImportedContent::Memory { .. } => Self::Memory,
            ImportedContent::WikiPage { .. } => Self::WikiPage,
            ImportedContent::TaskHistory { .. } => Self::TaskHistory,
            ImportedContent::LegacyAuditArchive { .. } => Self::LegacyAuditArchive,
            ImportedContent::Attachment { .. } => Self::Attachment,
            ImportedContent::WorkProduct { .. } => Self::WorkProduct,
        }
    }
}

/// Issued by the root coordinator after a successful, sandbox-verified worker exit.
/// The gateway compares this to its active import-mode manifest, not worker input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportGrant {
    pub migration_id: RequestId,
    pub employee: Option<EmployeeId>,
    pub source_id: String,
    pub class: ContentClass,
    pub stream_bytes: u64,
    pub records: u64,
    pub stream_sha256: Sha256Digest,
}
impl ImportGrant {
    pub fn validate(&self) -> Result<(), Error> {
        if self.source_id.is_empty() || self.source_id.len() > 400
            || self.records > MAX_IMPORT_RECORDS || self.stream_bytes > MAX_IMPORT_STREAM_BYTES
            || (self.class != ContentClass::LegacyAuditArchive && self.employee.is_none()) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

/// Has no public constructor: consumers can only obtain records after complete validation.
pub struct StagedImport { grant: ImportGrant, records: Vec<ImportRecord> }
impl StagedImport {
    pub fn grant(&self) -> &ImportGrant { &self.grant }
    pub fn records(&self) -> &[ImportRecord] { &self.records }

    pub fn receive(grant: ImportGrant, source: impl Read, worker_succeeded: bool) -> Result<Self, Error> {
        grant.validate()?;
        if !worker_succeeded { return Err(Error::Readback); }
        // Read at most the declared length plus one; no unbounded read_line allocation.
        let mut bytes = Vec::new();
        source.take(grant.stream_bytes + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != grant.stream_bytes
            || Sha256Digest(Sha256::digest(&bytes).into()) != grant.stream_sha256 {
            return Err(Error::Readback);
        }
        if !bytes.is_empty() && bytes.last() != Some(&b'\n') { return Err(Error::Invalid); }
        let mut budget = ImportBudget::default();
        let mut records = Vec::new();
        let mut ids = BTreeSet::new();
        let prefix = format!("{}:", grant.source_id);
        for line in bytes.split(|byte| *byte == b'\n').take_while(|line| !line.is_empty()) {
            if line.len() > MAX_IMPORT_RECORD_BYTES { return Err(Error::Limit); }
            budget.observe(line.len()).map_err(|_| Error::Limit)?;
            let record = ImportRecord::decode(line).map_err(|_| Error::Invalid)?;
            if record.migration_id != grant.migration_id || record.employee != grant.employee
                || record.sequence != records.len() as u64 || ContentClass::of(&record.content) != grant.class
                || record.source_record_id.strip_prefix(&prefix).is_none_or(str::is_empty)
                || !ids.insert(record.source_record_id.clone()) {
                return Err(Error::Invalid);
            }
            records.push(record);
        }
        // Blank/interior lines must not hide a suffix, including a zero-record request.
        if records.len() as u64 != grant.records
            || bytes.split_inclusive(|byte| *byte == b'\n').count() != records.len() {
            return Err(Error::Invalid);
        }
        Ok(Self { grant, records })
    }
}

/// Reads a bounded JSON grant followed by exactly its declared JSONL bytes. The
/// caller authenticates the root peer and supplies its trusted manifest grant.
pub fn read_grant(reader: &mut impl BufRead) -> Result<ImportGrant, Error> {
    let mut line = Vec::new();
    reader.take(8193).read_until(b'\n', &mut line)?;
    if line.len() > 8192 || line.last() != Some(&b'\n') { return Err(Error::Limit); }
    let grant: ImportGrant = serde_json::from_slice(&line)?;
    grant.validate()?;
    Ok(grant)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> (ImportGrant, Vec<u8>) {
        let record = ImportRecord { schema_version: 1, migration_id: RequestId([1;16]), employee: Some(EmployeeId([2;16])),
            sequence: 0, source_record_id: "source:text".into(), content: ImportedContent::Memory { text: "legacy".into() } };
        let mut bytes = serde_json::to_vec(&record).unwrap(); bytes.push(b'\n');
        let grant = ImportGrant { migration_id: record.migration_id, employee: record.employee, source_id: "source".into(),
            class: ContentClass::Memory, stream_bytes: bytes.len() as u64, records: 1, stream_sha256: Sha256Digest(Sha256::digest(&bytes).into()) };
        (grant, bytes)
    }
    #[test]
    fn import_receiver_requires_complete_successful_worker() {
        let (grant, bytes) = sample();
        assert_eq!(StagedImport::receive(grant.clone(), bytes.as_slice(), true).unwrap().records().len(), 1);
        assert!(StagedImport::receive(grant.clone(), bytes.as_slice(), false).is_err());
        assert!(StagedImport::receive(grant.clone(), &bytes[..bytes.len()-1], true).is_err());
        let mut extra = bytes.clone(); extra.push(b'\n');
        assert!(StagedImport::receive(grant, extra.as_slice(), true).is_err());
    }
    #[test]
    fn import_receiver_rejects_oversize_before_reading() {
        struct NeverRead;
        impl Read for NeverRead { fn read(&mut self,_:&mut [u8])->std::io::Result<usize> { panic!("invalid grant must not read input") } }
        let (mut grant,_) = sample(); grant.stream_bytes=u64::MAX;
        assert!(StagedImport::receive(grant,NeverRead,true).is_err());
        let (grant,_) = sample(); assert!(StagedImport::receive(grant,NeverRead,false).is_err());
    }
    #[test]
    fn import_receiver_binds_host_identity_source_and_category() {
        let (grant, bytes) = sample();
        for index in 0..5 {
            let mut changed = grant.clone();
            match index { 0 => changed.migration_id = RequestId([3;16]), 1 => changed.employee = None,
                2 => changed.source_id = "other".into(), 3 => changed.class = ContentClass::TaskHistory, _ => changed.records = 2 }
            assert!(StagedImport::receive(changed, bytes.as_slice(), true).is_err());
        }
    }
    #[test]
    fn import_receiver_rejects_hidden_suffix_duplicate_and_wrong_sequence() {
        let (mut grant, bytes) = sample();
        for separator in [b"\n".as_slice(), b"".as_slice()] {
            let mut duplicate = bytes.clone(); duplicate.extend(separator); duplicate.extend(&bytes);
            grant.stream_bytes = duplicate.len() as u64; grant.records = 2;
            grant.stream_sha256 = Sha256Digest(Sha256::digest(&duplicate).into());
            assert!(StagedImport::receive(grant.clone(), duplicate.as_slice(), true).is_err());
        }
    }
}

/// Root relay; the input must be captured only after worker exit and sandbox
/// readback. The gateway socket is supplied by the isolated boot path, never HTTP.
#[cfg(target_os = "linux")]
pub fn send_import_stream(grant: ImportGrant, source: impl Read, worker_succeeded: bool, gateway_uid: u32) -> Result<u64, Error> {
    use std::{io::Write, os::unix::{fs::MetadataExt, net::UnixStream}, path::Path};
    if !rustix::process::geteuid().is_root() || gateway_uid == 0 { return Err(Error::Authentication); }
    grant.validate()?;
    if !worker_succeeded { return Err(Error::Readback); }
    let mut bytes = Vec::new();
    source.take(grant.stream_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    let staged = StagedImport::receive(grant,bytes.as_slice(),worker_succeeded)?;
    let path = Path::new(duduclaw_isolation::ids::IMPORT_SOCKET);
    for parent in [Path::new("/run"),path.parent().ok_or(Error::Invalid)?] {
        let meta = std::fs::symlink_metadata(parent)?;
        if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 { return Err(Error::Authentication); }
    }
    let meta = std::fs::symlink_metadata(path)?;
    use std::os::unix::fs::FileTypeExt;
    if !meta.file_type().is_socket() || meta.uid() != gateway_uid || meta.mode() & 0o007 != 0 { return Err(Error::Authentication); }
    let mut socket = UnixStream::connect(path)?;
    let timeout = Some(std::time::Duration::from_secs(120));
    socket.set_read_timeout(timeout)?; socket.set_write_timeout(timeout)?;
    authenticate_gateway_stream(&mut socket,gateway_uid)?;
    socket.write_all(&serde_json::to_vec(staged.grant())?)?; socket.write_all(b"\n")?;
    socket.write_all(&bytes)?; socket.shutdown(std::net::Shutdown::Write)?;
    let mut reply = Vec::new(); socket.take(1025).read_to_end(&mut reply)?;
    if reply.len()>1024 { return Err(Error::Limit); }
    #[derive(Deserialize)] #[serde(deny_unknown_fields)] struct Receipt { records:u64, complete:bool }
    let receipt:Receipt = serde_json::from_slice(&reply)?;
    if !receipt.complete || receipt.records != staged.grant().records { return Err(Error::Readback); }
    Ok(receipt.records)
}

/// Authenticate the process accepting a systemd-owned listening socket. SO_PEERCRED
/// alone names PID 1 at listen time, not the gateway handling this connection.
/// Enable per-message kernel credentials, then request a one-byte gateway reply.
#[cfg(target_os="linux")]
pub fn authenticate_gateway_stream(socket:&mut std::os::unix::net::UnixStream,gateway_uid:u32)->Result<(),Error> {
    use std::{io::Write,os::fd::AsRawFd};
    if gateway_uid==0 {return Err(Error::Authentication);}
    let enabled:libc::c_int=1;
    // SAFETY: borrowed socket fd and exactly-sized integer option buffer.
    if unsafe {libc::setsockopt(socket.as_raw_fd(),libc::SOL_SOCKET,libc::SO_PASSCRED,
        (&enabled as *const libc::c_int).cast(),std::mem::size_of_val(&enabled) as libc::socklen_t)}!=0 {return Err(Error::Authentication);}
    socket.write_all(b"G")?;
    let mut byte=[0u8;1];
    let mut iov=libc::iovec {iov_base:byte.as_mut_ptr().cast(),iov_len:1};
    // usize storage has the alignment required for cmsghdr on Linux targets.
    let mut control=[0usize;16];
    // SAFETY: all-zero msghdr is initialized with live buffers below.
    let mut message:libc::msghdr=unsafe {std::mem::zeroed()};
    message.msg_iov=&mut iov;message.msg_iovlen=1;
    message.msg_control=control.as_mut_ptr().cast();message.msg_controllen=std::mem::size_of_val(&control);
    // SAFETY: message references writable buffers of the supplied lengths.
    let received=unsafe {libc::recvmsg(socket.as_raw_fd(),&mut message,libc::MSG_CMSG_CLOEXEC)};
    if received<0 {return Err(Error::Authentication);}
    let mut credentials=None;
    let mut unexpected=false;
    // SAFETY: kernel-generated ancillary headers lie in message's aligned buffer;
    // CMSG_NXTHDR bounds traversal by the returned msg_controllen.
    unsafe {
        let mut header=libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            let minimum=libc::CMSG_LEN(0) as usize;
            if (*header).cmsg_len<minimum {return Err(Error::Authentication);}
            let size=(*header).cmsg_len-minimum;
            if (*header).cmsg_level==libc::SOL_SOCKET && (*header).cmsg_type==libc::SCM_CREDENTIALS
                && size==std::mem::size_of::<libc::ucred>() && credentials.is_none() {
                credentials=Some(std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<libc::ucred>()));
            } else {
                unexpected=true;
                // Never leak descriptors supplied by an unexpected peer.
                if (*header).cmsg_level==libc::SOL_SOCKET && (*header).cmsg_type==libc::SCM_RIGHTS {
                    for index in 0..size/std::mem::size_of::<libc::c_int>() {
                        libc::close(std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<libc::c_int>().add(index)));
                    }
                }
            }
            header=libc::CMSG_NXTHDR(&message,header);
        }
    }
    if received!=1 || byte!=*b"I" || message.msg_flags&(libc::MSG_CTRUNC|libc::MSG_TRUNC)!=0 || unexpected
        || !credentials.is_some_and(|cred|cred.uid==gateway_uid && cred.pid>0) {return Err(Error::Authentication);}
    Ok(())
}

#[cfg(all(test,target_os="linux"))]
mod activation_tests {
    use super::*;
    #[test]
    #[ignore = "requires Linux root; child drops to nobody and only reads/writes a socketpair"]
    fn iso_root_import_authenticates_acceptor_after_root_socket_activation() {
        use std::os::fd::AsRawFd;
        assert!(rustix::process::geteuid().is_root(),"run ignored test as root");
        let (mut client,server)=std::os::unix::net::UnixStream::pair().unwrap();
        client.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
        client.set_write_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
        let server_fd=server.as_raw_fd();let client_fd=client.as_raw_fd();
        // SAFETY: after fork the child calls only async-signal-safe libc routines,
        // does not touch the allocator/runtime, and exits without Rust destructors.
        let pid=unsafe {libc::fork()};assert!(pid>=0);
        if pid==0 {
            unsafe {
                libc::close(client_fd);
                if libc::setgroups(0,std::ptr::null())!=0 || libc::setgid(65534)!=0 || libc::setuid(65534)!=0 {libc::_exit(2);}
                let mut byte=0u8;
                if libc::read(server_fd,(&mut byte as *mut u8).cast(),1)!=1 || byte!=b'G' {libc::_exit(3);}
                let answer=b'I';
                if libc::write(server_fd,(&answer as *const u8).cast(),1)!=1 {libc::_exit(4);}
                libc::_exit(0);
            }
        }
        drop(server);
        let result=authenticate_gateway_stream(&mut client,65534);
        drop(client);
        let mut status=0;
        // SAFETY: waitpid waits for this test's child and writes one status integer.
        assert_eq!(unsafe {libc::waitpid(pid,&mut status,0)},pid);
        assert_eq!(status,0);
        result.unwrap();
    }
}
