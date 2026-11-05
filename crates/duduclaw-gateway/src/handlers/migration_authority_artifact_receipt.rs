//! An approved artifact is prepared and fsynced before its encrypted plan is
//! committed. Publication preserves that inode, making a retry distinguishable
//! from an unrelated same-content normal write. No live path exists at prepare.
use super::*;
#[derive(Serialize,Deserialize)]
pub(super) struct Prepared {source:String,identity:Identity}
#[derive(Serialize,Deserialize,PartialEq,Eq)]
struct Identity {device:u64,inode:u64,mtime:i64,nanos:i64,len:u64}
fn identity(path:&Path)->Result<Identity> {
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        let m=std::fs::symlink_metadata(path).map_err(|_|Error::Storage)?;
        if !m.file_type().is_file() {return Err(Error::Storage);}
        Ok(Identity {device:m.dev(),inode:m.ino(),mtime:m.mtime(),nanos:m.mtime_nsec(),len:m.len()})
    }
    #[cfg(not(unix))] {let _=path;Err(Error::Denied)}
}
impl Prepared {
    pub(super) fn new(home:&Path,item:&ConfirmationItem,content:&str)->Result<Self> {
        let source=format!("artifact-{}-{}",operation(&item.binding),hex::encode(item.binding.proposal_digest.0));
        let path=home.join("migration-authority").join(&source);
        let mut opts=std::fs::OpenOptions::new();opts.write(true).create_new(true);
        #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;opts.mode(0o600).custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC);}
        match opts.open(&path) {
            Ok(mut f)=>{f.write_all(content.as_bytes()).and_then(|_|f.sync_all()).map_err(|_|Error::Storage)?;},
            Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists=>{if read_optional(&path)?!=content {return Err(Error::Stale);}},
            Err(_)=>return Err(Error::Storage),
        }
        std::fs::File::open(path.parent().ok_or(Error::Storage)?).and_then(|f|f.sync_all()).map_err(|_|Error::Storage)?;
        Ok(Self {source,identity:identity(&path)?})
    }
    pub(super) fn owns(&self,path:&Path)->bool {identity(path).is_ok_and(|id|id==self.identity)}
    pub(super) fn publish(&self,home:&Path,path:&Path,before:Option<&str>,after:&str)->Result<()> {
        let source=home.join("migration-authority").join(&self.source);
        duduclaw_core::with_file_lock(path,|| {
            let current=super::artifacts::optional(path).map_err(|_|std::io::Error::other("artifact unavailable"))?;
            if current.as_deref()==Some(after) && self.owns(path) {return Ok(());}
            if current.as_deref()!=before {return Err(std::io::Error::other("artifact changed"));}
            if !self.owns(&source) || read_optional(&source).map_err(|_|std::io::Error::other("prepared artifact unavailable"))?!=after {return Err(std::io::Error::other("prepared artifact changed"));}
            crate::product_policy::validate_write(home,path,before.unwrap_or(""),after).map_err(std::io::Error::other)?;
            // Rename preserves the pre-recorded inode and leaves a single link,
            // as required by the normal skill-file editor's anti-hardlink gate.
            std::fs::rename(&source,path)?;
            sync_parent(&source)?;sync_parent(path)
        }).map_err(|_|Error::Apply)
    }
    pub(super) fn restore(&self,home:&Path,path:&Path,before:Option<&str>,after:&str)->Result<()> {
        let source=home.join("migration-authority").join(&self.source);
        duduclaw_core::with_file_lock(path,|| {
            let current=super::artifacts::optional(path).map_err(|_|std::io::Error::other("artifact unavailable"))?;
            if current.as_deref()==before {return Ok(());}
            if current.as_deref()==Some(after) && self.owns(path) {
                if source.exists() {return Err(std::io::Error::other("prepared artifact conflict"));}
                std::fs::rename(path,&source)?;sync_parent(&source)?;sync_parent(path)?;
            } else if !(current.is_none()&&self.owns(&source)&&read_optional(&source).map_err(|_|std::io::Error::other("prepared artifact unavailable"))?==after) {
                return Err(std::io::Error::other("artifact ownership changed"));
            }
            if let Some(before)=before {config_commit::replace_owner_only(path,before)?;}
            sync_parent(path)
        }).map_err(|_|Error::Compensation)
    }
}
fn sync_parent(path:&Path)->std::io::Result<()> {
    std::fs::File::open(path.parent().ok_or_else(||std::io::Error::other("artifact parent"))?).and_then(|f|f.sync_all())
}
