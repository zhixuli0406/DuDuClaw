#![cfg(target_os = "linux")]
use duduclaw_parse_worker::{client, worker::*};
static ROOT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn require_root() {
    assert!(nix::unistd::geteuid().is_root(), "requires root and installed socket/template/worker");
}

#[test]
#[ignore = "requires root, systemd and installed Package E units/binary"]
fn iso_root_systemd_document_roundtrip() {
    let _guard = ROOT_LOCK.lock().unwrap();
    require_root();
    assert_eq!(client::extract_bytes("員工文件".as_bytes(), ContentFormat::Text).unwrap(), "員工文件");
}

#[test]
#[ignore = "iso_root_: installed worker/units, system bus and root-created clean retirement.lease; disposable VM"]
fn iso_root_e2e_worker_retirement_holds_host_slot_through_response_and_drop() {
    assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"),
        "this test changes a global host lease and requires an explicitly disposable VM");
    use duduclaw_parse_worker::{client::WorkerLauncher, retirement::{RetiringLauncher, LEASE_PATH}, transport};
    use std::time::{Duration, Instant};
    let _guard = ROOT_LOCK.lock().unwrap();
    require_root();
    assert_eq!(std::fs::read_to_string(LEASE_PATH).unwrap(), "clean\n");
    let request = client::request(ParseOperation::ExtractDocument, ContentFormat::Text, 5);
    let mut session = RetiringLauncher.launch(&request, 64 * 1024 * 1024).unwrap();
    assert_eq!(std::fs::read_to_string(LEASE_PATH).unwrap(), "dirty\n");
    assert!(RetiringLauncher.launch(&request, 64 * 1024 * 1024).is_err());
    let file = transport::snapshot(&mut &b"hello"[..]).unwrap();
    let response = session.exchange(&request, &[file]).unwrap();
    assert!(matches!(response.result, ParseResult::Complete { .. }));
    // A valid result alone must not free the host budget or permit reuse.
    assert_eq!(std::fs::read_to_string(LEASE_PATH).unwrap(), "dirty\n");
    assert!(RetiringLauncher.launch(&request, 64 * 1024 * 1024).is_err());
    drop(session);
    let deadline = Instant::now() + Duration::from_secs(45);
    while std::fs::read_to_string(LEASE_PATH).unwrap() != "clean\n" {
        assert!(Instant::now() < deadline, "unit/cgroup retirement was not proven");
        std::thread::sleep(Duration::from_millis(50));
    }
    // Cancellation before a snapshot also waits for the actual worker to exit.
    let session = RetiringLauncher.launch(&request, 64 * 1024 * 1024).unwrap();
    drop(session);
    let deadline = Instant::now() + Duration::from_secs(45);
    while std::fs::read_to_string(LEASE_PATH).unwrap() != "clean\n" {
        assert!(Instant::now() < deadline, "dropped session did not retire");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(feature = "engine")]
#[test]
#[ignore = "requires root, systemd and installed Package E units/binary"]
fn iso_root_systemd_malicious_inputs_return_bounded_errors_and_recover() {
    let _guard = ROOT_LOCK.lock().unwrap();
    use std::io::{Cursor, Write};
    require_root();
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("bomb.txt", zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)).unwrap();
    zip.write_all(&vec![0; 1024 * 1024]).unwrap();
    let bomb = zip.finish().unwrap().into_inner();
    assert_eq!(client::inspect_archive_bytes(&bomb), Err(ParseError::LimitExceeded));
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
    zip.write_all(format!("{}<w:t>x</w:t>{}", "<x>".repeat(40), "</x>".repeat(40)).as_bytes()).unwrap();
    let deep = zip.finish().unwrap().into_inner();
    assert_eq!(client::extract_bytes(&deep, ContentFormat::OfficeDocument), Err(ParseError::LimitExceeded));
    let cfb = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1, 0, 0];
    assert_eq!(client::extract_bytes(&cfb, ContentFormat::Spreadsheet), Err(ParseError::InvalidContent));
    assert_eq!(client::extract_bytes(&vec![b'x'; MAX_RESULT_TEXT_BYTES + 1], ContentFormat::Text),
        Err(ParseError::LimitExceeded));
    assert_eq!(client::extract_bytes(b"still alive", ContentFormat::Text).unwrap(), "still alive");
}

