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
                if pid != 0 { return (unit, pid); }
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

#[test]
#[ignore = "requires root, cgroup v2, systemd and installed Package E units/binary"]
fn iso_root_systemd_identity_namespaces_limits_and_disconnect_cleanup() {
    let _guard = ROOT_LOCK.lock().unwrap();
    require_root();
    let previous = worker_units();
    let connection = duduclaw_parse_worker::transport::connect().unwrap();
    let (unit, pid) = await_worker(&previous);
    let cgroup = property(&unit, "ControlGroup");
    assert!(!cgroup.is_empty());
    assert_eq!(property(&unit, "DynamicUser"), "yes");
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
    for hidden in ["etc/shadow", "var/lib/duduclaw", "run/dbus/system_bus_socket", "run/docker.sock"] {
        assert!(!std::path::Path::new(&format!("/proc/{pid}/root/{hidden}")).exists(), "visible {hidden}");
    }
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
