//! RM-141: only verified employee-local files may become Odoo attachments.
use super::{mcp_error, tool_text, OdooState};
use super::odoo_access::{ToolPolicy, error_text, identity_suffix};
use super::odoo_identity_client::{self, OdooIdentity};
use duduclaw_odoo::OdooConnector;
use serde_json::{json, Value};
use std::{fs::File, io::{Read, Cursor}, path::{Path, Component}, sync::Arc};

const TOOL: &str = "odoo_attachment_add";
const FILE_REFUSED: &str = "附件必須是本員工 attachments 資料夾內的一般檔案，不能使用連結或越界路徑。";
type AttachmentResult<T> = std::result::Result<T, String>;

// Use the verified process/isolated-call identity, never a tool argument or
// ToolPolicy.agent_id (which may be an ephemeral employee's parent).
pub(super) fn verified_employee(home: &Path, agent: &str) -> bool {
    !agent.is_empty() && agent.trim() == agent && agent != "." && agent != ".."
        && !agent.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
        && super::caller_token_verified(home, agent)
}

struct Attachment {
    model: String,
    res_id: i64,
    file: String,
    name: String,
}

fn relative_parts(file: &str) -> AttachmentResult<Vec<&str>> {
    // Also reject Windows separators/ADS on Unix, so a request means the same
    // thing on every host. Empty and dot components are deliberately refused.
    if file.is_empty() || file.chars().any(|c| c.is_control() || matches!(c, '\\' | ':'))
        || Path::new(file).is_absolute()
        || Path::new(file).components().any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(FILE_REFUSED.into());
    }
    let parts: Vec<_> = file.split('/').collect();
    if parts.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return Err(FILE_REFUSED.into());
    }
    Ok(parts)
}

fn parse(params: &Value) -> AttachmentResult<Attachment> {
    let object = params.as_object().ok_or("Arguments must be an object")?;
    if object.keys().any(|k| !["model", "res_id", "file", "name"].contains(&k.as_str())) {
        return Err("Unexpected Odoo attachment argument".into());
    }
    let model = params.get("model").and_then(Value::as_str).ok_or("model is required")?;
    if !duduclaw_odoo::schema::valid_model_name(model) {
        return Err("Invalid Odoo model name".into());
    }
    let res_id = params.get("res_id").and_then(Value::as_i64).filter(|n| *n > 0)
        .ok_or("res_id must be a positive integer")?;
    let file = params.get("file").and_then(Value::as_str).ok_or("file is required")?;
    let parts = relative_parts(file)?;
    let name = match params.get("name") {
        None => *parts.last().ok_or(FILE_REFUSED)?,
        Some(value) => value.as_str().ok_or("name must be plain text")?,
    };
    if name.is_empty() || name.chars().take(256).count() > 255
        || name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\'))
    {
        return Err("name must be 1–255 plain-text characters without separators or control characters".into());
    }
    Ok(Attachment { model: model.into(), res_id, file: file.into(), name: name.into() })
}

/// Keep directory handles alive until the bounded read has finished. On
/// Windows these handles also deny delete sharing, pinning every path prefix.
struct OpenedAttachment {
    file: File,
    _directories: Vec<File>,
}

