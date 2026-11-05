//! Match Codex 0.166's machine-wide paths (Known Folder, not ProgramData env).
use std::{ffi::{c_void, OsString}, os::windows::ffi::OsStringExt, path::PathBuf};

#[repr(C)]
struct Guid { a: u32, b: u16, c: u16, d: [u8; 8] }
#[link(name = "shell32")]
unsafe extern "system" {
    fn SHGetKnownFolderPath(id: *const Guid, flags: u32, token: *mut c_void, path: *mut *mut u16) -> i32;
}
#[link(name = "ole32")]
unsafe extern "system" { fn CoTaskMemFree(value: *mut c_void); }
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(key: *mut c_void, subkey: *const u16, value: *const u16, flags: u32,
        kind: *mut u32, data: *mut c_void, size: *mut u32) -> i32;
}

pub(super) fn paths() -> Result<Vec<PathBuf>, String> {
    let id = Guid { a: 0x62ab5d82, b: 0xfdc1, c: 0x4dc3, d: [0xa9,0xdd,0x07,0x0d,0x1d,0x49,0x5d,0x97] };
    let mut raw = std::ptr::null_mut();
    // SAFETY: Windows initializes an allocated NUL-terminated UTF-16 string.
    let status = unsafe { SHGetKnownFolderPath(&id, 0, std::ptr::null_mut(), &mut raw) };
    let system = if status == 0 && !raw.is_null() {
        let path = unsafe {
            let mut length = 0;
            while *raw.add(length) != 0 { length += 1; }
            let path = PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(raw, length)));
            CoTaskMemFree(raw.cast());
            path
        };
        path
    } else {
        // Same fallback as Codex's windows_codex_system_dir.
        PathBuf::from(r"C:\ProgramData")
    }.join("OpenAI/Codex");
    let registry = PathBuf::from(r"HKLM\SOFTWARE\Policies\OpenAI\Codex\RequirementsPath");
    let subkey: Vec<u16> = r"SOFTWARE\Policies\OpenAI\Codex".encode_utf16().chain(Some(0)).collect();
    let value: Vec<u16> = "RequirementsPath".encode_utf16().chain(Some(0)).collect();
    let mut bytes = 0u32;
    // Read the 64-bit policy key exactly as Codex, without environment expansion.
    let flags = 0x00000002 | 0x00000004 | 0x00010000 | 0x10000000;
    let hklm = (-2147483646isize) as *mut c_void;
    let read = |data: *mut c_void, size: &mut u32| unsafe {
        RegGetValueW(hklm, subkey.as_ptr(), value.as_ptr(), flags, std::ptr::null_mut(), data, size)
    };
    let status = read(std::ptr::null_mut(), &mut bytes);
    let requirements = match status {
        2 | 3 => system.join("requirements.toml"),
        0 => {
            if bytes == 0 || bytes > 65536 || bytes % 2 != 0 {
                return Err(super::refusal(&registry, "無法解析管理設定路徑"));
            }
            let mut data = vec![0u16; bytes as usize / 2];
            if read(data.as_mut_ptr().cast(), &mut bytes) != 0 {
                return Err(super::refusal(&registry, "無法讀取管理設定路徑"));
            }
            let length = data.iter().position(|ch| *ch == 0).unwrap_or(data.len());
            let path = PathBuf::from(OsString::from_wide(&data[..length]));
            if !path.is_absolute() { return Err(super::refusal(&registry, "管理設定路徑不是絕對路徑")); }
            path
        }
        _ => return Err(super::refusal(&registry, "無法讀取管理設定路徑")),
    };
    Ok(vec![system.join("config.toml"), requirements])
}
