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

#[derive(Clone, PartialEq)]
struct Attachment {
    model: String,
    res_id: i64,
    file: String,
    name: String,
}

/// Owned, sanitized content. Only local validation can construct this value;
/// no file handles or path lookups cross the credential/connection boundary.
pub(crate) struct ValidatedAttachment {
    call: Attachment,
    home: std::path::PathBuf,
    agent: String,
    bytes: Vec<u8>,
    mime: &'static str,
    name: String,
}

impl ValidatedAttachment {
    fn recheck(&self, home: &Path, agent: &str, call: &Attachment, max_mb: u8)
        -> AttachmentResult<()> {
        if self.home.as_path() != home || self.agent != agent || &self.call != call {
            return Err(FILE_REFUSED.into());
        }
        if !(1..=25).contains(&max_mb) || self.bytes.len() as u64 > u64::from(max_mb) * 1024 * 1024 {
            return Err(SIZE_REFUSED.into());
        }
        if !display_name_matches(&self.name, self.mime) { return Err(FILE_REFUSED.into()); }
        let image = match self.mime {
            "image/png" => Some(image::ImageFormat::Png),
            "image/jpeg" => Some(image::ImageFormat::Jpeg),
            _ => None,
        };
        if let Some(format) = image {
            if image::guess_format(&self.bytes).ok() != Some(format) { return Err(FILE_REFUSED.into()); }
        } else if mimetype(&self.name, &self.bytes)? != self.mime {
            return Err(FILE_REFUSED.into());
        }
        Ok(())
    }
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

const PDF_MAX_NAMES: usize = 100_000;
const PDF_MAX_STREAMS: usize = 10_000;
const PDF_MAX_DEPTH: usize = 32;
const PDF_MAX_INFLATED: usize = 50 * 1024 * 1024;

fn pdf_space(byte: u8) -> bool { matches!(byte, 0 | 9 | 10 | 12 | 13 | 32) }

fn pdf_delimiter(byte: u8) -> bool {
    pdf_space(byte) || b"()<>[]{}/%".contains(&byte)
}

fn pdf_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

// Decode into a fixed-size prefix, but validate every escape even in long names.
// All names of interest fit in this prefix; longer names cannot match them.
fn pdf_name(raw: &[u8]) -> Option<([u8; 16], usize)> {
    let mut name = [0; 16];
    let mut length = 0;
    let mut pos = 0;
    while let Some(&byte) = raw.get(pos) {
        let decoded = if byte == b'#' {
            let high = pdf_hex(*raw.get(pos + 1)?)?;
            let low = pdf_hex(*raw.get(pos + 2)?)?;
            pos += 3;
            high * 16 + low
        } else {
            pos += 1;
            byte
        };
        if let Some(slot) = name.get_mut(length) { *slot = decoded; }
        length += 1;
    }
    Some((name, length))
}

fn pdf_name_is(raw: &[u8], expected: &[u8]) -> bool {
    pdf_name(raw).is_some_and(|(name, length)|
        length == expected.len() && name.get(..length) == Some(expected))
}

fn pdf_names_valid(bytes: &[u8], names: &mut usize) -> bool {
    let mut pos = 0;
    while let Some(&byte) = bytes.get(pos) {
        pos += 1;
        if byte != b'/' { continue; }
        *names += 1;
        if *names > PDF_MAX_NAMES { return false; }
        let start = pos;
        while bytes.get(pos).is_some_and(|b| !pdf_delimiter(*b)) { pos += 1; }
        let Some(raw) = bytes.get(start..pos) else { return false };
        let Some((name, length)) = pdf_name(raw) else { return false };
        if let Some(name) = name.get(..length) {
            if [b"JavaScript".as_slice(), b"JS", b"Launch", b"EmbeddedFile", b"EmbeddedFiles",
                b"OpenAction", b"AA", b"RichMedia", b"XFA", b"SubmitForm", b"ImportData",
                b"GoToR", b"GoToE", b"Sound", b"Movie", b"Encrypt"]
                .iter().any(|blocked| name.eq_ignore_ascii_case(blocked)) { return false; }
        }
    }
    true
}

#[derive(Clone, Copy)]
enum PdfToken<'a> {
    Name(&'a [u8]),
    Word(&'a [u8]),
    Dictionary,
    EndDictionary,
    Array,
    EndArray,
    Other,
}

// A bounded lexical walk, not a reference-table parser. Strings/comments are
// skipped only for stream structure; the separate name pass covers ALL bytes.
fn pdf_token<'a>(bytes: &'a [u8], pos: &mut usize) -> Result<Option<PdfToken<'a>>, ()> {
    while let Some(&byte) = bytes.get(*pos) {
        if pdf_space(byte) { *pos += 1; continue; }
        if byte == b'%' {
            while bytes.get(*pos).is_some_and(|b| !matches!(b, b'\r' | b'\n')) { *pos += 1; }
            continue;
        }
        *pos += 1;
        return Ok(Some(match byte {
            b'<' if bytes.get(*pos) == Some(&b'<') => { *pos += 1; PdfToken::Dictionary }
            b'>' if bytes.get(*pos) == Some(&b'>') => { *pos += 1; PdfToken::EndDictionary }
            b'[' => PdfToken::Array,
            b']' => PdfToken::EndArray,
            b'(' => {
                let mut depth = 1;
                while depth > 0 {
                    let byte = *bytes.get(*pos).ok_or(())?;
                    *pos += 1;
                    match byte {
                        b'\\' => { bytes.get(*pos).ok_or(())?; *pos += 1; }
                        b'(' => { depth += 1; if depth > PDF_MAX_DEPTH { return Err(()); } }
                        b')' => depth -= 1,
                        _ => (),
                    }
                }
                PdfToken::Other
            }
            b'<' => {
                loop {
                    let byte = *bytes.get(*pos).ok_or(())?;
                    *pos += 1;
                    if byte == b'>' { break; }
                    if !pdf_space(byte) && pdf_hex(byte).is_none() { return Err(()); }
                }
                PdfToken::Other
            }
            b')' | b'>' | b'{' | b'}' => return Err(()),
            b'/' => {
                let start = *pos;
                while bytes.get(*pos).is_some_and(|b| !pdf_delimiter(*b)) { *pos += 1; }
                PdfToken::Name(bytes.get(start..*pos).ok_or(())?)
            }
            _ => {
                let start = *pos - 1;
                while bytes.get(*pos).is_some_and(|b| !pdf_delimiter(*b)) { *pos += 1; }
                PdfToken::Word(bytes.get(start..*pos).ok_or(())?)
            }
        }));
    }
    Ok(None)
}

