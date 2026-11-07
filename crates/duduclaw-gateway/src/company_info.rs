//! Customer identity is local configuration, separate from the signed product brand.
use std::{io::{Cursor, Read, Write}, path::Path};
use image::{ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const MAX_LOGO_BYTES: usize = 1024 * 1024;
const MAX_DIMENSION: u32 = 4096;
const INVALID_LOGO: &str = "請選擇 1 MB 以內的 PNG、JPG 或 WebP 靜態圖片。";
const UNAVAILABLE: &str = "目前無法讀取或儲存公司資訊，請稍後再試。";

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub(crate) struct CompanyInfo {
    pub name: String,
    pub logo_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompanyUpdate {
    pub name: String,
    #[serde(default)]
    pub remove_logo: bool,
}

pub(crate) fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_name(name: &str) -> bool {
    static FORMAT_CONTROLS: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\p{Cf}").expect("Unicode format category"));
    name.chars().count() <= 120 && !FORMAT_CONTROLS.is_match(name)
        && !name.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

fn from_table(table: &toml::Table) -> Result<CompanyInfo, String> {
    let Some(value) = table.get("company") else { return Ok(CompanyInfo::default()); };
    let company = value.as_table().ok_or(UNAVAILABLE)?;
    let name = match company.get("name") {
        None => "", Some(value) => value.as_str().filter(|s| valid_name(s)).ok_or(UNAVAILABLE)?,
    };
    let hash = match company.get("logo_hash") {
        None => None, Some(value) => Some(value.as_str().filter(|s| valid_hash(s)).ok_or(UNAVAILABLE)?),
    };
    Ok(CompanyInfo { name: name.to_owned(), logo_url: hash.map(|h| format!("/api/company/logo/{h}.png")) })
}

pub(crate) fn read(home: &Path) -> Result<CompanyInfo, String> {
    let raw = crate::handlers::config_commit::read_text_or_empty(&home.join("config.toml")).map_err(|_| UNAVAILABLE)?;
    let table = raw.parse::<toml::Table>().map_err(|_| UNAVAILABLE)?;
    from_table(&table)
}

// Reject appended payloads instead of relying on decoders that ignore bytes
// after the image. Metadata inside a valid image is discarded by re-encoding.
fn complete_container(bytes: &[u8], format: ImageFormat) -> bool {
    match format {
        ImageFormat::Png => {
            let mut pos = 8usize;
            while let Some(header) = bytes.get(pos..pos.saturating_add(8)) {
                let len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
                let Some(end) = pos.checked_add(12).and_then(|p| p.checked_add(len)) else { return false; };
                if end > bytes.len() { return false; }
                if &header[4..8] == b"acTL" { return false; }
                if &header[4..8] == b"IEND" { return len == 0 && end == bytes.len(); }
                pos = end;
            }
            false
        }
        ImageFormat::WebP => {
            let Some(header) = bytes.get(..12) else { return false; };
            let size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
            if size.checked_add(8) != Some(bytes.len()) { return false; }
            let mut pos = 12usize;
            while let Some(chunk) = bytes.get(pos..pos.saturating_add(8)) {
                if matches!(&chunk[..4], b"ANIM" | b"ANMF") { return false; }
                let len = u32::from_le_bytes(chunk[4..8].try_into().unwrap()) as usize;
                let Some(end) = pos.checked_add(8).and_then(|p| p.checked_add(len)).and_then(|p| p.checked_add(len % 2)) else { return false; };
                if end > bytes.len() { return false; }
                pos = end;
            }
            pos == bytes.len()
        }
        ImageFormat::Jpeg => {
            let mut pos = 2usize;
            let mut entropy = false;
            while pos < bytes.len() {
                if bytes[pos] != 0xff {
                    if entropy { pos += 1; continue; }
                    return false;
                }
                while bytes.get(pos) == Some(&0xff) { pos += 1; }
                let Some(&marker) = bytes.get(pos) else { return false; };
                pos += 1;
                if entropy && (marker == 0 || (0xd0..=0xd7).contains(&marker)) { continue; }
                if marker == 0xd9 { return pos == bytes.len(); }
                if marker == 0 || marker == 0xd8 || (0xd0..=0xd7).contains(&marker) { return false; }
                let Some(length) = bytes.get(pos..pos.saturating_add(2)) else { return false; };
                let length = u16::from_be_bytes(length.try_into().unwrap()) as usize;
                if length < 2 { return false; }
                let Some(end) = pos.checked_add(length) else { return false; };
                if end > bytes.len() { return false; }
                entropy = marker == 0xda;
                pos = end;
            }
            false
        }
        _ => false,
    }
}

fn decode_logo(bytes: &[u8], max_dimension: u32) -> Result<image::DynamicImage, String> {
    if bytes.is_empty() || bytes.len() > MAX_LOGO_BYTES { return Err(INVALID_LOGO.into()); }
    let format = image::guess_format(bytes).map_err(|_| INVALID_LOGO)?;
    if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)
        || !complete_container(bytes, format) { return Err(INVALID_LOGO.into()); }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(max_dimension);
    limits.max_image_height = Some(max_dimension);
    limits.max_alloc = Some(if max_dimension == 256 { 1024 * 1024 } else { 64 * 1024 * 1024 });
    // Set limits before constructing any decoder, including a dimension probe:
    // PNG decoder initialization itself may decompress untrusted metadata.
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| INVALID_LOGO.to_owned())?;
    if decoded.width() == 0 || decoded.height() == 0 { return Err(INVALID_LOGO.into()); }
    Ok(decoded)
}

pub(crate) fn sanitize_logo(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let image = decode_logo(bytes, MAX_DIMENSION)?;
    let clean = image::DynamicImage::ImageRgba8(image.thumbnail(256, 256).to_rgba8());
    let mut png = Cursor::new(Vec::new());
    clean.write_to(&mut png, ImageFormat::Png).map_err(|_| INVALID_LOGO)?;
    Ok(png.into_inner())
}

fn store_logo(home: &Path, png: &[u8]) -> Result<String, String> {
    let hash = hex::encode(Sha256::digest(png));
    let dir = home.join("company_logos");
    std::fs::create_dir_all(&dir).map_err(|_| UNAVAILABLE)?;
    if !std::fs::symlink_metadata(&dir).map_err(|_| UNAVAILABLE)?.is_dir() { return Err(UNAVAILABLE.into()); }
    let mut temp = tempfile::NamedTempFile::new_in(&dir).map_err(|_| UNAVAILABLE)?;
    temp.write_all(png).map_err(|_| UNAVAILABLE)?;
    temp.as_file().sync_all().map_err(|_| UNAVAILABLE)?;
    // Replacing a same-hash entry repairs corruption without changing its URL.
    temp.persist(dir.join(format!("{hash}.png"))).map_err(|_| UNAVAILABLE)?;
    #[cfg(unix)]
    std::fs::File::open(&dir).and_then(|f| f.sync_all()).map_err(|_| UNAVAILABLE)?;
    Ok(hash)
}

pub(crate) fn update(home: &Path, input: CompanyUpdate, guard: impl FnOnce() -> Result<(), String>) -> Result<CompanyInfo, String> {
    if !valid_name(&input.name) {
        return Err("公司名稱最多 120 字，請確認圖片操作後再儲存。".into());
    }
    persist(home, Some(&input.name), input.remove_logo, None, guard)
}

// Only the authenticated HTTP handler calls this after validating the raw body.
// Preserve the current name, including concurrent name edits, through config CAS.
pub(crate) fn upload_logo(home: &Path, png: &[u8], guard: impl FnOnce() -> Result<(), String>) -> Result<CompanyInfo, String> {
    persist(home, None, false, Some(png), guard)
}

fn persist(home: &Path, name: Option<&str>, remove_logo: bool, png: Option<&[u8]>, guard: impl FnOnce() -> Result<(), String>) -> Result<CompanyInfo, String> {
    let path = home.join("config.toml");
    let raw = crate::handlers::config_commit::read_text_or_empty(&path).map_err(|_| UNAVAILABLE)?;
    let mut table = raw.parse::<toml::Table>().map_err(|_| UNAVAILABLE)?;
    from_table(&table)?;
    let expected = crate::handlers::config_commit::content_hash(&raw);
    crate::handlers::config_commit::commit_rendered_locked(&path, &expected, |current| {
        guard()?;
        let company = crate::handlers::config_commit::table_at_mut(&mut table, &["company"])?;
        if let Some(name) = name { company.insert("name".into(), toml::Value::String(name.trim().to_owned())); }
        if remove_logo { company.remove("logo_hash"); }
        if let Some(png) = png { company.insert("logo_hash".into(), toml::Value::String(store_logo(home, png)?)); }
        crate::handlers::config_commit::render_preserving(current, &table)
    }).map_err(|_| "公司資訊尚未儲存，請重新整理後再試。".to_owned())?;
    from_table(&table)
}

pub(crate) fn read_logo(home: &Path, filename: &str) -> Option<Vec<u8>> {
    let hash = filename.strip_suffix(".png").filter(|s| valid_hash(s))?;
    let file = open_logo(home, filename)?;
    read_open_logo(file, hash)
}

fn read_open_logo(file: std::fs::File, hash: &str) -> Option<Vec<u8>> {
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_LOGO_BYTES as u64 || !single_link(&file, &meta) { return None; }
    let mut bytes = Vec::new();
    (&file).take(MAX_LOGO_BYTES as u64 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > MAX_LOGO_BYTES || !single_link(&file, &file.metadata().ok()?)
        || hex::encode(Sha256::digest(&bytes)) != hash
        || image::guess_format(&bytes).ok()? != ImageFormat::Png { return None; }
    decode_logo(&bytes, 256).ok()?;
    Some(bytes)
}

#[cfg(unix)]
fn open_logo(home: &Path, filename: &str) -> Option<std::fs::File> {
    use std::os::{fd::{AsRawFd, FromRawFd}, unix::fs::OpenOptionsExt};
    use nix::fcntl::{openat, OFlag};
    // Pin the directory too: swapping it cannot redirect the subsequent openat.
    let dir = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(home.join("company_logos")).ok()?;
    let fd = openat(Some(dir.as_raw_fd()), filename,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        nix::sys::stat::Mode::empty()).ok()?;
    // SAFETY: openat returned a new owned descriptor, transferred exactly once.
    Some(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn single_link(_: &std::fs::File, meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.nlink() == 1
}

#[cfg(windows)]
fn open_logo(home: &Path, filename: &str) -> Option<std::fs::File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    // Hold the parent without FILE_SHARE_DELETE so it cannot be replaced.
    let dir = std::fs::OpenOptions::new().access_mode(0x80).share_mode(0x1 | 0x2)
        .custom_flags(0x0200_0000 | 0x0020_0000).open(home.join("company_logos")).ok()?;
    let meta = dir.metadata().ok()?;
    if !meta.is_dir() || meta.file_attributes() & 0x400 != 0 { return None; }
    // OPEN_REPARSE_POINT refuses following the final link; the handle check
    // below rejects it. Deny writes/deletes while validating and reading.
    std::fs::OpenOptions::new().read(true).share_mode(0x1)
        .custom_flags(0x0020_0000).open(home.join("company_logos").join(filename)).ok()
}

#[cfg(windows)]
fn single_link(file: &std::fs::File, meta: &std::fs::Metadata) -> bool {
    use std::{ffi::c_void, os::windows::{fs::MetadataExt, io::AsRawHandle}};
    // Stable std does not expose the Windows link count. FILE_STANDARD_INFO
    // queries it on the already-open handle (no path lookup).
    #[repr(C)]
    #[derive(Default)]
    struct StandardInfo { allocation: i64, end: i64, links: u32, delete_pending: u8, directory: u8 }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(handle: *mut c_void, class: i32, info: *mut c_void, size: u32) -> i32;
    }
    let mut info = StandardInfo::default();
    // SAFETY: the borrowed File owns a live handle and the output uses the
    // FILE_STANDARD_INFO ABI, with its exact size (class FileStandardInfo = 1).
    let ok = unsafe { GetFileInformationByHandleEx(file.as_raw_handle(), 1,
        (&mut info as *mut StandardInfo).cast(), std::mem::size_of::<StandardInfo>() as u32) };
    ok != 0 && info.links == 1 && info.directory == 0 && info.delete_pending == 0
        && meta.file_attributes() & 0x400 == 0
}

#[cfg(not(any(unix, windows)))]
fn open_logo(_: &Path, _: &str) -> Option<std::fs::File> { None }
#[cfg(not(any(unix, windows)))]
fn single_link(_: &std::fs::File, _: &std::fs::Metadata) -> bool { false }

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(format: ImageFormat) -> Vec<u8> {
        let mut buffer = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(600, 300).write_to(&mut buffer, format).unwrap();
        buffer.into_inner()
    }
    #[test]
    fn company_name_rejects_controls_and_line_separators() {
        for name in ["line\nnext", "line\rnext", "line\u{2028}next", "line\u{2029}next", "null\0byte"] {
            assert!(!valid_name(name));
        }
        assert!(valid_name(""));
        assert!(valid_name("範例公司"));
        assert!(valid_name(&"名".repeat(120)));
        assert!(!valid_name(&"名".repeat(121)));
    }
    #[test]
    fn company_name_rejects_bidi_and_format_controls_in_updates_and_config() {
        let home = tempfile::tempdir().unwrap();
        for c in (0x202a..=0x202e).chain(0x2066..=0x2069)
            .chain([0x00ad, 0x061c, 0x200b, 0x200c, 0x200d, 0x200e, 0x200f, 0x2060, 0xfeff, 0xe0001]) {
            let name = format!("Acme{}Corp", char::from_u32(c).unwrap());
            assert!(!valid_name(&name), "U+{c:04X}");
            let mut table = toml::Table::new();
            table.insert("company".into(), toml::Value::Table(toml::Table::from_iter([
                ("name".into(), toml::Value::String(name.clone())),
            ])));
            assert!(from_table(&table).is_err());
            assert!(update(home.path(), CompanyUpdate { name, remove_logo: false }, || Ok(())).is_err());
        }
        assert!(!home.path().join("config.toml").exists());
        assert!(valid_name("臺灣・株式会社 café 🐈"));
    }
    #[test]
    fn company_update_rejects_legacy_base64_upload_even_null() {
        for value in [serde_json::json!(null), serde_json::json!("aGVsbG8=")] {
            assert!(serde_json::from_value::<CompanyUpdate>(serde_json::json!({
                "name": "Acme", "logo_base64": value,
            })).is_err());
        }
    }
    #[test]
    fn company_logo_normalizes_all_allowed_formats_to_small_png() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP] {
            let clean = sanitize_logo(&sample(format)).unwrap();
            assert_eq!(image::guess_format(&clean).unwrap(), ImageFormat::Png);
            let image = image::load_from_memory(&clean).unwrap();
            assert_eq!((image.width(), image.height()), (256, 128));
        }
    }
    #[test]
    fn company_logo_refuses_svg_gif_truncated_oversized_and_polyglots() {
        for bytes in [b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec(), sample(ImageFormat::Gif), vec![0; MAX_LOGO_BYTES + 1]] {
            assert!(sanitize_logo(&bytes).is_err());
        }
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP] {
            let mut bytes = sample(format);
            assert!(sanitize_logo(&bytes[..bytes.len() - 1]).is_err());
            bytes.extend_from_slice(b"<script>alert(1)</script>");
            assert!(sanitize_logo(&bytes).is_err());
            bytes.extend_from_slice(&[0xff, 0xd9]);
            assert!(sanitize_logo(&bytes).is_err());
        }
        assert!(sanitize_logo(b"data:image/png;base64,aaaa").is_err());
    }
    #[test]
    fn company_logo_reencoding_strips_embedded_metadata() {
        let original = sample(ImageFormat::Jpeg);
        let note = b"private-company-metadata";
        let mut bytes = original[..2].to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe]);
        bytes.extend_from_slice(&((note.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(note);
        bytes.extend_from_slice(&original[2..]);
        let clean = sanitize_logo(&bytes).unwrap();
        assert!(!clean.windows(note.len()).any(|window| window == note));
    }
    #[test]
    fn company_logo_refuses_large_dimensions() {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(MAX_DIMENSION + 1, 1).write_to(&mut bytes, ImageFormat::Png).unwrap();
        assert!(sanitize_logo(bytes.get_ref()).is_err());
    }
    #[test]
    fn company_updates_use_guard_and_preserve_config_comments_and_product_brand() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        std::fs::write(&path, "# operator comment\n[gateway]\nport = 18889\n").unwrap();
        let input = || CompanyUpdate { name: "範例公司".into(), remove_logo: false };
        assert!(update(home.path(), input(), || Err("denied".into())).is_err());
        assert!(!home.path().join("company_logos").exists());
        let result = update(home.path(), input(), || Ok(())).unwrap();
        assert_eq!(result.name, "範例公司");
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# operator comment"));
        let png = sanitize_logo(&sample(ImageFormat::Png)).unwrap();
        assert!(upload_logo(home.path(), &png, || Err("denied".into())).is_err());
        assert!(!home.path().join("company_logos").exists());
        let result = upload_logo(home.path(), &png, || Ok(())).unwrap();
        assert_eq!(result.name, "範例公司");
        let filename = result.logo_url.unwrap().rsplit('/').next().unwrap().to_owned();
        assert!(read_logo(home.path(), &filename).is_some());
        assert!(read_logo(home.path(), "../config.toml").is_none());
        let result = update(home.path(), CompanyUpdate { name: "".into(), remove_logo: true }, || Ok(())).unwrap();
        assert_eq!(result, CompanyInfo::default());
        assert!(read_logo(home.path(), &filename).is_some());
        assert!(!home.path().join("distributor_brand.json").exists());
    }

    #[test]
    fn company_public_logo_requires_compliant_png_even_with_matching_hash() {
        let home = tempfile::tempdir().unwrap();
        let clean = sanitize_logo(&sample(ImageFormat::Png)).unwrap();
        let mut appended = clean.clone();
        appended.extend_from_slice(b"not png");
        let mut broken = clean.clone();
        broken.truncate(broken.len() - 1);
        for bytes in [b"private text".to_vec(), sample(ImageFormat::Jpeg), sample(ImageFormat::Png),
            vec![0; MAX_LOGO_BYTES + 1], appended, broken] {
            let hash = store_logo(home.path(), &bytes).unwrap();
            assert!(read_logo(home.path(), &format!("{hash}.png")).is_none());
        }
        let hash = store_logo(home.path(), &clean).unwrap();
        let filename = format!("{hash}.png");
        assert_eq!(read_logo(home.path(), &filename), Some(clean));
        std::fs::write(home.path().join("company_logos").join(&filename), b"replaced").unwrap();
        assert!(read_logo(home.path(), &filename).is_none());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn company_public_logo_rejects_hard_links() {
        let home = tempfile::tempdir().unwrap();
        let clean = sanitize_logo(&sample(ImageFormat::Png)).unwrap();
        let hash = store_logo(home.path(), &clean).unwrap();
        let filename = format!("{hash}.png");
        let path = home.path().join("company_logos").join(&filename);
        std::fs::hard_link(&path, home.path().join("other.png")).unwrap();
        assert!(read_logo(home.path(), &filename).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn company_public_logo_pins_fd_and_rejects_symlinks_directories_and_fifos() {
        use std::os::unix::fs::symlink;
        let home = tempfile::tempdir().unwrap();
        let clean = sanitize_logo(&sample(ImageFormat::Png)).unwrap();
        let hash = store_logo(home.path(), &clean).unwrap();
        let filename = format!("{hash}.png");
        let dir = home.path().join("company_logos");
        let path = dir.join(&filename);
        let file = open_logo(home.path(), &filename).unwrap();
        // A deterministic metadata-to-read swap: the open descriptor remains
        // bound to the original inode even when its name becomes a symlink.
        std::fs::rename(&path, home.path().join("original.png")).unwrap();
        std::fs::write(home.path().join("secret"), b"private").unwrap();
        symlink(home.path().join("secret"), &path).unwrap();
        assert_eq!(read_open_logo(file, &hash), Some(clean.clone()));
        assert!(read_logo(home.path(), &filename).is_none());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(read_logo(home.path(), &filename).is_none());
        std::fs::remove_dir(&path).unwrap();
        nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR).unwrap();
        assert!(read_logo(home.path(), &filename).is_none());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, clean).unwrap();
        std::fs::rename(&dir, home.path().join("moved")).unwrap();
        symlink(home.path().join("moved"), &dir).unwrap();
        assert!(read_logo(home.path(), &filename).is_none());
    }
}