fn systemctl(args: &[&str]) -> String {
    let output = std::process::Command::new("systemctl").args(args).output().unwrap();
    assert!(output.status.success(), "systemctl failed: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

fn worker_units() -> Vec<String> {
    systemctl(&["list-units", "--all", "--plain", "--no-legend", "duduclaw-parse@*.service"])
        .lines().filter_map(|line| line.split_whitespace().next().map(str::to_owned)).collect()
}

fn property(unit: &str, key: &str) -> String {
    systemctl(&["show", unit, "--property", key, "--value"]).trim().to_owned()
}

fn await_worker(previous: &[String]) -> (String, u32) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        for unit in worker_units() {
            if previous.contains(&unit) { continue; }
            if let Ok(pid) = property(&unit, "MainPID").parse::<u32>() {
                if pid == 0 { continue; }
                let exec_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                let exe_path = format!("/proc/{pid}/exe");
                while std::time::Instant::now() < exec_deadline {
                    match std::fs::read_link(&exe_path) {
                        Ok(exe) if exe.file_name() == Some(std::ffi::OsStr::new("duduclaw-parse-worker")) => {
                            return (unit, pid);
                        }
                        Ok(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            panic!("worker process {pid} exited before execing duduclaw-parse-worker: {error}");
                        }
                        Err(error) => panic!("cannot inspect {exe_path} while waiting for worker exec: {error}"),
                    }
                }
                panic!("worker process {pid} did not exec duduclaw-parse-worker within 5 seconds");
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("socket activation produced no live worker");
}

fn await_cleanup(unit: &str, pid: u32, cgroup: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
    while std::time::Instant::now() < deadline {
        let state = property(unit, "ActiveState");
        let populated = std::fs::read_to_string(format!("/sys/fs/cgroup{cgroup}/cgroup.events"))
            .map(|text| text.lines().any(|line| line == "populated 1")).unwrap_or(false);
        if matches!(state.as_str(), "inactive" | "failed") && !populated
            && !std::path::Path::new(&format!("/proc/{pid}")).exists() { return; }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("worker unit/cgroup/process did not retire");
}

// Mount options before " - " describe this mount, unlike the superblock
// options after it: a read-only bind can have a writable backing superblock.
fn check_mountinfo(mountinfo: &str) -> Result<(), String> {
    let mut saw_sys = false;
    for line in mountinfo.lines() {
        let (mount, filesystem) = line.split_once(" - ").ok_or_else(|| format!("invalid mountinfo: {line}"))?;
        let fields: Vec<_> = mount.split_whitespace().collect();
        let fs_fields: Vec<_> = filesystem.split_whitespace().collect();
        if fields.len() < 6 || fs_fields.len() < 3 {
            return Err(format!("invalid mountinfo: {line}"));
        }
        let destination = fields[4];
        let read_only = fields[5].split(',').any(|option| option == "ro");
        let api_filesystem = matches!(fs_fields[0], "tmpfs" | "proc" | "sysfs" | "devpts"
            | "mqueue" | "hugetlbfs" | "securityfs" | "cgroup2" | "bpf" | "pstore"
            | "debugfs" | "tracefs" | "fusectl" | "configfs" | "efivarfs");
        if !api_filesystem && !read_only {
            return Err(format!("writable host filesystem: {line}"));
        }
        saw_sys |= destination == "/sys";
        if (destination == "/sys" || destination.starts_with("/sys/")) && !read_only {
            return Err(format!("writable /sys projection: {line}"));
        }
    }
    if !saw_sys { return Err("missing /sys projection".to_owned()); }
    Ok(())
}

#[test]
fn mountinfo_rejects_writable_disk_and_sys_mounts() {
    let sys = "1 0 0:1 / /sys ro,nosuid - sysfs sysfs ro\n";
    // Optional fields, escaped paths and rw superblocks must not confuse the
    // per-mount read-only check. Unknown filesystem types fail closed.
    let readonly = "2 1 8:1 / /usr\\040files ro,relatime shared:1 - ext4 /dev/root rw\n";
    assert!(check_mountinfo(&format!("{sys}{readonly}")).is_ok());
    for fs in ["ext4", "xfs", "btrfs", "vfat", "overlay", "unknownfs"] {
        assert!(check_mountinfo(&format!("{sys}2 1 8:1 / /var/tmp rw - {fs} source rw")).is_err());
    }
    assert!(check_mountinfo(&sys.replace("/sys ro,", "/sys rw,")).is_err());
    assert!(check_mountinfo(&format!("{sys}2 1 0:2 / /sys/fs/cgroup rw - cgroup2 cgroup rw")).is_err());
    assert!(check_mountinfo(&format!("{sys}malformed")).is_err());
    assert!(check_mountinfo("").is_err());
}

#[test]
#[ignore = "requires root, cgroup v2, systemd and installed Package E units/binary"]
fn iso_root_systemd_identity_namespaces_limits_and_disconnect_cleanup() {
    let _guard = ROOT_LOCK.lock().unwrap();
    require_root();
    // These host files must not leak into the worker's minimal runtime root.
    // Exclusive creation and Drop cleanup never overwrite operator data.
    struct Sentinel(std::path::PathBuf);
    impl Drop for Sentinel { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); } }
    let sentinels: Vec<_> = ["/usr/local", "/usr/lib/duduclaw"].into_iter().map(|directory| {
        let path = std::path::Path::new(directory).join(format!("parse-worker-sentinel-{}", std::process::id()));
        let mut file = std::fs::File::options().write(true).create_new(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, b"host-only sentinel").unwrap();
        Sentinel(path)
    }).collect();
    let previous = worker_units();
    let connection = duduclaw_parse_worker::transport::connect().unwrap();
    let (unit, pid) = await_worker(&previous);
    let cgroup = property(&unit, "ControlGroup");
    assert!(!cgroup.is_empty());
    assert_eq!(property(&unit, "DynamicUser"), "yes");
    assert_eq!(property(&unit, "RootDirectory"), "/usr/lib/duduclaw/parse-root");
    assert_eq!(property(&unit, "ProtectSystem"), "strict");
    assert_eq!(property(&unit, "PrivateNetwork"), "yes");
    assert_eq!(property(&unit, "NoNewPrivileges"), "yes");
    assert_eq!(property(&unit, "MemoryMax"), (512 * 1024 * 1024u64).to_string());
    assert_eq!(property(&unit, "TasksMax"), "1");
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let uid: u32 = status.lines().find_map(|line| line.strip_prefix("Uid:"))
        .unwrap().split_whitespace().next().unwrap().parse().unwrap();
    assert!((61184..=65519).contains(&uid));
    for key in ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"] {
        assert_eq!(status.lines().find_map(|line| line.strip_prefix(key)).unwrap().trim(), "0000000000000000");
    }
    for namespace in ["net", "mnt", "ipc"] {
        assert_ne!(std::fs::read_link(format!("/proc/{pid}/ns/{namespace}")).unwrap(),
            std::fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap());
    }
    let worker_root = std::path::PathBuf::from(format!("/proc/{pid}/root"));
    for sentinel in &sentinels {
        assert!(sentinel.0.is_file());
        let path = worker_root.join(sentinel.0.strip_prefix("/").unwrap());
        let error = std::fs::symlink_metadata(path).expect_err("host sentinel visible in worker root");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
    for hidden in ["etc", "etc/shadow", "etc/passwd", "home", "root", "var/lib/duduclaw",
        "run/dbus", "run/dbus/system_bus_socket", "run/docker.sock"] {
        let error = std::fs::symlink_metadata(worker_root.join(hidden)).expect_err(&format!("visible {hidden}"));
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "cannot inspect {hidden}: {error}");
    }
    // These must be real, usable mounts; permission errors must not masquerade
    // as an empty root. /tmp is a bind alias of this instance's bounded tmpfs.
    assert!(worker_root.join("usr/libexec/duduclaw/duduclaw-parse-worker").is_file());
    assert!(worker_root.join("proc/self").exists());
    assert!(worker_root.join("dev/null").exists());
    // systemd 249 keeps its empty mount-propagation endpoint even in a private
    // /run. Permit that exact skeleton and only the bounded tmp mountpoint
    // under /var, never host runtime files or sockets.
    for (directory, allowed) in [("var", "tmp"), ("run", "systemd"), ("run/systemd", "incoming")] {
        for entry in std::fs::read_dir(worker_root.join(directory)).unwrap() {
            let entry = entry.unwrap();
            assert_eq!(entry.file_name(), std::ffi::OsStr::new(allowed));
            assert!(entry.file_type().unwrap().is_dir());
        }
    }
    assert_eq!(std::fs::read_dir(worker_root.join("run/systemd/incoming")).unwrap().count(), 0);
    use std::os::unix::fs::MetadataExt;
    let scratch = std::fs::metadata(worker_root.join(".tmp")).unwrap();
    for suffix in ["tmp", "var/tmp"] {
        let tmp = std::fs::metadata(worker_root.join(suffix)).unwrap();
        assert_eq!((tmp.dev(), tmp.ino()), (scratch.dev(), scratch.ino()), "different /{suffix}");
    }
    let filesystem = |suffix: &str| {
        let path = std::ffi::CString::new(format!("/proc/{pid}/root/{suffix}")).unwrap();
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: the path is NUL-terminated, the output is writable, and a
        // successful statvfs initializes the structure before it is read.
        assert_eq!(unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) }, 0);
        unsafe { stat.assume_init() }
    };
    for suffix in ["", "usr", "run"] {
        assert_ne!(filesystem(suffix).f_flag & libc::ST_RDONLY, 0, "writable /{suffix}");
    }
    for suffix in ["tmp", "var/tmp"] {
        let stat = filesystem(suffix);
        assert_eq!(stat.f_blocks as u64 * stat.f_frsize as u64, 8 * 1024 * 1024);
        assert_eq!(stat.f_files, 1024);
        let flags = libc::ST_NOSUID | libc::ST_NODEV | libc::ST_NOEXEC;
        assert_eq!(stat.f_flag & flags, flags);
        assert_eq!(stat.f_flag & libc::ST_RDONLY, 0);
        let path = std::ffi::CString::new(format!("/proc/{pid}/root/{suffix}")).unwrap();
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: the path is NUL-terminated and stat is writable; success
        // initializes the structure before assume_init reads it.
        assert_eq!(unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { stat.assume_init() }.f_type, libc::TMPFS_MAGIC);
    }
    let mountinfo = std::fs::read_to_string(format!("/proc/{pid}/mountinfo")).unwrap();
    check_mountinfo(&mountinfo).unwrap();
    assert_eq!(std::fs::read_to_string(format!("/sys/fs/cgroup{cgroup}/memory.max")).unwrap().trim(),
        (512 * 1024 * 1024u64).to_string());
    assert_eq!(std::fs::read_to_string(format!("/sys/fs/cgroup{cgroup}/pids.max")).unwrap().trim(), "1");
    drop(connection);
    await_cleanup(&unit, pid, &cgroup);
    assert_eq!(client::extract_bytes(b"after disconnect", ContentFormat::Text).unwrap(), "after disconnect");
}