#[cfg(unix)]
fn open_file(home: &Path, agent: &str, relative: &str) -> AttachmentResult<OpenedAttachment> {
    use std::os::{fd::{AsRawFd, FromRawFd}, unix::fs::OpenOptionsExt};
    let parts = relative_parts(relative)?;
    // Home is host-owned and may use a platform alias such as macOS /var.
    // Everything below that trusted anchor is opened one component at a time.
    let home = home.canonicalize().map_err(|_| FILE_REFUSED)?;
    let root = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(home).map_err(|_| FILE_REFUSED)?;
    let mut directories = vec![root];
    let all: Vec<&str> = ["agents", agent, "attachments"].into_iter().chain(parts).collect();
    for (index, part) in all.iter().enumerate() {
        let last = index + 1 == all.len();
        let component = std::ffi::CString::new(*part).map_err(|_| FILE_REFUSED)?;
        let parent = directories.last().ok_or(FILE_REFUSED)?.as_raw_fd();
        let mut before = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent is live; component is a single NUL-terminated name;
        // fstatat initializes before only on success, checked below.
        if unsafe { libc::fstatat(parent, component.as_ptr(), before.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            return Err(FILE_REFUSED.into());
        }
        let before = unsafe { before.assume_init() };
        let expected = if last { libc::S_IFREG } else { libc::S_IFDIR };
        if before.st_mode & libc::S_IFMT != expected { return Err(FILE_REFUSED.into()); }
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK
            | if last { 0 } else { libc::O_DIRECTORY };
        // SAFETY: same live parent/name; ownership of the new fd is transferred
        // immediately to File, including all subsequent failure paths.
        let fd = unsafe { libc::openat(parent, component.as_ptr(), flags) };
        if fd < 0 { return Err(FILE_REFUSED.into()); }
        let opened = unsafe { File::from_raw_fd(fd) };
        let mut after = std::mem::MaybeUninit::<libc::stat>::uninit();
        // Validate the opened object, not a second path lookup. NONBLOCK keeps
        // a FIFO swapped in between validation and open from hanging the MCP.
        if unsafe { libc::fstat(opened.as_raw_fd(), after.as_mut_ptr()) } != 0 {
            return Err(FILE_REFUSED.into());
        }
        let after = unsafe { after.assume_init() };
        if after.st_mode & libc::S_IFMT != expected
            || before.st_dev != after.st_dev || before.st_ino != after.st_ino
        {
            return Err(FILE_REFUSED.into());
        }
        if last { return Ok(OpenedAttachment { file: opened, _directories: directories }); }
        directories.push(opened);
    }
    Err(FILE_REFUSED.into())
}

#[cfg(windows)]
fn open_file(home: &Path, agent: &str, relative: &str) -> AttachmentResult<OpenedAttachment> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_ATTRIBUTE_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE};
    let parts = relative_parts(relative)?;
    let mut path = home.canonicalize().map_err(|_| FILE_REFUSED)?;
    let all: Vec<&str> = ["agents", agent, "attachments"].into_iter().chain(parts).collect();
    let mut directories = Vec::new();
    // Opening the anchor too prevents replacement of home during the walk.
    for index in 0..=all.len() {
        if index > 0 { path.push(all[index - 1]); }
        let last = index == all.len();
        let opened = std::fs::OpenOptions::new().read(true)
            .share_mode(if last { FILE_SHARE_READ } else { FILE_SHARE_READ | FILE_SHARE_WRITE })
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(&path).map_err(|_| FILE_REFUSED)?;
        // GetFileInformationByHandle through File::metadata validates exactly
        // the handle we will read. Never follow reparse points, including
        // junctions. Ancestors cannot be renamed while their handles are held.
        let metadata = opened.metadata().map_err(|_| FILE_REFUSED)?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || if last { !metadata.is_file() } else { !metadata.is_dir() }
        {
            return Err(FILE_REFUSED.into());
        }
        if last { return Ok(OpenedAttachment { file: opened, _directories: directories }); }
        directories.push(opened);
    }
    Err(FILE_REFUSED.into())
}

#[cfg(not(any(unix, windows)))]
fn open_file(_: &Path, _: &str, _: &str) -> AttachmentResult<OpenedAttachment> {
    Err("Safe attachment file access is unavailable on this platform".into())
}

const IMAGE_MAX_DIMENSION: u32 = 12_000;
const IMAGE_MAX_ALLOC: u64 = 256 * 1024 * 1024;
const SIZE_REFUSED: &str = "附件超過 attachment_max_mb 大小限制。";