#[derive(Default)]
struct PdfDictionary {
    // None = no filter; Some(0) = a sole image filter; Some(n) = n Flate stages.
    filter: Option<usize>,
    length: Option<usize>,
    length_seen: bool,
}

fn pdf_filter(bytes: &[u8], pos: &mut usize) -> Result<usize, ()> {
    let first = pdf_token(bytes, pos)?.ok_or(())?;
    let array = matches!(first, PdfToken::Array);
    let mut token = if array { pdf_token(bytes, pos)?.ok_or(())? } else { first };
    let mut count = 0;
    let mut image = false;
    loop {
        let PdfToken::Name(name) = token else { return Err(()) };
        count += 1;
        if count > PDF_MAX_DEPTH { return Err(()); }
        if pdf_name_is(name, b"FlateDecode") || pdf_name_is(name, b"Fl") {
            if image { return Err(()); }
        } else if count == 1 && [b"DCTDecode".as_slice(), b"JPXDecode", b"CCITTFaxDecode", b"JBIG2Decode"]
            .iter().any(|allowed| pdf_name_is(name, allowed)) {
            image = true;
        } else { return Err(()); }
        if !array { break; }
        token = pdf_token(bytes, pos)?.ok_or(())?;
        if matches!(token, PdfToken::EndArray) { break; }
    }
    Ok(if image { 0 } else { count })
}

