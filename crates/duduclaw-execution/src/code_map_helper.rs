//! Bounded offline source indexing, executed only inside the employee namespace.
use serde_json::{Value, json};
pub const RUNTIME: &str = "duduclaw-code-map-v1";
pub const ARGUMENT: &str = "--code-map-fd-v1";
pub const MAX_REQUEST: usize = 64 * 1024;
pub const MAX_RESULT: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request { pub query: String, pub chat_files: Vec<String>, pub max_files: usize, pub path: Option<String> }
pub fn relative(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.bytes().any(|b| b == 0 || b == b'\\')
        && value.split('/').all(|part| !matches!(part, "" | "." | ".."))
        && !std::path::Path::new(value).is_absolute()
}
impl Request {
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_REQUEST { return Err("request too large".into()); }
        let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid request")?;
        let fields = value.as_object().ok_or("invalid request")?;
        if fields.keys().any(|key| !matches!(key.as_str(), "query" | "chat_files" | "max_files" | "path")) { return Err("unknown field".into()); }
        let query = match value.get("query") { None => String::new(), Some(v) => v.as_str().ok_or("invalid query")?.to_owned() };
        let chat_files = match value.get("chat_files") {
            None => vec![], Some(Value::Array(values)) if values.len() <= 100 => values.iter()
                .map(|value| value.as_str().filter(|v| relative(v)).map(str::to_owned).ok_or("invalid chat file"))
                .collect::<Result<Vec<_>, _>>()?, _ => return Err("invalid chat files".into()),
        };
        let max_files = match value.get("max_files") { None => 15, Some(v) => v.as_u64().filter(|v| (1..=100).contains(v)).ok_or("invalid max_files")? as usize };
        let path = match value.get("path") { None => None, Some(v) => Some(v.as_str().filter(|p| relative(p)).ok_or("invalid path")?.into()) };
        if query.len() > 8192 || (query.trim().is_empty() && chat_files.is_empty()) { return Err("missing query or files".into()); }
        Ok(Self { query, chat_files, max_files, path })
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut value = json!({"query":self.query,"chat_files":self.chat_files,"max_files":self.max_files});
        if let Some(path) = &self.path { value["path"] = json!(path); }
        let bytes = serde_json::to_vec(&value).map_err(|_| "invalid request")?;
        Self::decode(&bytes)?; Ok(bytes)
    }
}

/// Typed validation at the host boundary; never accept arbitrary helper JSON as
/// a successful MCP result. The source index is intentionally bounded/partial.
pub fn decode_result(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_RESULT { return Err("result too large".into()); }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid result")?;
    let object = value.as_object().ok_or("invalid result")?;
    if object.len() != 5 || object.keys().any(|key| !matches!(key.as_str(), "map" | "files" | "indexed_files" | "indexed_symbols" | "truncated"))
        || value["map"].as_str().is_none_or(|s| s.len() > 128 * 1024)
        || value["indexed_files"].as_u64().is_none_or(|n| n > 500)
        || value["indexed_symbols"].as_u64().is_none_or(|n| n > 10_000)
        || value["truncated"].as_bool().is_none() { return Err("invalid result".into()); }
    let files = value["files"].as_array().filter(|v| v.len() <= 100).ok_or("invalid files")?;
    for file in files {
        let fields = file.as_object().ok_or("invalid file")?;
        if fields.len() != 3 || file["path"].as_str().is_none_or(|v| !relative(v))
            || file["score"].as_u64().is_none()
            || file["symbols"].as_array().is_none_or(|symbols| symbols.len() > 20 || symbols.iter().any(|s| s.as_str().is_none_or(|s| s.len() > 256))) {
            return Err("invalid file".into());
        }
    }
    Ok(value)
}

