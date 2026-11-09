//! A department is a real shared-wiki directory, never a receipt-only registry.
//! Stage through the normal creator, then publish its recorded inode without
//! replacing an independently created department. Linux is the target platform.
use super::*;
#[derive(Clone,Serialize,Deserialize)]
pub(super) struct DepartmentPlan {name:String,stage:String,device:u64,inode:u64}
#[cfg(unix)]
fn directory_id(path:&Path)->Result<(u64,u64)> {use std::os::unix::fs::MetadataExt;let m=std::fs::symlink_metadata(path).map_err(|_|Error::Storage)?;if !m.file_type().is_dir(){return Err(Error::Storage);}Ok((m.dev(),m.ino()))}
#[cfg(not(unix))]
fn directory_id(_: &Path)->Result<(u64,u64)>{Err(Error::Denied)}
pub(super) fn department_plan(home:&Path,item:&ConfirmationItem)->Result<DepartmentPlan> {
    if !cfg!(target_os="linux") {return Err(Error::Denied);}
    let name=text(item,"name")?;
    if !duduclaw_core::is_valid_department(name) || crate::departments::list_departments(home,&[]).iter().any(|d|d.name==name) {return Err(Error::Stale);}
    let stage=format!("department-{}-{}",operation(&item.binding),hex::encode(item.binding.proposal_digest.0));
    let staging=home.join("migration-authority").join(&stage);create_private_dir(&staging)?;
    let source=crate::departments::wiki_departments_root(&staging).join(name);
    if !source.exists() {crate::departments::create_department(&staging,name,&[]).map_err(|_|Error::Storage)?;}
    plain_directory(&source)?;if std::fs::read_dir(&source).map_err(|_|Error::Storage)?.next().is_some() {return Err(Error::Stale);}
    stamp(&source,&stage)?;
    let (device,inode)=directory_id(&source)?;
    sync(&source)?;sync(source.parent().ok_or(Error::Storage)?)?;
    Ok(DepartmentPlan {name:name.into(),stage,device,inode})
}
impl DepartmentPlan {
    fn source(&self,home:&Path)->PathBuf {crate::departments::wiki_departments_root(&home.join("migration-authority").join(&self.stage)).join(&self.name)}
    fn target(&self,home:&Path)->PathBuf {crate::departments::wiki_departments_root(home).join(&self.name)}
    fn owns(&self,path:&Path)->bool {directory_id(path).is_ok_and(|id|id==(self.device,self.inode)) && receipt(path).as_deref()==Some(self.stage.as_bytes())}
    pub(super) fn needs_stage(&self,home:&Path)->bool {!self.source(home).exists()&&!self.target(home).exists()}
    pub(super) fn publish(&self,home:&Path)->Result<()> {
        let target=self.target(home);if self.owns(&target) {return self.read_back(home);}
        if std::fs::symlink_metadata(&target).is_ok() {return Err(Error::Stale);}
        let source=self.source(home);if !self.owns(&source) {return Err(Error::Stale);}
        let mut parent=home.to_path_buf();for part in ["shared","wiki",duduclaw_core::DEPARTMENTS_NAMESPACE] {parent.push(part);if !parent.exists(){create_private_dir(&parent)?;}plain_directory(&parent)?;}
        rename_new(&source,&target)?;sync(source.parent().ok_or(Error::Storage)?)?;sync(&parent)?;self.read_back(home)
    }
    pub(super) fn read_back(&self,home:&Path)->Result<()> {
        if !self.owns(&self.target(home)) || !crate::departments::list_departments(home,&[]).iter().any(|d|d.name==self.name) {return Err(Error::Apply);}Ok(())
    }
    pub(super) fn disable(&self,home:&Path)->Result<()> {
        let anchor=self.target(home);
        duduclaw_core::with_file_lock(&anchor,||Ok(self.disable_locked(home))).map_err(|_|Error::Compensation)?
    }
    fn disable_locked(&self,home:&Path)->Result<()> {
        let path=self.target(home);
        match std::fs::symlink_metadata(&path) {Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(()),Err(_)=>return Err(Error::Compensation),Ok(_)=>{}}
        if !self.owns(&path) {return Err(Error::Compensation);}
        // rmdir atomically refuses a nonempty directory, including a file added
        // after this ownership check. Never move or recursively delete content.
        std::fs::remove_dir(&path).map_err(|_|Error::Compensation)?;sync(path.parent().ok_or(Error::Storage)?)?;
        if path.exists() {return Err(Error::Compensation);}Ok(())
    }
}
fn sync(path:&Path)->Result<()> {std::fs::File::open(path).and_then(|f|f.sync_all()).map_err(|_|Error::Storage)}
fn rename_new(source:&Path,target:&Path)->Result<()> {
    #[cfg(target_os="linux")] {
        use std::os::unix::ffi::OsStrExt;
        let from=std::ffi::CString::new(source.as_os_str().as_bytes()).map_err(|_|Error::Invalid)?;
        let to=std::ffi::CString::new(target.as_os_str().as_bytes()).map_err(|_|Error::Invalid)?;
        // renameat2 is required by the native Linux platform; do not replace an
        // empty destination directory created by another normal operator.
        if unsafe {libc::syscall(libc::SYS_renameat2,libc::AT_FDCWD,from.as_ptr(),libc::AT_FDCWD,to.as_ptr(),libc::RENAME_NOREPLACE)}!=0 {return Err(Error::Apply);}Ok(())
    }
    #[cfg(not(target_os="linux"))] {let _=(source,target);Err(Error::Denied)}
}