// Bound encoded output while writing, rather than allocating an oversized file
// and checking only after the encoder has finished.
struct ImageOutput {
    bytes: Vec<u8>,
    limit: u64,
}
impl std::io::Write for ImageOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.bytes.len() as u64) {
            return Err(std::io::Error::other(SIZE_REFUSED));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

fn reencode_image(bytes: &[u8], expected: image::ImageFormat, limit: u64)
    -> AttachmentResult<Vec<u8>> {
    let invalid = "圖片無法完整解碼或重新編碼，或超過圖片尺寸、記憶體或附件大小限制。";
    if bytes.len() as u64 > limit { return Err(SIZE_REFUSED.into()); }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format().map_err(|_| invalid)?;
    if reader.format() != Some(expected) {
        return Err("圖片內容與副檔名不一致。".into());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(IMAGE_MAX_DIMENSION);
    limits.max_image_height = Some(IMAGE_MAX_DIMENSION);
    limits.max_alloc = Some(IMAGE_MAX_ALLOC);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| invalid)?;
    let mut output = ImageOutput { bytes: Vec::new(), limit };
    // Fresh encoders receive pixels only: no original metadata or trailing bytes.
    // WebP is deliberately normalized to PNG (including its name and MIME type).
    match expected {
        image::ImageFormat::Jpeg => decoded.write_with_encoder(
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 90)),
        image::ImageFormat::Png | image::ImageFormat::WebP => decoded.write_with_encoder(
            image::codecs::png::PngEncoder::new(&mut output)),
        _ => return Err(invalid.into()),
    }.map_err(|_| invalid)?;
    if output.bytes.len() as u64 > limit { return Err(SIZE_REFUSED.into()); }
    Ok(output.bytes)
}

fn forbidden_pdf_name(name: &[u8]) -> bool {
    // lopdf has already decoded PDF #xx name escapes. Match whole names, also
    // rejecting nonstandard case variants conservatively.
    [b"JavaScript".as_slice(), b"JS", b"Launch", b"EmbeddedFile", b"EmbeddedFiles",
        b"OpenAction", b"AA", b"RichMedia", b"XFA", b"SubmitForm", b"ImportData",
        b"GoToR", b"GoToE", b"Sound", b"Movie"]
        .iter().any(|blocked| name.eq_ignore_ascii_case(blocked))
}

fn pdf_names_valid(object: &lopdf::Object, depth: usize) -> bool {
    if depth > 128 { return false; }
    let dict = match object {
        lopdf::Object::Name(name) => return !forbidden_pdf_name(name),
        lopdf::Object::Array(values) => return values.iter().all(|v| pdf_names_valid(v, depth + 1)),
        lopdf::Object::Dictionary(dict) => dict,
        lopdf::Object::Stream(stream) => &stream.dict,
        _ => return true,
    };
    dict.iter().all(|(key, value)| !forbidden_pdf_name(key) && pdf_names_valid(value, depth + 1))
}

fn pdf_load_filter(id: lopdf::ObjectId, object: &mut lopdf::Object)
    -> Option<(lopdf::ObjectId, lopdf::Object)> {
    // Inspect original stream dictionaries before lopdf removes Filter and
    // DecodeParms during object-stream expansion. Rejected entries leave the
    // cross-reference map incomplete, which pdf_valid refuses below.
    pdf_names_valid(object, 0).then(|| (id, object.clone()))
}

fn pdf_dictionary_valid(dict: &lopdf::Dictionary, doc: &lopdf::Document, depth: usize) -> bool {
    depth <= 128 && dict.iter().all(|(key, value)|
        !forbidden_pdf_name(key) && pdf_object_valid(value, doc, depth + 1))
}

