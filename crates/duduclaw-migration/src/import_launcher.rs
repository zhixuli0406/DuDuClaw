//! Root-only, one-source transient service. No parser runs with the root uid.
use std::{collections::BTreeMap, fs::{self, File}, io::{Read, Write}, os::{fd::AsRawFd, unix::{fs::{MetadataExt, PermissionsExt}, net::{UnixListener, UnixStream}}}, path::{Path, PathBuf}, process::{Command, Stdio}, time::{Duration, Instant}};
use crate::Error;
use duduclaw_isolation::migration::MAX_IMPORT_STREAM_BYTES;

const TIMEOUT: Duration = Duration::from_secs(75);
use crate::import_profile::{CALLS, sandbox};

fn trusted_executable(path: &Path) -> Result<PathBuf, Error> {
    let path=fs::canonicalize(path)?;
    for entry in path.ancestors() {
        let meta=fs::symlink_metadata(entry)?;
        if meta.uid()!=0 || meta.mode()&0o022!=0 || meta.file_type().is_symlink() {return Err(Error::Authentication);}
    }
    let meta=fs::metadata(&path)?;
    if !meta.is_file() || meta.mode()&0o111==0 {return Err(Error::Invalid);}
    Ok(path)
}
fn executable(name: &str)->Result<PathBuf,Error> {
    for prefix in ["/usr/bin","/bin"] {
        let path=Path::new(prefix).join(name);
        if path.try_exists()? {return trusted_executable(&path);}
    }
    Err(Error::Unavailable)
}
fn command(name:&str,args:&[String])->Result<String,Error> {
    let mut output=tempfile::tempfile()?;
    let mut child=Command::new(executable(name)?).args(args).env_clear()
        .env("PATH","/usr/sbin:/usr/bin:/sbin:/bin").env("LC_ALL","C")
        .stdin(Stdio::null()).stdout(output.try_clone()?).stderr(Stdio::null()).spawn()?;
    let deadline=Instant::now()+Duration::from_secs(10);
    let status=loop {
        if output.metadata()?.len()>65536 || Instant::now()>=deadline {
            let _=child.kill();let _=child.wait();return Err(Error::Unavailable);
        }
        if let Some(status)=child.try_wait()? {break status;}
        std::thread::sleep(Duration::from_millis(10));
    };
    if !status.success() {return Err(Error::Unavailable);}
    use std::io::{Seek,SeekFrom};output.seek(SeekFrom::Start(0))?;
    let mut bytes=Vec::new();output.take(65537).read_to_end(&mut bytes)?;
    if bytes.len()>65536 {return Err(Error::Limit);}
    String::from_utf8(bytes).map_err(|_|Error::Invalid)
}
fn show(unit:&str)->Result<BTreeMap<String,String>,Error> {
    let output=command("systemctl", &["show".into(),unit.into(),"--no-pager".into(),
        "--property=MainPID,DynamicUser,PrivateNetwork,PrivateTmp,ProtectSystem,ProtectHome,NoNewPrivileges,CapabilityBoundingSet,AmbientCapabilities,MemoryMax,TasksMax,RuntimeMaxUSec,LimitFSIZE,LimitNOFILE,LimitCORE,RestrictAddressFamilies,SystemCallFilter,InaccessiblePaths,Result,ExecMainCode,ExecMainStatus,ActiveState,SubState".into()])?;
    let mut values=BTreeMap::new();
    for line in output.lines() {let (key,value)=line.split_once('=').ok_or(Error::Readback)?;
        if values.insert(key.into(),value.into()).is_some() {return Err(Error::Readback);}}
    Ok(values)
}