#[test]
#[ignore = "requires root, systemd and installed Package E units/binary; waits for 30s watchdog"]
fn iso_root_systemd_silent_client_deadline_and_cgroup_cleanup() {
    let _guard = ROOT_LOCK.lock().unwrap();
    require_root();
    let previous = worker_units();
    let connection = duduclaw_parse_worker::transport::connect().unwrap();
    let (unit, pid) = await_worker(&previous);
    let cgroup = property(&unit, "ControlGroup");
    // No request arrives. The production receive deadline must retire the whole
    // worker even while the client still owns its endpoint.
    await_cleanup(&unit, pid, &cgroup);
    drop(connection);
    assert_eq!(client::extract_bytes(b"after timeout", ContentFormat::Text).unwrap(), "after timeout");
}

#[cfg(feature = "engine")]
#[test]
#[ignore = "requires root, systemd and installed Package E units/binary with output-fd support"]
fn iso_root_systemd_image_artifacts_and_html_roundtrip() {
    use duduclaw_parse_worker::media_adapters;
    let _guard = ROOT_LOCK.lock().unwrap();
    require_root();
    let mut original = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(4, 4).write_to(&mut original, image::ImageFormat::Png).unwrap();
    // These APIs seal and hash the returned output fd before exposing bytes.
    let jpeg = media_adapters::resize_image(original.get_ref(), 2).unwrap();
    let resized = image::load_from_memory(&jpeg).unwrap();
    assert_eq!((resized.width(), resized.height()), (2, 2));
    let png = media_adapters::mask_image(original.get_ref(), &[[2, 2, u32::MAX, u32::MAX]], [255, 0, 0]).unwrap();
    let masked = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!(masked.get_pixel(3, 3).0, [255, 0, 0, 255]);
    assert_eq!(masked.get_pixel(1, 1).0, [0, 0, 0, 255]);
    let query = media_adapters::html_query("card", ".card", HtmlOutputFormat::Json).unwrap();
    let results = media_adapters::extract_html(b"<div class='card'><span>hello</span></div>", vec![query]).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "card");
    assert_eq!(results[0].1["children"][0]["text"], "hello");
    assert_eq!(media_adapters::search_html(b"<p class='result__snippet'>isolated search result</p>").unwrap(),
        "1. isolated search result");
    assert_eq!(client::extract_bytes(b"after media", ContentFormat::Text).unwrap(), "after media");
}

#[cfg(feature = "engine")]
#[test]
#[ignore = "requires root, systemd and installed Package E units/binary with archive output-fd support"]
fn iso_root_systemd_archive_expansion_roundtrip() {
    use std::io::{Cursor, Write};
    let _guard = ROOT_LOCK.lock().unwrap();
    require_root();
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [("a.txt", b"hello"), ("dir/b.txt", b"world")] {
        zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    let bytes = zip.finish().unwrap().into_inner();
    let pack = duduclaw_parse_worker::archive_adapter::extract_pack(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(pack.entries.len(), 2);
    assert_eq!(pack.entries[0].name.as_str(), "a.txt");
    assert_eq!(pack.entries[0].offset, 0);
    assert_eq!(pack.entries[1].name.as_str(), "dir/b.txt");
    assert_eq!(pack.entries[1].offset, 5);
    assert_eq!(pack.bytes, b"helloworld");
    assert_eq!(pack.legacy_encoding, None);
    assert_eq!(pack.junk_skipped, 0);
    assert_eq!(client::extract_bytes(b"after archive", ContentFormat::Text).unwrap(), "after archive");
}
