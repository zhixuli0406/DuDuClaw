//! Read-only installer checks. I1 supplies the composite runtime verifier.
use super::*;

pub(super) trait CompositeReadiness {
    fn verify(&mut self) -> Result<()>;
}
pub(super) struct MissingComposite;
impl CompositeReadiness for MissingComposite {
    fn verify(&mut self) -> Result<()> {
        Err(refused("尚未接上完整隔離就緒檢查；不能啟用員工執行。"))
    }
}

fn binaries() -> Result<()> {
    if !layout::published_components(Path::new("/"), 0, 0)? {
        return Err(refused("缺少隔離元件。"));
    }
    for name in [BINARY, LAUNCHER] {
        let path = Path::new(name);
        parents(path)?;
        trusted(path, false)?;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.mode() & 0o7777 != 0o755 || metadata.nlink() != 1 || metadata.len() == 0 {
            return Err(refused("服務執行檔權限或內容不符。"));
        }
    }
    Ok(())
}

fn directories() -> Result<()> {
    for (path, mode) in layout::DIRECTORIES {
        parents(Path::new(path))?;
        trusted(Path::new(path), true)?;
        if fs::symlink_metadata(path)?.mode() & 0o7777 != *mode {
            return Err(refused("隔離服務目錄權限不符。"));
        }
    }
    for path in ["/usr/lib/duduclaw/parse-root", "/usr/lib/duduclaw/parse-root-tree"] {
        if !layout::parse_root_is_reusable(Path::new(path), 0, 0)? {
            return Err(refused("解析服務目錄含有非預期內容。"));
        }
    }
    Ok(())
}

fn accounts() -> Result<()> {
    let defs = fs::read_to_string("/etc/login.defs")?;
    let mut identities = Vec::new();
    for name in std::iter::once("duduclaw").chain(layout::ACCOUNTS.iter().copied()) {
        let passwd = String::from_utf8(checked("getent", &["passwd", name])?.stdout)
            .map_err(|_| refused("無法讀取服務帳號。"))?;
        let fields: Vec<_> = passwd.trim_end_matches('\n').split(':').collect();
        let expected_home = if name == "duduclaw" { "/var/lib/duduclaw" } else { "/nonexistent" };
        if fields.len() != 7 || fields[0] != name || fields[5] != expected_home
            || !matches!(fields[6], "/usr/sbin/nologin" | "/sbin/nologin") {
            return Err(refused("服務帳號的家目錄或登入設定不符。"));
        }
        let uid = fields[2].parse::<u32>().map_err(|_| refused("無效的服務身分。"))?;
        let gid = fields[3].parse::<u32>().map_err(|_| refused("無效的服務群組。"))?;
        layout::validate_system_ids(uid, gid, &defs).map_err(refused)?;
        if identities.iter().any(|(u, g)| *u == uid || *g == gid) { return Err(refused("服務身分重複。")); }
        identities.push((uid, gid));
        if name == "duduclaw" {
            let home = Path::new(expected_home);
            parents(home)?;
            if !layout::migrated_home_is_private(home, 0, 0, uid, gid)? {
                return Err(refused("遷移後的服務資料目錄擁有者或權限不符。"));
            }
        }
        let groups = String::from_utf8(checked("id", &["-G", name])?.stdout)
            .map_err(|_| refused("無法讀取服務群組。"))?;
        if groups.split_whitespace().collect::<Vec<_>>() != vec![fields[3]] {
            return Err(refused("服務帳號帶有額外群組。"));
        }
        let shadow = String::from_utf8(checked("getent", &["shadow", name])?.stdout)
            .map_err(|_| refused("無法確認服務帳號已鎖定。"))?;
        let shadow: Vec<_> = shadow.trim_end_matches('\n').split(':').collect();
        if shadow.len() != 9 || shadow[0] != name || !matches!(shadow[1].as_bytes().first(), Some(b'!' | b'*')) {
            return Err(refused("服務帳號尚未鎖定密碼。"));
        }
        if name != "duduclaw" && shadow[7] != "1" { return Err(refused("輔助服務帳號的到期封鎖設定不符。")); }
        match fs::symlink_metadata(format!("/var/lib/systemd/linger/{name}")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            _ => return Err(refused("服務帳號仍可啟動背景登入工作階段。")),
        }
    }
    Ok(())
}

fn units() -> Result<()> {
    for (name, expected) in layout::UNITS {
        let path = Path::new("/etc/systemd/system").join(name);
        parents(&path)?;
        trusted(&path, false)?;
        if fs::symlink_metadata(&path)?.mode() & 0o7777 != 0o644 || fs::read_to_string(&path)? != *expected {
            return Err(refused("隔離服務單元與安裝版本不符。"));
        }
    }
    for name in [UNIT, "duduclaw-spawnd.service", "duduclaw-spawnd.socket", "duduclaw-egress.service",
        "duduclaw-egress.socket", "duduclaw-updated.service", "duduclaw-parse.socket"] {
        let output = checked("systemctl", &["show", name,
            "--property=LoadState,ActiveState,FragmentPath,NeedDaemonReload,DropInPaths"])?;
        let text = String::from_utf8(output.stdout).map_err(|_| refused("無法讀取服務狀態。"))?;
        let mut fields: Vec<_> = text.lines().collect(); fields.sort_unstable();
        let fragment = format!("FragmentPath=/etc/systemd/system/{name}");
        if fields != ["ActiveState=active", "DropInPaths=", fragment.as_str(), "LoadState=loaded", "NeedDaemonReload=no"] {
            return Err(refused("隔離服務尚未載入預期設定，或含有尚未驗證的覆寫設定。"));
        }
    }
    Ok(())
}

// Check every group even if one fails, so the administrator sees a useful
// summary. Never echo subprocess output (getent shadow includes credentials).
pub(super) fn check(composite: &mut impl CompositeReadiness) -> Result<()> {
    let results = [("執行檔", binaries()), ("服務帳號", accounts()),
        ("目錄權限", directories()), ("服務單元", units()), ("完整隔離驗證", composite.verify())];
    let mut failed = 0;
    for (label, result) in results {
        if result.is_ok() { println!("[通過] {label}"); }
        else { failed += 1; println!("[未通過] {label}"); }
    }
    if failed != 0 { return Err(refused(format!("{failed} 項就緒檢查未通過；尚未確認可安全執行員工工作。"))); }
    Ok(())
}