fn pdf_keyword(bytes: &[u8], pos: usize, keyword: &[u8]) -> bool {
    let Some(end) = pos.checked_add(keyword.len()) else { return false };
    bytes.get(pos..end) == Some(keyword)
        && (pos == 0 || bytes.get(pos - 1).is_some_and(|b| pdf_delimiter(*b)))
        && bytes.get(end).is_none_or(|b| pdf_delimiter(*b))
}

fn pdf_stream_start(bytes: &[u8], mut pos: usize) -> Option<usize> {
    // PDF stream data starts after LF or CRLF (accept CR conservatively too).
    while bytes.get(pos).is_some_and(|b| matches!(b, 0 | 9 | 12 | 32)) { pos += 1; }
    match bytes.get(pos) {
        Some(b'\r') => Some(pos + 1 + usize::from(bytes.get(pos + 1) == Some(&b'\n'))),
        Some(b'\n') => Some(pos + 1),
        _ => None,
    }
}

fn pdf_has_nested_stream(bytes: &[u8]) -> bool {
    // Refuse delimited stream + EOL syntax in every raw or inflated payload,
    // including image bytes. Never recursively parse or inflate nested streams.
    bytes.windows(6).enumerate().any(|(pos, window)| window == b"stream"
        && pdf_keyword(bytes, pos, b"stream")
        && pos.checked_add(6).and_then(|end| pdf_stream_start(bytes, end)).is_some())
}

fn pdf_stream_end(bytes: &[u8], start: usize, length: Option<usize>) -> Option<(usize, usize)> {
    // Use Length only when it ends exactly at a delimited endstream keyword.
    if let Some(end) = length.and_then(|n| start.checked_add(n)).filter(|end| *end <= bytes.len()) {
        // Only an immediate EOL may separate a consistent length and marker;
        // never rescan a distant whitespace suffix for each claimed Length.
        let marker = match bytes.get(end) {
            Some(b'\r') => end + 1 + usize::from(bytes.get(end + 1) == Some(&b'\n')),
            Some(b'\n') => end + 1,
            _ => end,
        };
        if pdf_keyword(bytes, marker, b"endstream") { return Some((end, marker + 9)); }
    }
    let rest = bytes.get(start..)?;
    for (offset, window) in rest.windows(9).enumerate() {
        let marker = start + offset;
        if window == b"endstream" && pdf_keyword(bytes, marker, b"endstream") {
            let mut end = marker;
            if end > start && bytes.get(end - 1) == Some(&b'\n') { end -= 1; }
            if end > start && bytes.get(end - 1) == Some(&b'\r') { end -= 1; }
            return Some((end, marker + 9));
        }
    }
    None
}

fn pdf_inflate(data: &[u8], cap: usize) -> Option<Vec<u8>> {
    if cap == 0 { return None; }
    let decoder = flate2::read::ZlibDecoder::new(data);
    let mut limited = decoder.take((cap as u64) + 1);
    // Reserve the exact bound up front; read_to_end may grow beyond the bound
    // while probing EOF. The fixed scratch buffer never depends on PDF input.
    let mut output = Vec::with_capacity(cap);
    let mut chunk = [0; 8192];
    loop {
        let read = limited.read(&mut chunk).ok()?;
        if read == 0 { break; }
        if read >= cap.saturating_sub(output.len()) { return None; }
        output.extend_from_slice(chunk.get(..read)?);
    }
    // Do not accept trailing compressed data hidden behind the first zlib member.
    if limited.into_inner().total_in() != data.len() as u64 { return None; }
    Some(output)
}

