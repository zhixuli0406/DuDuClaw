//! Fixed approved Office script mapping; no uv, package installation or PATH fallback.
use serde_json::{Value, json};
pub const ARGUMENT: &str = "--office-fd-v1";
pub const MAX_REQUEST: usize = 64 * 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request { pub args: Vec<String> }
pub fn script_id(skill: &str, script: &str) -> Result<String, String> {
    if !matches!(skill, "docx" | "xlsx" | "pptx" | "pdf") || !matches!(script, "create" | "extract") || script.len() > 64
        || !script.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')) { return Err("invalid Office mapping".into()); }
    Ok(format!("duduclaw-office-{skill}-{script}-v1"))
}
fn argument(value: &str) -> bool {
    // Paths are always relative to the authorized /work mount. Protect agent
    // structure names too, even though the protected workspace is separate.
    let value = value.split_once('=').map(|(_, v)| v).unwrap_or(value);
    !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
        && !value.contains('\\') && !std::path::Path::new(value).is_absolute()
        && value.split('/').all(|part| !matches!(part, "" | "." | ".."))
        && !matches!(value.rsplit('/').next().unwrap_or("").to_ascii_lowercase().as_str(),
            "soul.md" | "agent.toml" | "contract.toml" | ".mcp.json")
}
impl Request {
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_REQUEST { return Err("Office request too large".into()); }
        let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid Office request")?;
        if value.as_object().is_none_or(|object| object.len() != 1 || !object.contains_key("args")) { return Err("unknown Office field".into()); }
        let values = value["args"].as_array().filter(|v| v.len() <= 128).ok_or("invalid Office args")?;
        let args = values.iter().map(|v| v.as_str().filter(|v| argument(v)).map(str::to_owned).ok_or("invalid Office argument"))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { args })
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let bytes = serde_json::to_vec(&json!({"args":self.args})).map_err(|_| "invalid Office request")?;
        Self::decode(&bytes)?; Ok(bytes)
    }
}
/// Exact schemas for the bundled scripts. Unpublished/custom script stems and
/// arbitrary flags cannot hide additional input/output paths in data values.
pub fn paths(request: &Request, skill: &str, script: &str) -> Result<(String, Option<String>, Option<String>), String> {
    if !matches!(script, "create" | "extract") { return Err("unapproved script".into()); }
    let mut input = None;
    let mut output = None;
    let mut format = None;
    let mut values = request.args.iter();
    while let Some(value) = values.next() {
        match value.as_str() {
            "--out" if script == "create" && output.is_none() => { output = Some(values.next().ok_or("missing output")?.clone()); }
            "--outdir" if script == "to_pdf" && output.is_none() => { output = Some(values.next().ok_or("missing output")?.clone()); }
            "--format" if script != "to_pdf" && format.is_none() => { format = Some(values.next().ok_or("missing format")?.clone()); }
            _ if !value.starts_with('-') && input.is_none() => { input = Some(value.clone()); }
            _ => return Err("unsupported Office arguments".into()),
        }
    }
    let input = input.ok_or("missing input")?;
    if !crate::code_map_helper::relative(&input) || output.as_ref().is_some_and(|v| !crate::code_map_helper::relative(v))
        || (script == "create" && output.is_none()) { return Err("invalid Office paths".into()); }
    if let Some(format) = &format {
        let valid = if script == "extract" { matches!(format.as_str(), "json" | "md") }
            else { match skill { "xlsx" => matches!(format.as_str(), "json" | "csv"), "pdf" => matches!(format.as_str(), "md" | "text"),
                _ => matches!(format.as_str(), "md" | "json") } };
        if !valid { return Err("invalid Office format".into()); }
    }
    Ok((input, output, format))
}
pub fn main_entry() -> i32 {
    #[cfg(target_os = "linux")]
    {
        use std::{io::{Read, Write}, os::unix::fs::{MetadataExt, OpenOptionsExt}};
        use sha2::{Digest, Sha256};
        let args: Vec<_> = std::env::args().skip(1).collect();
        if args.len() != 5 || args[0] != ARGUMENT || script_id(&args[1], &args[2]).is_err()
            || !crate::program_helper::valid_digest(&args[3]) || !crate::program_helper::valid_digest(&args[4]) { return 64; }
        let fixed: Vec<_> = args.iter().map(String::as_str).collect();
        let Ok(bytes) = crate::program_helper::helper_input_args(&fixed, MAX_REQUEST) else { return 77; };
        let Ok(request) = Request::decode(&bytes) else { return 64; };
        // HX5: signed files under `/helpers` and staging under
        // `/helper-tmp/<call>` in-run; `/runtime` + `/tmp/office` legacy.
        let Ok(layout) = crate::helper_layout::current() else { return 77; };
        let (Ok(python), Ok(path)) = (layout.file("python3"), layout.file(&format!("office-{}-{}.py", args[1], args[2])))
            else { return 77; };
        if crate::program_helper::verify_runtime_file(&python, &args[4]).is_err() { return 77; }
        let Ok(file) = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(&path) else { return 77; };
        let Ok(metadata) = file.metadata() else { return 77; };
        if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 || metadata.len() > 1024 * 1024 { return 77; }
        let mut script = Vec::new();
        if file.take(1024 * 1024 + 1).read_to_end(&mut script).is_err() || script.len() > 1024 * 1024 { return 77; }
        let digest: String = Sha256::digest(&script).iter().map(|byte| format!("{byte:02x}")).collect();
        if digest != args[3] { return 77; }
        // The script is on B2's immutable root-owned runtime mount; the signed
        // argv pins its digest. Dependencies must ship there, never auto-install.
        let staging = layout.staging("office");
        let Ok(prepared) = crate::office_io::Prepared::prepare(&request, &args[1], &args[2], std::path::Path::new("/work"), &staging) else { return 77; };
        let mut arguments = vec!["-I", path.as_str()];
        arguments.extend(prepared.arguments.iter().map(String::as_str));
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return 70; };
        let mut result = runtime.block_on(crate::program_helper::execute_command(&python, &arguments, &[], 180,
            std::path::Path::new("/work"), &layout));
        if result.status == crate::program_helper::ProgramStatus::Exited && result.exit_code == Some(0) {
            match prepared.publish() {
                Ok(Some(name)) => result.stdout = format!("Office output published: {name}"),
                Ok(None) => {},
                Err(_) => { result.status = crate::program_helper::ProgramStatus::IoFailed; result.exit_code = None; result.signal = None;
                    result.stdout.clear(); result.stderr = "Office output publication failed".into(); }
            }
        }
        let Ok(bytes) = result.encode() else { return 70; };
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
    fn office_approved_schemas_reject_extra_paths_flags_and_converters() {
        let request = |args: &[&str]| Request { args: args.iter().map(|v| (*v).into()).collect() };
        assert!(paths(&request(&["input.md", "--out", "out.docx", "--format", "md"]), "docx", "create").is_ok());
        assert!(paths(&request(&["input.xlsx", "--format", "json"]), "xlsx", "extract").is_ok());
        for args in [vec!["input.md", "--out", "out.docx", "extra"],
            vec!["input.md", "--out=out.docx"], vec!["input.md", "--out", "out.docx", "--format", "shell"],
            vec!["input.md", "--out", "a.docx", "--out", "b.docx"]] {
            assert!(paths(&request(&args), "docx", "create").is_err());
        }
        assert!(script_id("docx", "to_pdf").is_err());
    }

    #[test]
    fn office_protocol_has_exact_signed_mapping_and_workspace_arguments() {
        assert_eq!(script_id("pdf", "extract").unwrap(), "duduclaw-office-pdf-extract-v1");
        for (skill, script) in [("shell", "create"), ("docx", "../create"), ("pdf", "create.py")] { assert!(script_id(skill, script).is_err()); }
        for arg in ["/etc/passwd", "../x", "--out=/etc/x", "SOUL.md", "a/../x", "--out=agent.toml"] {
            assert!(Request { args: vec![arg.into()] }.encode().is_err(), "{arg}");
        }
        let request = Request { args: vec!["input.docx".into(), "--out".into(), "results/中文.pdf".into()] };
        assert_eq!(Request::decode(&request.encode().unwrap()).unwrap(), request);
        assert!(Request::decode(br#"{"args":[],"script":"forged"}"#).is_err());
    }
}
