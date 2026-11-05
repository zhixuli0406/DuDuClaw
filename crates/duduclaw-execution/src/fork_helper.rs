//! Bounded employee-side fork snapshots. Only fixed workspace-relative roots.
use serde_json::{Value, json};
pub const RUNTIME: &str = "duduclaw-fork-snapshot-v1";
pub const ARGUMENT: &str = "--fork-snapshot-fd-v1";
pub const MAX_REQUEST: usize = 4096;
pub const MAX_RESULT: usize = 256 * 1024;
pub const MAX_FILES: usize = 1000;
pub const MAX_BYTES: u64 = 16 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation { CloneParent, SnapshotBranch }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request { pub id: String, pub operation: Operation }
impl Request {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if !crate::recording_helper::valid_id(&self.id) { return Err("invalid fork id".into()); }
        serde_json::to_vec(&json!({"id":self.id,"operation": match self.operation {
            Operation::CloneParent => "clone_parent", Operation::SnapshotBranch => "snapshot_branch" }})).map_err(|_| "encoding".into())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_REQUEST { return Err("oversize fork request".into()); }
        let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid fork request")?;
        let fields = value.as_object().ok_or("invalid fork fields")?;
        if fields.len() != 2 || fields.keys().any(|k| !matches!(k.as_str(), "id" | "operation")) { return Err("invalid fork fields".into()); }
        let id = value["id"].as_str().ok_or("invalid fork id")?.to_owned();
        let operation = match value["operation"].as_str() { Some("clone_parent") => Operation::CloneParent,
            Some("snapshot_branch") => Operation::SnapshotBranch, _ => return Err("invalid operation".into()) };
        let request = Self { id, operation }; request.encode()?; Ok(request)
    }
}
pub fn decode_result(bytes: &[u8]) -> Result<Value, String> {
    use sha2::{Digest, Sha256};
    if bytes.len() > MAX_RESULT { return Err("oversize fork result".into()); }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid fork result")?;
    let fields = value.as_object().ok_or("invalid fork result")?;
    if fields.len() != 4 || fields.keys().any(|k| !matches!(k.as_str(), "id" | "workspace" | "files" | "digest"))
        || !value["id"].as_str().is_some_and(crate::recording_helper::valid_id) { return Err("invalid fork result fields".into()); }
    let expected = format!("fork-{}", value["id"].as_str().unwrap());
    if value["workspace"] != expected { return Err("fork workspace mismatch".into()); }
    let files = value["files"].as_object().ok_or("invalid files")?;
    if files.len() > MAX_FILES { return Err("too many files".into()); }
    for (name, digest) in files {
        if name.is_empty() || name.len() > 4096 || name.bytes().any(|b| b == 0 || b == b'\\')
            || name.split('/').any(|part| part.is_empty() || part == "." || part == "..")
            || !digest.as_str().is_some_and(crate::program_helper::valid_digest) { return Err("invalid manifest".into()); }
    }
    let canonical = serde_json::to_vec(&json!({"files":files})).map_err(|_| "encoding")?;
    let digest = format!("{:x}", Sha256::digest(canonical));
    if value["digest"] != digest { return Err("digest mismatch".into()); }
    Ok(value)
}
#[cfg(target_os = "linux")]
pub fn main_entry() -> i32 {
    use std::io::Write;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice() != [ARGUMENT] { return 64; }
    let Ok(bytes) = crate::program_helper::helper_input(ARGUMENT, MAX_REQUEST) else { return 77; };
    let Ok(request) = Request::decode(&bytes) else { return 64; };
    let Ok(result) = crate::fork_io::snapshot(std::path::Path::new("/work"), &request) else { return 74; };
    let Ok(bytes) = serde_json::to_vec(&result) else { return 74; };
    if decode_result(&bytes).is_err() { return 74; }
    if std::io::stdout().write_all(&bytes).and_then(|_| std::io::stdout().flush()).is_err() { return 74; }
    0
}
#[cfg(not(target_os = "linux"))]
pub fn main_entry() -> i32 { 77 }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fork_protocol_rejects_paths_and_forged_digest() {
        let request = Request { id:"ab".repeat(16), operation:Operation::CloneParent };
        assert_eq!(Request::decode(&request.encode().unwrap()).unwrap(), request);
        assert!(Request::decode(br#"{"id":"../../etc","operation":"clone_parent"}"#).is_err());
        assert!(Request::decode(br#"{"id":"abababababababababababababababab","operation":"clone_parent","root":"/"}"#).is_err());
        assert!(decode_result(br#"{"id":"abababababababababababababababab","workspace":"fork-abababababababababababababababab","files":{},"digest":"fake"}"#).is_err());
    }
}