// A directory inode can be reused after a normal delete/create. A protected
// operation attribute distinguishes that fresh directory without adding a fake
// wiki page or making the normal department appear nonempty.
#[cfg(target_os="linux")]
fn receipt(path:&Path)->Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    let path=std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut bytes=[0u8;256];
    let len=unsafe {libc::lgetxattr(path.as_ptr(),c"user.duduclaw_migration".as_ptr(),bytes.as_mut_ptr().cast(),bytes.len())};
    if len<0 || len as usize>bytes.len() {None}else{Some(bytes[..len as usize].to_vec())}
}
#[cfg(not(target_os="linux"))]
fn receipt(_: &Path)->Option<Vec<u8>> {None}
#[cfg(target_os="linux")]
fn stamp(path:&Path,value:&str)->Result<()> {
    use std::os::unix::ffi::OsStrExt;
    if let Some(old)=receipt(path) {return if old==value.as_bytes(){Ok(())}else{Err(Error::Stale)};}
    let path=std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_|Error::Invalid)?;
    if unsafe {libc::lsetxattr(path.as_ptr(),c"user.duduclaw_migration".as_ptr(),value.as_ptr().cast(),value.len(),libc::XATTR_CREATE)}!=0 {return Err(Error::Storage);}Ok(())
}
#[cfg(not(target_os="linux"))]
fn stamp(_: &Path,_:&str)->Result<()> {Err(Error::Denied)}

#[cfg(all(test,target_os="linux"))] mod tests {
    use super::*;
    fn item()->ConfirmationItem {ConfirmationItem {binding:ProposalBinding {operation_id:ProposalOperationId {proposal_id:duduclaw_migration::ids::RequestId([5;16]),revision:1},proposal_digest:Sha256Digest([4;32])},category:ConfirmationCategory::Department,status:ConfirmationStatus::Applying,high_risk:false,fields:vec![ProposalField {name:"name".into(),value:ProposalValue::Text {value:"sales".into()}}]}}
    #[test] fn migration_authority_department_refuses_independently_created_directory() {
        let home=tempfile::tempdir().unwrap();let mut a=GatewayAuthority::new(home.path().into()).unwrap();let p=item();a.stage(&p).unwrap();
        crate::departments::create_department(home.path(),"sales",&[]).unwrap();
        assert!(a.publish(&p).is_err());assert!(a.read_back(&p).is_err());assert!(a.disable_and_read_back(&p).is_err());
        assert!(crate::departments::wiki_departments_root(home.path()).join("sales").is_dir());
    }
    #[test] fn migration_authority_department_compensation_preserves_new_user_content() {
        let home=tempfile::tempdir().unwrap();let mut a=GatewayAuthority::new(home.path().into()).unwrap();let p=item();a.stage(&p).unwrap();a.publish(&p).unwrap();
        let content=crate::departments::wiki_departments_root(home.path()).join("sales/notes.md");std::fs::write(&content,"keep").unwrap();
        assert!(a.disable_and_read_back(&p).is_err());assert_eq!(std::fs::read_to_string(content).unwrap(),"keep");
    }
}