struct Unit(String);
impl Drop for Unit {
    fn drop(&mut self) {
        let _=command("systemctl", &["stop".into(),self.0.clone()]);
        let _=command("systemctl", &["reset-failed".into(),self.0.clone()]);
    }
}
fn source(file:&File)->Result<(),Error> {
    let metadata=file.metadata()?;
    // SAFETY: fcntl observes a borrowed, live descriptor without changing it.
    let flags=unsafe {libc::fcntl(file.as_raw_fd(),libc::F_GETFL)};
    if flags<0 || flags&libc::O_ACCMODE!=libc::O_RDONLY || flags&libc::O_PATH!=0 || !metadata.is_file()
        || metadata.nlink()!=1 || metadata.len()>256*1024*1024 {return Err(Error::Invalid);}
    Ok(())
}
fn send_fds(stream:&UnixStream,sources:&[&File])->Result<(),Error> {
    let mut marker=[b'F'];
    let mut iov=libc::iovec {iov_base:marker.as_mut_ptr().cast(),iov_len:1};
    // usize gives cmsghdr its required alignment. At most two descriptors.
    let mut control=[0usize;8];
    let bytes=sources.len()*std::mem::size_of::<libc::c_int>();
    let descriptors:Vec<libc::c_int>=sources.iter().map(|f|f.as_raw_fd()).collect();
    // SAFETY: all pointers reference live buffers with the stated lengths. The
    // receiver takes its own references; the caller retains ownership of sources.
    unsafe {
        let mut message:libc::msghdr=std::mem::zeroed();
        message.msg_iov=&mut iov;message.msg_iovlen=1;message.msg_control=control.as_mut_ptr().cast();
        message.msg_controllen=libc::CMSG_SPACE(bytes as u32) as usize;
        let header=libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level=libc::SOL_SOCKET;(*header).cmsg_type=libc::SCM_RIGHTS;
        (*header).cmsg_len=libc::CMSG_LEN(bytes as u32) as usize;
        std::ptr::copy_nonoverlapping(descriptors.as_ptr(),libc::CMSG_DATA(header).cast(),descriptors.len());
        if libc::sendmsg(stream.as_raw_fd(),&message,libc::MSG_NOSIGNAL)!=1 {return Err(Error::Storage);}
    }
    Ok(())
}

/// The request is a bounded importer JSON request, never a shell command. The
/// caller pins source files with openat2 in the quarantined tree, and validates
/// the returned JSONL against its host-built grant before forwarding anything.
/// No bytes are returned on worker failure, timeout or sandbox-readback failure.
pub fn launch(worker:&Path,request_json:&[u8],sources:&[&File])->Result<Vec<u8>,Error> {
    launch_protected(worker,request_json,sources,&[])
}