fn pdf_object_valid(object: &lopdf::Object, doc: &lopdf::Document, depth: usize) -> bool {
    if depth > 128 { return false; }
    match object {
        lopdf::Object::Name(name) => !forbidden_pdf_name(name),
        lopdf::Object::Array(values) => values.iter().all(|v| pdf_object_valid(v, doc, depth + 1)),
        lopdf::Object::Dictionary(dict) => pdf_dictionary_valid(dict, doc, depth + 1),
        lopdf::Object::Stream(stream) => {
            if !pdf_dictionary_valid(&stream.dict, doc, depth + 1) { return false; }
            if !stream.dict.has_type(b"ObjStm") { return true; }
            // The loader expands object streams, but can silently drop malformed
            // entries. Reparse each container and require all declared entries;
            // inspect even objects not selected by the final cross-reference map.
            let mut stream = stream.clone();
            if stream.dict.has(b"Filter") && stream.decompress().is_err() { return false; }
            let Ok(count) = stream.dict.get(b"N").and_then(lopdf::Object::as_i64) else { return false };
            let Ok(first) = stream.dict.get(b"First").and_then(lopdf::Object::as_i64) else { return false };
            let Ok(first) = usize::try_from(first) else { return false };
            let Some(header) = stream.content.get(..first) else { return false };
            let Ok(header) = std::str::from_utf8(header) else { return false };
            let numbers: Result<Vec<u32>, _> = header.split_ascii_whitespace().map(str::parse).collect();
            let Ok(numbers) = numbers else { return false };
            let Ok(count) = usize::try_from(count) else { return false };
            if count.checked_mul(2) != Some(numbers.len()) { return false; }
            let Ok(objects) = lopdf::ObjectStream::new(&mut stream) else { return false };
            objects.objects.len() == count
                && objects.objects.values().all(|v| pdf_object_valid(v, doc, depth + 1))
        }
        // All indirect objects are visited independently below; do not recurse
        // through references (valid page trees contain cycles).
        lopdf::Object::Reference(id) => doc.objects.contains_key(id),
        _ => true,
    }
}

fn pdf_valid(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"%PDF-") { return false; }
    let tail = &bytes[bytes.len().saturating_sub(1024)..];
    if !tail.windows(5).rposition(|w| w == b"%%EOF")
        .is_some_and(|end| tail[end + 5..].iter().all(u8::is_ascii_whitespace)) { return false; }
    let Ok(doc) = lopdf::Document::load_mem_with_options(bytes,
        lopdf::LoadOptions { strict: true, filter: Some(pdf_load_filter), ..Default::default() }) else { return false };
    // Empty-password encryption can be transparently decrypted on load.
    if doc.trailer.has(b"Encrypt") || doc.is_encrypted() || doc.was_encrypted() { return false; }
    // lopdf's loader may skip an object on a parse error. Never treat an
    // incomplete loaded object map as a successfully screened document.
    for (&id, entry) in &doc.reference_table.entries {
        let generation = match entry {
            lopdf::xref::XrefEntry::Normal { generation, .. } => *generation,
            lopdf::xref::XrefEntry::Compressed { container, .. } => {
                if !doc.objects.get(&(*container, 0)).is_some_and(|v|
                    v.as_stream().is_ok_and(|s| s.dict.has_type(b"ObjStm"))) { return false; }
                0
            }
            lopdf::xref::XrefEntry::Free | lopdf::xref::XrefEntry::UnusableFree => continue,
        };
        if !doc.objects.contains_key(&(id, generation)) { return false; }
    }
    doc.catalog().is_ok()
        && pdf_dictionary_valid(&doc.trailer, &doc, 0)
        && doc.objects.values().all(|object| pdf_object_valid(object, &doc, 0))
}

fn display_name_matches(name: &str, mime: &str) -> bool {
    let extension = Path::new(name).extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    match mime {
        "application/pdf" => extension == "pdf",
        "image/png" => extension == "png",
        "image/jpeg" => matches!(extension.as_str(), "jpg" | "jpeg"),
        "image/webp" => extension == "webp",
        "text/plain" => extension == "txt",
        "text/csv" => extension == "csv",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => extension == "xlsx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => extension == "docx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => extension == "pptx",
        _ => false,
    }
}

fn mimetype(file: &str, bytes: &[u8]) -> AttachmentResult<&'static str> {
    let extension = Path::new(file).extension().and_then(|s| s.to_str())
        .unwrap_or("").to_ascii_lowercase();
    let kind = match extension.as_str() {
        "pdf" => {
            if !pdf_valid(bytes) {
                return Err("PDF 無法安全解析、已加密或含有不允許的主動內容，無法上傳。".into());
            }
            "application/pdf"
        }
        "txt" | "csv" if !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok() => {
            if extension == "txt" { "text/plain" } else { "text/csv" }
        }
        "xlsx" | "docx" | "pptx" if bytes.starts_with(b"PK\x03\x04") => {
            ooxml_type(&extension, bytes)?
        }
        _ => return Err("附件副檔名或檔頭不受支援，或兩者不一致。".into()),
    };
    Ok(kind)
}