fn pdf_streams_valid(bytes: &[u8], names: &mut usize) -> Option<()> {
    let file_cap = bytes.len().saturating_mul(10).min(PDF_MAX_INFLATED);
    let mut inflated_total = 0usize;
    let mut streams = 0;
    let mut pos = 0;
    // None is an array frame; Some is a dictionary frame. No recursive parsing.
    let mut stack: Vec<Option<PdfDictionary>> = Vec::new();
    let mut preceding_dictionary = None;
    let mut saw_dictionary = false;
    while let Some(token) = pdf_token(bytes, &mut pos).ok()? {
        let previous = preceding_dictionary.take();
        match token {
            PdfToken::Dictionary => {
                if stack.len() >= PDF_MAX_DEPTH { return None; }
                stack.push(Some(PdfDictionary::default()));
                saw_dictionary = true;
            }
            PdfToken::Array => {
                if stack.len() >= PDF_MAX_DEPTH { return None; }
                stack.push(None);
            }
            PdfToken::EndDictionary => { preceding_dictionary = Some(stack.pop()??); }
            PdfToken::EndArray => { if stack.pop()?.is_some() { return None; } }
            PdfToken::Name(name) if pdf_name_is(name, b"Filter") => {
                let dictionary = stack.last_mut()?.as_mut()?;
                if dictionary.filter.is_some() { return None; }
                dictionary.filter = Some(pdf_filter(bytes, &mut pos).ok()?);
            }
            PdfToken::Name(name) if pdf_name_is(name, b"Length") => {
                let dictionary = stack.last_mut()?.as_mut()?;
                if dictionary.length_seen { return None; }
                dictionary.length_seen = true;
                let mut next = pos;
                if let Some(PdfToken::Word(number)) = pdf_token(bytes, &mut next).ok()? {
                    // Indirect lengths are ignored unless the candidate is consistent.
                    dictionary.length = number.iter().try_fold(0usize, |n, b|
                        if b.is_ascii_digit() { n.checked_mul(10)?.checked_add(usize::from(b - b'0')) }
                        else { None });
                }
            }
            PdfToken::Word(b"stream") => {
                if !stack.is_empty() { return None; }
                let dictionary = previous?;
                streams += 1;
                if streams > PDF_MAX_STREAMS { return None; }
                let start = pdf_stream_start(bytes, pos)?;
                let (end, after) = pdf_stream_end(bytes, start, dictionary.length)?;
                let data = bytes.get(start..end)?;
                if pdf_has_nested_stream(data) { return None; }
                let stream_cap = data.len().saturating_mul(10).min(PDF_MAX_INFLATED);
                let mut decoded = Vec::new();
                for stage in 0..dictionary.filter.unwrap_or(0) {
                    let input = if stage == 0 { data } else { decoded.as_slice() };
                    let cap = stream_cap.min(file_cap.checked_sub(inflated_total)?);
                    let output = pdf_inflate(input, cap)?;
                    inflated_total = inflated_total.checked_add(output.len())?;
                    if !pdf_names_valid(&output, names) { return None; }
                    if pdf_has_nested_stream(&output) { return None; }
                    decoded = output;
                }
                pos = after;
            }
            PdfToken::Word(b"endstream") => return None,
            _ => (),
        }
    }
    (saw_dictionary && stack.is_empty()).then_some(())
}