/// The root pipeline additionally hides its actual fresh home and quarantine,
/// including installations using a non-default root-owned state directory.
pub fn launch_protected(worker:&Path,request_json:&[u8],sources:&[&File],protected_paths:&[&Path])->Result<Vec<u8>,Error> {
    if !rustix::process::geteuid().is_root() {return Err(Error::Authentication);}
    if request_json.len()>8192 || !(1..=2).contains(&sources.len()) {return Err(Error::Invalid);}
    for file in sources {source(file)?;}
    let mut protected=vec!["-/var/lib/duduclaw".to_string(),"-/var/lib/duduclaw-migrate".to_string()];
    for path in protected_paths {
        let text=path.to_str().ok_or(Error::Invalid)?;
        if !path.is_absolute() || path.parent().is_none()
            || !text.bytes().all(|b|b.is_ascii_alphanumeric()||b"/_-.".contains(&b))
            || path.components().any(|c|!matches!(c,std::path::Component::RootDir|std::path::Component::Normal(_)))
            || !fs::symlink_metadata(path)?.is_dir() {return Err(Error::Invalid);}
        protected.push(text.into());
    }
    let worker=trusted_executable(worker)?;
    let run=fs::symlink_metadata("/run")?;
    if !run.is_dir() || run.uid()!=0 || run.mode()&0o022!=0 {return Err(Error::Authentication);}
    let dir=tempfile::Builder::new().prefix("duduclaw-import-").tempdir_in("/run")?;
    fs::set_permissions(dir.path(),fs::Permissions::from_mode(0o711))?;
    let path=dir.path().join("handoff.sock");
    let listener=UnixListener::bind(&path)?;
    fs::set_permissions(&path,fs::Permissions::from_mode(0o666))?;
    listener.set_nonblocking(true)?;
    let nonce:[u8;16]=rand::random();
    let name=format!("duduclaw-import-{}.service",nonce.iter().map(|b|format!("{b:02x}")).collect::<String>());
    let guard=Unit(name.clone());
    let mut args=vec!["--quiet".into(),"--no-ask-password".into(),format!("--unit={name}")];
    for property in ["Type=exec","RemainAfterExit=yes","DynamicUser=yes","PrivateNetwork=yes","IPAddressDeny=any",
        "PrivateTmp=yes","TemporaryFileSystem=/tmp:rw,nosuid,nodev,noexec,size=600M,nr_inodes=128",
        "ProtectSystem=strict","ProtectHome=yes",
        "PrivateDevices=yes","ProtectKernelTunables=yes","ProtectKernelModules=yes","ProtectControlGroups=yes",
        "ProtectKernelLogs=yes","RestrictNamespaces=yes","RestrictSUIDSGID=yes","LockPersonality=yes",
        "CapabilityBoundingSet=","AmbientCapabilities=","NoNewPrivileges=yes","RestrictAddressFamilies=AF_UNIX",
        "SystemCallArchitectures=native","SystemCallErrorNumber=EPERM","MemoryMax=1G","TasksMax=1",
        "RuntimeMaxSec=60","LimitFSIZE=268435456","LimitNOFILE=32","LimitCORE=0","UMask=0077",
        "StandardOutput=null","StandardError=null"] {args.push(format!("--property={property}"));}
    args.push(format!("--property=SystemCallFilter={CALLS}"));
    args.push(format!("--property=InaccessiblePaths={}",protected.join(" ")));
    args.extend(["--".into(),worker.to_str().ok_or(Error::Invalid)?.into(),"--handoff".into(),path.to_str().ok_or(Error::Invalid)?.into()]);
    command("systemd-run",&args)?;
    let deadline=Instant::now()+TIMEOUT;
    let mut stream=loop {
        match listener.accept() {
            Ok((stream,_))=>break stream,
            Err(error) if error.kind()==std::io::ErrorKind::WouldBlock=>{},
            Err(_)=>return Err(Error::Storage),
        }
        if Instant::now()>=deadline {return Err(Error::Unavailable);}
        std::thread::sleep(Duration::from_millis(10));
    };
    stream.set_read_timeout(Some(Duration::from_secs(65)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut cred=libc::ucred {pid:0,uid:0,gid:0};let mut size=std::mem::size_of_val(&cred) as libc::socklen_t;
    // SAFETY: exact writable ucred/length buffers for the connected socket.
    if unsafe {libc::getsockopt(stream.as_raw_fd(),libc::SOL_SOCKET,libc::SO_PEERCRED,
        (&mut cred as *mut libc::ucred).cast(),&mut size)}!=0 || size as usize!=std::mem::size_of_val(&cred)
        || !(61184..=65519).contains(&cred.uid) {return Err(Error::Authentication);}
    let properties=show(&name)?;sandbox(&properties,&protected)?;
    if properties.get("MainPID").and_then(|p|p.parse::<i32>().ok())!=Some(cred.pid) {return Err(Error::Authentication);}
    let status=fs::read_to_string(format!("/proc/{}/status",cred.pid))?;
    for (key,value) in [("NoNewPrivs:","1"),("Seccomp:","2"),("CapEff:","0000000000000000"),
        ("CapPrm:","0000000000000000"),("CapBnd:","0000000000000000")] {
        if !status.lines().any(|line|line.split_once(':').is_some_and(|(name,v)|format!("{name}:")==key && v.trim()==value)) {
            return Err(Error::Readback);
        }
    }
    if fs::read_link(format!("/proc/{}/ns/net",cred.pid))?==fs::read_link("/proc/self/ns/net")? {return Err(Error::Readback);}
    send_fds(&stream,sources)?;
    stream.write_all(&(request_json.len() as u32).to_be_bytes())?;stream.write_all(request_json)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut bytes=Vec::new();(&mut stream).take(MAX_IMPORT_STREAM_BYTES+1).read_to_end(&mut bytes)?;
    if bytes.len() as u64>MAX_IMPORT_STREAM_BYTES {return Err(Error::Limit);}
    loop {
        let properties=show(&name)?;sandbox(&properties,&protected)?;
        if properties.get("SubState").map(String::as_str)==Some("exited") {
            if properties.get("Result").map(String::as_str)!=Some("success")
                || properties.get("ExecMainStatus").map(String::as_str)!=Some("0")
                || properties.get("ExecMainCode").map(String::as_str)!=Some("1") {return Err(Error::Readback);}
            break;
        }
        if properties.get("ActiveState").map(String::as_str)==Some("failed") {
            // Only the explicit, normally exited parser code is recoverable.
            // Sandbox setup failures, signals, I/O errors and timeout never are.
            if properties.get("Result").map(String::as_str)==Some("exit-code")
                && properties.get("ExecMainCode").map(String::as_str)==Some("1")
                && properties.get("ExecMainStatus").map(String::as_str)==Some("4") {
                return Err(Error::SourceRejected);
            }
            return Err(Error::Readback);
        }
        if Instant::now()>=deadline {return Err(Error::Readback);}
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(guard);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn importer_launcher_rejects_writable_descriptors() {
        let file=tempfile::tempfile().unwrap();assert!(source(&file).is_err());
        let file=tempfile::NamedTempFile::new().unwrap();
        assert!(source(&File::open(file.path()).unwrap()).is_ok());
    }
    #[test]
    fn importer_launcher_missing_properties_fail_closed() {
        assert!(sandbox(&BTreeMap::new(),&[]).is_err());
    }
    #[test]
    #[ignore="requires root, systemd and installed importer; proves parser refusal is distinct from sandbox failure"]
    fn iso_root_importer_wrong_schema_is_rejected_source() {
        let binary=std::env::var_os("DUDUCLAW_IMPORT_TEST_BINARY").expect("set installed importer path");
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("sessions.db");
        let conn=rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE malicious(role TEXT);").unwrap();drop(conn);
        let source=File::open(path).unwrap();
        let request=serde_json::json!({"version":1,"migration_id":vec![7;16],"employee":vec![8;16],
            "source_id":"bad-schema","kind":"sessions_sqlite","title":"","legacy_agent":"sales","has_wal":false,"blob_handle":null});
        assert_eq!(launch(Path::new(&binary),&serde_json::to_vec(&request).unwrap(),&[&source]),Err(Error::SourceRejected));
    }
    #[test]
    #[ignore="requires root, systemd and installed importer; exercises main plus WAL descriptors in the actual sandbox"]
    fn iso_root_importer_sqlite_wal_fd_transport() {
        let binary=std::env::var_os("DUDUCLAW_IMPORT_TEST_BINARY").expect("set installed importer path");
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("sessions.db");
        let conn=rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../../duduclaw-import/src/session-v1793.sql")).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); INSERT INTO sessions(id,agent_id,last_active,created_at) VALUES ('s','sales','now','now'); INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('s','system','from WAL','now');").unwrap();
        let main=File::open(path).unwrap();let wal=File::open(dir.path().join("sessions.db-wal")).unwrap();
        let request=serde_json::json!({"version":1,"migration_id":vec![7;16],"employee":vec![8;16],
            "source_id":"native-wal","kind":"sessions_sqlite","title":"","legacy_agent":"sales","has_wal":true,"blob_handle":null});
        let bytes=launch(Path::new(&binary),&serde_json::to_vec(&request).unwrap(),&[&main,&wal]).unwrap();
        let record:serde_json::Value=serde_json::from_slice(&bytes).unwrap();
        assert!(record["content"]["text"].as_str().unwrap().contains("from WAL"));
        drop(conn);
    }
    #[test]
    #[ignore="requires root, systemd, installed root-owned importer; DUDUCLAW_IMPORT_TEST_BINARY specifies the binary"]
    fn iso_root_importer_dynamic_user_fd_transport() {
        let binary=std::env::var_os("DUDUCLAW_IMPORT_TEST_BINARY").expect("set installed importer path");
        let file=tempfile::NamedTempFile::new().unwrap();fs::write(file.path(),b"legacy text").unwrap();
        let readonly=File::open(file.path()).unwrap();
        let request=serde_json::json!({"version":1,"migration_id":vec![7;16],"employee":vec![8;16],
            "source_id":"native-test","kind":"memory_text","title":"","legacy_agent":null,"has_wal":false,"blob_handle":null});
        let bytes=launch(Path::new(&binary),&serde_json::to_vec(&request).unwrap(),&[&readonly]).unwrap();
        let record:serde_json::Value=serde_json::from_slice(&bytes).unwrap();
        assert_eq!(record["content"]["text"],"legacy text");
    }
}