#[cfg(target_os = "linux")]
fn build(request: Request) -> Result<Value, String> {
    use std::{fs::{File, OpenOptions}, io::Read, os::unix::{fs::{MetadataExt, OpenOptionsExt}, ffi::OsStrExt}};
    let root = std::path::PathBuf::from("/work");
    let start = request.path.as_ref().map(|p| root.join(p)).unwrap_or(root.clone());
    // Validate every selected path component before walking. B2/D8 excludes a
    // concurrent employee writer; per-entry nofollow opens still reject links.
    let mut checked = root.clone();
    if let Some(path) = &request.path {
        for part in path.split('/') {
            checked.push(part);
            let metadata = checked.symlink_metadata().map_err(|_| "directory unavailable")?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() { return Err("invalid directory".into()); }
        }
    }
    let terms: Vec<_> = request.query.split_whitespace().take(32).map(str::to_lowercase).collect();
    let mut stack = vec![(start, 0usize)];
    let (mut visited, mut indexed, mut total_bytes, mut symbols_count) = (0usize, 0usize, 0usize, 0usize);
    let mut files = Vec::new();
    let mut truncated = false;
    'walk: while let Some((dir, depth)) = stack.pop() {
        if depth > 32 { truncated = true; continue; }
        let entries = std::fs::read_dir(dir).map_err(|_| "directory unavailable")?;
        for entry in entries {
            visited += 1;
            if visited > 5000 || indexed >= 500 || total_bytes >= 8 * 1024 * 1024 { truncated = true; break 'walk; }
            let entry = entry.map_err(|_| "entry unavailable")?;
            let kind = entry.file_type().map_err(|_| "entry unavailable")?;
            if kind.is_symlink() { continue; }
            let name = entry.file_name();
            if name.as_bytes().first() == Some(&b'.') || matches!(name.to_str(), Some("node_modules" | "target" | "vendor")) { continue; }
            if kind.is_dir() { stack.push((entry.path(), depth + 1)); continue; }
            if !kind.is_file() || !matches!(entry.path().extension().and_then(|v| v.to_str()),
                Some("rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "c" | "h" | "cpp")) { continue; }
            let file: File = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(entry.path()).map_err(|_| "file unavailable")?;
            let metadata = file.metadata().map_err(|_| "file unavailable")?;
            if !metadata.is_file() || metadata.nlink() != 1 { continue; }
            if metadata.len() > 128 * 1024 { truncated = true; continue; }
            let mut bytes = Vec::new();
            file.take(128 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| "file unavailable")?;
            if bytes.len() > 128 * 1024 { truncated = true; continue; }
            total_bytes += bytes.len();
            let Ok(text) = std::str::from_utf8(&bytes) else { continue; };
            let path = entry.path();
            let Some(relative) = path.strip_prefix(&root).ok().and_then(|v| v.to_str()).filter(|p| relative(p)) else { continue; };
            indexed += 1;
            let mut symbols = Vec::new();
            for line in text.lines() {
                let line = line.trim_start().strip_prefix("pub ").unwrap_or(line.trim_start());
                if ["fn ", "async fn ", "struct ", "enum ", "trait ", "def ", "class ", "function ", "export function ", "func "]
                    .iter().any(|prefix| line.strip_prefix(prefix).is_some()) && symbols.len() < 20 {
                    symbols.push(line.chars().take(64).collect::<String>());
                }
            }
            symbols_count += symbols.len();
            let haystack = format!("{} {}", relative, symbols.join(" ")).to_lowercase();
            let mut score = terms.iter().filter(|term| haystack.contains(term.as_str())).count() as u64;
            if request.chat_files.iter().any(|path| path == relative) { score += 100; }
            files.push(json!({"path":relative, "score":score, "symbols":symbols}));
        }
    }
    files.sort_by(|a,b| b["score"].as_u64().cmp(&a["score"].as_u64()).then_with(|| a["path"].as_str().cmp(&b["path"].as_str())));
    files.truncate(request.max_files);
    let mut map = files.iter().map(|file| format!("{}\n{}", file["path"].as_str().unwrap_or(""),
        file["symbols"].as_array().unwrap().iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"))).collect::<Vec<_>>().join("\n\n");
    if map.len() > 128 * 1024 {
        let end = map.char_indices().map(|(index, _)| index).take_while(|index| *index <= 128 * 1024).last().unwrap_or(0);
        map.truncate(end); truncated = true;
    }
    let value = json!({"map":map,"files":files,"indexed_files":indexed,"indexed_symbols":symbols_count,"truncated":truncated});
    let encoded = serde_json::to_vec(&value).map_err(|_| "result unavailable")?;
    decode_result(&encoded)
}

pub fn main_entry() -> i32 {
    #[cfg(target_os = "linux")]
    {
        use std::io::Write;
        let Ok(bytes) = crate::program_helper::helper_input(ARGUMENT, MAX_REQUEST) else { return 77; };
        let Ok(request) = Request::decode(&bytes) else { return 64; };
        let Ok(value) = build(request) else { return 74; };
        let Ok(bytes) = serde_json::to_vec(&value) else { return 70; };
        if std::io::stdout().lock().write_all(&bytes).is_err() { return 74; }
        0
    }
    #[cfg(not(target_os = "linux"))]
    { 64 }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn code_map_protocol_refuses_paths_and_authority_fields() {
        let input = json!({"query":"中文", "max_files":15});
        let request = Request::decode(&serde_json::to_vec(&input).unwrap()).unwrap();
        assert_eq!(Request::decode(&request.encode().unwrap()).unwrap(), request);
        for path in ["/etc", "../secret", "a/../b", "a//b", "a\\b", "a/./b"] {
            let mut input = input.clone(); input["path"] = json!(path);
            assert!(Request::decode(&serde_json::to_vec(&input).unwrap()).is_err());
        }
        for key in ["cwd", "root", "argv", "env"] {
            let mut input = input.clone(); input[key] = json!("forged");
            assert!(Request::decode(&serde_json::to_vec(&input).unwrap()).is_err());
        }
        assert!(decode_result(br#"{"map":"ok","files":[],"indexed_files":501,"indexed_symbols":0,"truncated":false}"#).is_err());
    }
}
