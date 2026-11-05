//! Exclusive import boot. This branch runs before normal gateway initialization,
//! so no public listener, channel, timer, model runtime or employee loader exists.
use std::path::Path;
use duduclaw_migration::{Error, import_manifest::{ImportManifest, IMPORT_MANIFEST}};
pub use duduclaw_migration::import_manifest::IMPORT_ONLY_ENV;

fn gateway_error(_: Error) -> duduclaw_core::error::DuDuClawError {
    duduclaw_core::error::DuDuClawError::Gateway("migration import startup refused".into())
}

/// Called by the CLI before config/bootstrap and again by start_gateway for library
/// callers. An explicit import request can never fall through to normal startup.
pub async fn maybe_run(home: &Path) -> duduclaw_core::error::Result<bool> {
    let requested = std::env::var_os(IMPORT_ONLY_ENV);
    #[cfg(target_os = "linux")]
    {
        let manifest = linux::read_manifest(Path::new(IMPORT_MANIFEST)).map_err(gateway_error)?;
        use duduclaw_migration::import_manifest::{boot_decision,ImportBootDecision};
        match boot_decision(requested.as_deref(),manifest.as_ref(),home).map_err(gateway_error)? {
            ImportBootDecision::Normal => Ok(false),
            ImportBootDecision::ImportOnly => {
                let plan=manifest.ok_or_else(||gateway_error(Error::Authentication))?;
                linux::run(plan).await.map_err(gateway_error)?;
                Ok(true)
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = home;
        if requested.is_some() { Err(gateway_error(Error::Unavailable)) } else { Ok(false) }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{fs::OpenOptions, io::Read, os::{fd::{AsRawFd, FromRawFd}, unix::{fs::{FileTypeExt, MetadataExt, OpenOptionsExt}, net::UnixListener}}, path::PathBuf};
    use duduclaw_migration::{import_manifest::MAX_MANIFEST_BYTES, import_receiver::{ImportGrant, ImportedBlob}};
    use crate::migration_import::{GatewayImportWriter, ImportModeGate, serve_import_connection};

    pub(super) fn read_manifest(path: &Path) -> Result<Option<ImportManifest>, Error> {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind()==std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(_) => {},
        }
        for parent in path.parent().ok_or(Error::Invalid)?.ancestors() {
            let meta=std::fs::symlink_metadata(parent)?;
            if !meta.is_dir() || meta.uid()!=0 || meta.mode()&0o022!=0 { return Err(Error::Authentication); }
        }
        let mut file=OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC|libc::O_NONBLOCK).open(path)?;
        let meta=file.metadata()?;
        if !meta.is_file() || meta.uid()!=0 || meta.nlink()!=1 || meta.mode()&0o022!=0 || meta.len()>MAX_MANIFEST_BYTES {
            return Err(Error::Authentication);
        }
        let mut bytes=Vec::new();
        Read::by_ref(&mut file).take(MAX_MANIFEST_BYTES+1).read_to_end(&mut bytes)?;
        if bytes.len() as u64>MAX_MANIFEST_BYTES {return Err(Error::Limit);}
        let manifest:ImportManifest=serde_json::from_slice(&bytes)?;
        manifest.validate()?;
        Ok(Some(manifest))
    }

    struct ManifestGate { manifest: ImportManifest, path: PathBuf }
    impl ManifestGate {
        fn current(&self) -> Result<ImportManifest, Error> {
            let current=read_manifest(&self.path)?.ok_or(Error::Authentication)?;
            if !self.manifest.same_plan(&current) {return Err(Error::Conflict);}
            Ok(current)
        }
    }
    impl ImportModeGate for ManifestGate {
        fn authorize(&self, grant: &ImportGrant) -> Result<Option<String>, Error> {
            self.readback_import_only()?;
            let entry=self.manifest.grants.iter().find(|entry|entry.grant==*grant).ok_or(Error::Authentication)?;
            Ok(entry.target_agent.clone())
        }
        fn readback_import_only(&self) -> Result<(), Error> {
            if self.current()?.completed || !duduclaw_core::gateway_instance::held(&self.manifest.home)
                || nix::unistd::geteuid().as_raw()!=self.manifest.gateway_uid {
                return Err(Error::Authentication);
            }
            Ok(())
        }
        fn expected_grants(&self) -> Result<Vec<ImportGrant>, Error> {
            self.readback_import_only()?;
            Ok(self.manifest.grants.iter().map(|entry|entry.grant.clone()).collect())
        }
        // Blobs require root copy plus confirmed B/H allocation. No authority or
        // filesystem receipt is inferred from old data or an archive record.
        fn readback_blob(&self, _: &ImportGrant, _: &ImportedBlob, _: bool) -> Result<(), Error> {Err(Error::Unavailable)}
    }

    fn take_listener(uid:u32) -> Result<tokio::net::UnixListener, Error> {
        if std::env::var("LISTEN_PID").ok().and_then(|s|s.parse::<u32>().ok())!=Some(std::process::id())
            || std::env::var("LISTEN_FDS").ok().as_deref()!=Some("1") {return Err(Error::Authentication);}
        let path=Path::new(duduclaw_migration::ids::IMPORT_SOCKET);
        let meta=std::fs::symlink_metadata(path)?;
        if !meta.file_type().is_socket() || meta.uid()!=uid || meta.mode()&0o777!=0o600 {return Err(Error::Authentication);}
        let mut accepting:libc::c_int=0;
        let mut len=std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: fd 3 is systemd's documented activation descriptor; getsockopt
        // only fills the exactly-sized integer buffer and does not take ownership.
        if unsafe {libc::getsockopt(3,libc::SOL_SOCKET,libc::SO_ACCEPTCONN,&mut accepting as *mut _ as *mut libc::c_void,&mut len)}!=0
            || accepting!=1 {return Err(Error::Authentication);}
        // SAFETY: ownership of the sole systemd activation fd is taken exactly
        // once, before any normal startup or task can use it.
        let listener=unsafe {UnixListener::from_raw_fd(3)};
        if listener.local_addr()?.as_pathname()!=Some(path) {return Err(Error::Authentication);}
        // SAFETY: fcntl changes only the valid listener's descriptor flags.
        if unsafe {libc::fcntl(listener.as_raw_fd(),libc::F_SETFD,libc::FD_CLOEXEC)}<0 {return Err(Error::Storage);}
        listener.set_nonblocking(true)?;
        tokio::net::UnixListener::from_std(listener).map_err(|_|Error::Storage)
    }

    pub(super) async fn run(manifest: ImportManifest) -> Result<(), Error> {
        if nix::unistd::geteuid().as_raw()!=manifest.gateway_uid {return Err(Error::Authentication);}
        // This early boot branch does not run the normal CLI hardening path.
        // Import has no inherited root-key fd, so force and read back the TCB
        // protections rather than relying on container/root-key detection.
        // SAFETY: these prctl operations use integer arguments, no pointers.
        if unsafe {libc::prctl(libc::PR_SET_DUMPABLE,0,0,0,0)}!=0
            || unsafe {libc::prctl(libc::PR_GET_DUMPABLE,0,0,0,0)}!=0
            || unsafe {libc::prctl(libc::PR_SET_NO_NEW_PRIVS,1,0,0,0)}!=0
            || unsafe {libc::prctl(libc::PR_GET_NO_NEW_PRIVS,0,0,0,0)}!=1 {
            return Err(Error::Authentication);
        }
        let meta=std::fs::symlink_metadata(&manifest.home)?;
        if !meta.is_dir() || meta.uid()!=manifest.gateway_uid || meta.mode()&0o077!=0 {return Err(Error::Authentication);}
        duduclaw_core::gateway_instance::acquire_for_restore(&manifest.home).map_err(|_|Error::Authentication)?;
        let listener=take_listener(manifest.gateway_uid)?;
        let gate=ManifestGate {manifest,path:PathBuf::from(IMPORT_MANIFEST)};
        gate.readback_import_only()?;
        crate::handlers::stage_import_proposals(&gate.manifest.home,&gate.manifest.pending_proposals)?;
        let mut writer=GatewayImportWriter::open(&gate.manifest.home,&gate)?;
        let mut interval=tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                _=interval.tick()=> {
                    if gate.current()?.completed {return Ok(());}
                }
                accepted=listener.accept()=> {
                    let (stream,_)=accepted.map_err(|_|Error::Storage)?;
                    // Invalid requests terminate this connection only. A root
                    // caller receives no success receipt and must not advance.
                    if serve_import_connection(stream,&gate,&mut writer).await.is_err() {
                        tracing::warn!("migration import request refused");
                    }
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn migration_import_boot_missing_manifest_does_not_open_home() {
            let home=tempfile::tempdir().unwrap();
            assert!(read_manifest(&home.path().join("missing")).unwrap().is_none());
            assert_eq!(std::fs::read_dir(home.path()).unwrap().count(),0);
        }
        #[test]
        #[ignore = "requires root on disposable Linux VM; creates isolated files under /run"]
        fn iso_root_migration_import_manifest_refuses_symlinks_and_modified_plan() {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(nix::unistd::geteuid().as_raw(),0,"run ignored test as root");
            let dir=tempfile::tempdir_in("/run").unwrap();
            let path=dir.path().join("manifest.json");
            let plan=ImportManifest {schema_version:1,migration_id:duduclaw_migration::ids::RequestId([8;16]),
                home:PathBuf::from("/var/lib/duduclaw/.duduclaw"),gateway_uid:991,grants:vec![],pending_proposals:vec![],completed:false};
            std::fs::write(&path,serde_json::to_vec(&plan).unwrap()).unwrap();
            std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(read_manifest(&path).unwrap(),Some(plan.clone()));
            let gate=ManifestGate {manifest:plan.clone(),path:path.clone()};
            let mut changed=plan.clone();changed.completed=true;
            std::fs::write(&path,serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(gate.current().unwrap().completed);
            changed.gateway_uid=992;
            std::fs::write(&path,serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(gate.current().is_err());
            let link=dir.path().join("linked.json");
            std::os::unix::fs::symlink(&path,&link).unwrap();
            assert!(read_manifest(&link).is_err());
            std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o666)).unwrap();
            assert!(read_manifest(&path).is_err());
        }
        #[test]
        fn migration_import_boot_refuses_untrusted_manifest_parent() {
            let dir=tempfile::tempdir().unwrap();
            let path=dir.path().join("manifest.json");
            std::fs::write(&path,b"{}").unwrap();
            assert!(read_manifest(&path).is_err());
        }
    }
}