fn ooxml_type(extension: &str, bytes: &[u8]) -> AttachmentResult<&'static str> {
    let invalid = "附件不是符合副檔名的 OOXML 文件。";
    let (part, mime) = match extension {
        "xlsx" => ("xl/workbook.xml", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        "docx" => ("word/document.xml", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "pptx" => ("ppt/presentation.xml", "application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        _ => return Err(invalid.into()),
    };
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| invalid)?;
    // Metadata and one bounded XML read only: never expand the whole archive.
    if archive.len() > 4096 || archive.file_names().any(|name| {
        let lower = name.to_ascii_lowercase();
        lower.ends_with("vbaproject.bin") || lower.split('/').any(|component| component == "activex")
    }) { return Err(invalid.into()); }
    // Bound both each relationship part and total expanded relationship bytes.
    // Reject entities/DTDs and non-UTF-8 rather than guessing XML encodings.
    let mode = regex::Regex::new(r"(?i)\bTargetMode\b").map_err(|_| invalid)?;
    let internal = regex::Regex::new(r#"(?i)^\s*=\s*(?:"Internal"|'Internal')"#).map_err(|_| invalid)?;
    let mut remaining = 4 * 1024 * 1024usize;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|_| invalid)?;
        if !entry.name().to_ascii_lowercase().ends_with(".rels") { continue; }
        let mut xml = Vec::new();
        entry.take(256 * 1024 + 1).read_to_end(&mut xml).map_err(|_| invalid)?;
        if xml.len() > 256 * 1024 || xml.len() > remaining || xml.contains(&0) || xml.contains(&b'&') {
            return Err(invalid.into());
        }
        remaining -= xml.len();
        let xml = std::str::from_utf8(&xml).map_err(|_| invalid)?;
        if xml.contains("<!DOCTYPE") || xml.contains("<!ENTITY")
            || mode.find_iter(xml).any(|m| !internal.is_match(&xml[m.end()..])) {
            return Err("OOXML 附件不能包含外部關聯或無法確認的關聯設定。".into());
        }
    }
    {
        let entry = archive.by_name(part).map_err(|_| invalid)?;
        if !entry.is_file() || entry.size() == 0 { return Err(invalid.into()); }
    }
    let entry = archive.by_name("[Content_Types].xml").map_err(|_| invalid)?;
    let mut xml = Vec::new();
    entry.take(256 * 1024 + 1).read_to_end(&mut xml).map_err(|_| invalid)?;
    if xml.len() > 256 * 1024 || xml.contains(&0) { return Err(invalid.into()); }
    std::str::from_utf8(&xml).map_err(|_| invalid)?;
    if xml.is_empty() { return Err(invalid.into()); }
    Ok(mime)
}

fn read_file(home: &Path, agent: &str, call: &Attachment, max_mb: u8)
    -> AttachmentResult<(Vec<u8>, &'static str, String)> {
    if !(1..=25).contains(&max_mb) { return Err("Invalid attachment_max_mb".into()); }
    let limit = u64::from(max_mb) * 1024 * 1024;
    let mut opened = open_file(home, agent, &call.file)?;
    let metadata = opened.file.metadata().map_err(|_| FILE_REFUSED)?;
    if !metadata.is_file() { return Err(FILE_REFUSED.into()); }
    if metadata.len() > limit { return Err("附件超過 attachment_max_mb 大小限制。".into()); }
    let mut bytes = Vec::new();
    (&mut opened.file).take(limit + 1).read_to_end(&mut bytes).map_err(|_| FILE_REFUSED)?;
    if bytes.len() as u64 > limit { return Err("附件超過 attachment_max_mb 大小限制。".into()); }
    prepare_file(call, bytes, limit)
}