fn pdf_valid(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"%PDF-1.") && !bytes.starts_with(b"%PDF-2.") { return false; }
    let Some(tail) = bytes.get(bytes.len().saturating_sub(1024)..) else { return false };
    if !tail.windows(5).rposition(|w| w == b"%%EOF")
        .and_then(|end| tail.get(end + 5..))
        .is_some_and(|suffix| suffix.iter().all(|b| pdf_space(*b))) { return false; }
    let mut names = 0;
    pdf_names_valid(bytes, &mut names) && pdf_streams_valid(bytes, &mut names).is_some()
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
    if archive.len() > 4096 { return Err(invalid.into()); }
    for name in archive.file_names() {
        let lower = name.map_err(|_| invalid)?.to_ascii_lowercase();
        if lower.ends_with("vbaproject.bin") || lower.split('/').any(|component| component == "activex") {
            return Err(invalid.into());
        }
    }
    // Bound both each relationship part and total expanded relationship bytes.
    // Reject entities/DTDs and non-UTF-8 rather than guessing XML encodings.
    let mode = regex::Regex::new(r"(?i)\bTargetMode\b").map_err(|_| invalid)?;
    let internal = regex::Regex::new(r#"(?i)^\s*=\s*(?:"Internal"|'Internal')"#).map_err(|_| invalid)?;
    let mut remaining = 4 * 1024 * 1024usize;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|_| invalid)?;
        if !entry.name().map_err(|_| invalid)?.to_ascii_lowercase().ends_with(".rels") { continue; }
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

/// All filesystem and content validation completes before selecting/resolving
/// an identity or preparing a bound connector. Directory fds die in this task.
pub(crate) async fn validate_attachment(home: &Path, agent: &str, params: &Value)
    -> AttachmentResult<ValidatedAttachment> {
    if !verified_employee(home, agent) { return Err("附件操作需要已驗證的員工身分。".into()); }
    let policy = ToolPolicy::load(home, agent)?;
    policy.data_preconnect(TOOL, params, None)?;
    let call = parse(params)?;
    let home = home.to_path_buf();
    let agent = agent.to_owned();
    tokio::task::spawn_blocking(move || {
        let (bytes, mime, name) = read_file(&home, &agent, &call, policy.access.attachment_max_mb)?;
        Ok::<_, String>(ValidatedAttachment { call, home, agent, bytes, mime, name })
    }).await.map_err(|_| "附件讀取或檢查失敗。".to_owned())?
}

pub(super) async fn handle(
    params: &Value, home: &Path, odoo: &OdooState, agent: &str, bound: Option<Arc<OdooConnector>>,
    validated: Option<ValidatedAttachment>,
) -> Value {
    if !verified_employee(home, agent) { return mcp_error("附件操作需要已驗證的員工身分。"); }
    let call = match parse(params) { Ok(call) => call, Err(error) => return mcp_error(&error) };
    let policy = match ToolPolicy::load(home, agent) {
        Ok(policy) => policy, Err(error) => return mcp_error(&error),
    };
    if let Err(error) = policy.data_preconnect(TOOL, params, None) { return mcp_error(&error); }
    let validated = match validated {
        Some(validated) => validated,
        None if bound.is_some() => return mcp_error("缺少連線前已驗證的附件。"),
        None => match validate_attachment(home, agent, params).await {
            Ok(validated) => validated, Err(error) => return mcp_error(&error),
        },
    };
    // Recheck the owned bytes and their request binding, never reopen the path.
    let check_home = home.to_path_buf();
    let check_agent = agent.to_owned();
    let check_call = call.clone();
    let max_mb = policy.access.attachment_max_mb;
    let checked = tokio::task::spawn_blocking(move || {
        validated.recheck(&check_home, &check_agent, &check_call, max_mb)?;
        Ok::<_, String>(validated)
    }).await;
    let validated = match checked {
        Ok(Ok(validated)) => validated,
        Ok(Err(error)) => return mcp_error(&error),
        Err(_) => return mcp_error("附件讀取或檢查失敗。"),
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
        let records = conn.search_read(&call.model, vec![json!(["id", "=", call.res_id])], &["id"], 1).await?;
        if !records.as_array().is_some_and(|rows| rows.len() == 1 && rows[0]["id"].as_i64() == Some(call.res_id)) {
            return Err("找不到目標記錄，或連線帳號無法讀取這筆資料。".into());
        }
        // An upstream traceback can include submitted values (including datas).
        // Do not reflect any upload error payload back to the model or audit.
        conn.attachment_create(&call.model, call.res_id, &validated.name, validated.mime, &validated.bytes).await
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
