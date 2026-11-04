//! Gateway admission snapshots are copied from held safe descriptors.
use super::*;
use sha2::{Digest, Sha256};

pub fn snapshot_attachment(agent_dir: &Path, source: &Path, destination: &Path, max_bytes: u64) -> Result<(String, u64), String> {
    let source = validate_attachment(agent_dir, source, max_bytes)?;
    let mut input = open_read(&source)?;
    check_file(&input, max_bytes)?;
    no_links(destination.parent().ok_or("會議暫存路徑無效。")?)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let mut output = options.open(destination).map_err(|_| "無法建立會議快照。")?;
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer).map_err(|_| "無法讀取會議錄音。")?;
        if read == 0 { break; }
        size = size.checked_add(read as u64).ok_or("會議錄音過大。")?;
        if size > max_bytes { return Err("會議錄音超過核准大小。".into()); }
        digest.update(&buffer[..read]);
        output.write_all(&buffer[..read]).map_err(|_| "無法儲存會議快照。")?;
    }
    if size == 0 { return Err("會議錄音不可為空。".into()); }
    output.sync_all().map_err(|_| "無法同步會議快照。")?;
    let mut permissions = output.metadata().map_err(|_| "無法讀取會議快照。")?.permissions();
    permissions.set_readonly(true);
    output.set_permissions(permissions).map_err(|_| "無法保護會議快照。")?;
    Ok((hex::encode(digest.finalize()), size))
}

pub fn verify_snapshot(path: &Path, expected_digest: &str, expected_size: u64) -> Result<(), String> {
    if expected_digest.len() != 64 || expected_size == 0 { return Err("會議快照缺少驗證資料，請重新提交。".into()); }
    let mut input = open_read(path)?;
    check_file(&input, expected_size)?;
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer).map_err(|_| "無法驗證會議快照。")?;
        if read == 0 { break; }
        size = size.checked_add(read as u64).ok_or("會議快照大小超限。")?;
        if size > expected_size { return Err("會議快照已變更，請重新提交。".into()); }
        digest.update(&buffer[..read]);
    }
    if size != expected_size || hex::encode(digest.finalize()) != expected_digest {
        return Err("會議快照已變更，請重新提交。".into());
    }
    Ok(())
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn transcription_admission_snapshot_survives_same_size_source_replacement() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        let attachments = home.join("attachments");
        std::fs::create_dir(&attachments).unwrap();
        let source = attachments.join("audio.wav");
        let snapshot = home.join("snapshot.wav");
        std::fs::write(&source, b"approved").unwrap();
        let (digest,size) = snapshot_attachment(&home,&source,&snapshot,100).unwrap();
        std::fs::remove_file(&source).unwrap();
        std::fs::write(&source,b"replaced").unwrap();
        verify_snapshot(&snapshot,&digest,size).unwrap();
        assert_eq!(std::fs::read(&snapshot).unwrap(), b"approved");
        assert!(verify_snapshot(&source,&digest,size).is_err());
    }
}