fn prepare_file(call: &Attachment, bytes: Vec<u8>, limit: u64)
    -> AttachmentResult<(Vec<u8>, &'static str, String)> {
    if bytes.len() as u64 > limit { return Err(SIZE_REFUSED.into()); }
    let extension = Path::new(&call.file).extension().and_then(|s| s.to_str())
        .unwrap_or("").to_ascii_lowercase();
    let image = match extension.as_str() {
        "png" => Some((image::ImageFormat::Png, "image/png")),
        "jpg" | "jpeg" => Some((image::ImageFormat::Jpeg, "image/jpeg")),
        "webp" => Some((image::ImageFormat::WebP, "image/webp")),
        _ => None,
    };
    let mime = match image {
        Some((_, mime)) => mime,
        None => mimetype(&call.file, &bytes)?,
    };
    if !display_name_matches(&call.name, mime) {
        return Err("附件顯示名稱的副檔名必須符合已驗證的檔案格式。".into());
    }
    let mut name = call.name.clone();
    let (bytes, mime) = if let Some((format, mime)) = image {
        let bytes = reencode_image(&bytes, format, limit)?;
        let mime = if format == image::ImageFormat::WebP {
            name = Path::new(&name).with_extension("png").to_str().ok_or(FILE_REFUSED)?.to_owned();
            "image/png"
        } else { mime };
        (bytes, mime)
    } else { (bytes, mime) };
    if bytes.len() as u64 > limit { return Err(SIZE_REFUSED.into()); }
    Ok((bytes, mime, name))
}

pub(crate) fn odoo_attachment_precheck(home: &Path, agent: &str, params: &Value) -> AttachmentResult<()> {
    if !verified_employee(home, agent) { return Err("附件操作需要已驗證的員工身分。".into()); }
    ToolPolicy::load(home, agent)?.check_call(TOOL, params, None)?;
    parse(params)?;
    Ok(())
}

pub(super) async fn handle(
    params: &Value, home: &Path, odoo: &OdooState, agent: &str, bound: Option<Arc<OdooConnector>>,
) -> Value {
    if !verified_employee(home, agent) { return mcp_error("附件操作需要已驗證的員工身分。"); }
    let call = match parse(params) { Ok(call) => call, Err(error) => return mcp_error(&error) };
    let policy = match ToolPolicy::load(home, agent) {
        Ok(policy) => policy, Err(error) => return mcp_error(&error),
    };
    odoo.register_agent(agent, policy.agent.clone()).await;
    if let Err(error) = odoo.load_global_for_gate(home).await { return mcp_error(&error); }
    let (conn, decision): (_, Option<OdooIdentity>) = match bound {
        Some(conn) => (conn, None),
        None => match super::odoo::connect_odoo_pool_checked(home, odoo, agent, TOOL, params).await {
            Ok((conn, identity)) => (conn, Some(identity)),
            Err(error) => return mcp_error(&error),
        },
    };
    let result = async {
        policy.check_call(TOOL, params, decision.as_ref())?;
        // Local file I/O finishes before the first await; no OS handles or ZIP
        // readers cross an await. Upload precisely the bytes that passed checks.
        let (bytes, mime, name) = read_file(home, agent, &call, policy.access.attachment_max_mb)?;
        let records = conn.search_read(&call.model, vec![json!(["id", "=", call.res_id])], &["id"], 1).await?;
        if !records.as_array().is_some_and(|rows| rows.len() == 1 && rows[0]["id"].as_i64() == Some(call.res_id)) {
            return Err("找不到目標記錄，或連線帳號無法讀取這筆資料。".into());
        }
        // An upstream traceback can include submitted values (including datas).
        // Do not reflect any upload error payload back to the model or audit.
        conn.attachment_create(&call.model, call.res_id, &name, mime, &bytes).await
            .map_err(|_| "Odoo 附件建立失敗，請檢查目標記錄與帳號權限；重試前先確認是否已建立附件。".to_owned())
    }.await;
    let suffix = identity_suffix(decision.as_ref(), "create");
    odoo_identity_client::audit_call(home, agent, TOOL, &suffix, result.is_ok(), None);
    match result {
        Ok(id) => tool_text(&format!("Odoo attachment created: {id}\n{suffix}")),
        Err(error) => mcp_error(&error_text(&error, decision.as_ref())),
    }
}

#[cfg(test)]
#[path = "odoo_attachment_tests.rs"]
pub(super) mod tests;
