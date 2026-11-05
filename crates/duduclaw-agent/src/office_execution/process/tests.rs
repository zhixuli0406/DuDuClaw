use super::*;
use std::path::Path;

fn python(source: &str) -> Command {
    let mut command = Command::new(duduclaw_core::platform::python3_command());
    command.env_clear().args(["-I", "-c", source]);
    if let Some(path) = std::env::var_os("PATH") { command.env("PATH", path); }
    configure(&mut command, None).unwrap();
    command
}

async fn output(source: &str) -> Output {
    collect(ManagedChild::spawn(&mut python(source)).unwrap(), Duration::from_secs(10))
        .await.unwrap()
}

#[tokio::test]
async fn office_pipe_accepts_exact_cap_and_refuses_the_next_byte() {
    let exact = vec![b'x'; MAX_PIPE_BYTES];
    assert_eq!(read_bounded(exact.as_slice(), "stdout").await.unwrap(), exact);
    let over = vec![b'x'; MAX_PIPE_BYTES + 1];
    let error = read_bounded(over.as_slice(), "stderr").await.unwrap_err();
    assert!(error.to_string().contains("stderr exceeded 32768 bytes"));
}

#[tokio::test]
async fn office_collect_drains_both_pipes_and_preserves_nonzero_exit() {
    // Each pipe exceeds typical kernel pipe capacity, but stays below its cap.
    let result = output("import os, sys\nos.write(1, b'o' * 24576)\nos.write(2, b'e' * 24576)\nsys.exit(7)").await;
    assert_eq!(result.status.code(), Some(7));
    assert_eq!(result.stdout, vec![b'o'; 24576]);
    assert!(result.stderr.ends_with(&vec![b'e'; 24576]));
    assert!(result.stderr.len() <= MAX_PIPE_BYTES);
}

const TREE: &str = r#"import os, pathlib, subprocess, sys, time
child = subprocess.Popen([sys.executable, '-I', '-c', 'import time; time.sleep(60)'])
pathlib.Path(sys.argv[1]).write_text(str(child.pid))
mode = sys.argv[2]
if mode in ('stdout', 'stderr'):
    os.write(1 if mode == 'stdout' else 2, b'x' * 32769)
time.sleep(60)
"#;

fn tree(path: &Path, mode: &str) -> ManagedChild {
    let mut command = python(TREE);
    command.arg(path).arg(mode);
    ManagedChild::spawn(&mut command).unwrap()
}

async fn descendant(path: &Path) -> u32 {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path)
                && let Ok(pid) = text.trim().parse() {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("Python fixture starts its descendant")
}

fn still_running(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // A killed orphan can remain a zombie until PID 1 reaps it in CI.
        if stat.rsplit_once(") ").is_some_and(|(_, rest)| rest.starts_with("Z ")) {
            return false;
        }
    }
    duduclaw_core::platform::process_alive(pid)
}

async fn assert_terminated(pid: u32) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while still_running(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("the process-group descendant must be terminated");
}

#[tokio::test]
async fn office_stdout_and_stderr_overflow_each_kill_descendants() {
    for stream in ["stdout", "stderr"] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("descendant");
        let child = tree(&path, stream);
        let pid = descendant(&path).await;
        let error = collect(child, Duration::from_secs(10)).await.unwrap_err();
        assert!(error.to_string().contains(&format!("{stream} exceeded")), "{error}");
        assert_terminated(pid).await;
    }
}

#[tokio::test]
async fn office_timeout_kills_descendants_that_hold_the_pipes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("descendant");
    let child = tree(&path, "wait");
    let pid = descendant(&path).await;
    let error = collect(child, Duration::from_millis(100)).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_terminated(pid).await;
}

#[tokio::test]
async fn office_task_cancellation_kills_descendants() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("descendant");
    let child = tree(&path, "wait");
    let pid = descendant(&path).await;
    let task = tokio::spawn(collect(child, Duration::from_secs(60)));
    tokio::task::yield_now().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_terminated(pid).await;
}

#[tokio::test]
async fn office_child_inherits_resource_limits_and_owns_its_group() {
    let result = output(r#"import json, os, resource
names = ['AS', 'DATA', 'RSS', 'CPU', 'FSIZE', 'NPROC', 'NOFILE', 'CORE']
print(json.dumps({'limits': {name: resource.getrlimit(getattr(resource, 'RLIMIT_' + name)) for name in names}, 'pid': os.getpid(), 'pgid': os.getpgrp()}))
"#).await;
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let data: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(data["pid"], data["pgid"]);
    for (name, ceiling) in [("AS", 2 * 1024 * 1024 * 1024_u64), ("CPU", 120),
        ("FSIZE", 256 * 1024 * 1024), ("NPROC", 512), ("NOFILE", 256), ("CORE", 0)] {
        // Collection fixtures deliberately opt out of isolation; native sandbox
        // acceptance is exercised separately by isolation tests.
        #[cfg(target_os = "macos")]
        if name == "AS" { continue; }
        let pair = data["limits"][name].as_array().unwrap();
        let soft = pair[0].as_u64().expect("finite nonnegative soft limit");
        let hard = pair[1].as_u64().expect("finite nonnegative hard limit");
        assert!(soft <= ceiling && hard <= ceiling, "{name}: {pair:?}");
        assert_eq!(soft, hard, "child cannot raise its {name} limit");
    }
}

#[tokio::test]
async fn office_python_fallback_refuses_wrong_dependency_before_running_script() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("script.py");
    let marker = temp.path().join("ran");
    std::fs::write(&script, "import pathlib, sys\npathlib.Path(sys.argv[1]).write_text('ran')\n").unwrap();
    let source = format!("import importlib.metadata\nimportlib.metadata.version = lambda name: '0.0.0'\n{PYTHON_BOOTSTRAP}");
    let mut command = python(&source);
    command.arg(&script).arg(r#"["python-docx==1.2.0"]"#).arg(&marker);
    let result = collect(ManagedChild::spawn(&mut command).unwrap(), Duration::from_secs(10)).await.unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("expected 1.2.0, found 0.0.0"));
    assert!(!marker.exists());

    let source = format!("import importlib.metadata\nimportlib.metadata.version = lambda name: '1.2.0'\n{PYTHON_BOOTSTRAP}");
    let mut command = python(&source);
    command.arg(&script).arg(r#"["python-docx==1.2.0"]"#).arg(&marker);
    let result = collect(ManagedChild::spawn(&mut command).unwrap(), Duration::from_secs(10)).await.unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "ran");
}

#[tokio::test]
async fn office_conversion_inherits_bounded_pipes_instead_of_buffering_libreoffice() {
    use std::os::unix::fs::PermissionsExt;
    for skill in ["docx", "pptx", "xlsx"] {
        for stream in ["stdout", "stderr"] {
            let temp = tempfile::tempdir().unwrap();
            let script = temp.path().join("script.py");
            std::fs::write(&script, manifest::embedded_script(skill, "to_pdf").unwrap()).unwrap();
            let input = temp.path().join(format!("input.{skill}"));
            std::fs::write(&input, "fake document for the fake converter").unwrap();
            let converter = temp.path().join("soffice");
            let fd = if stream == "stdout" { 1 } else { 2 };
            std::fs::write(&converter, format!("#!/bin/sh\n/usr/bin/head -c 32769 /dev/zero >&{fd}\n")).unwrap();
            std::fs::set_permissions(&converter, std::fs::Permissions::from_mode(0o700)).unwrap();
            let mut command = python(PYTHON_BOOTSTRAP);
            command.arg(script).arg("[]").arg(input)
                .env("PATH", format!("{}:/usr/bin:/bin", temp.path().display()));
            let error = collect(ManagedChild::spawn(&mut command).unwrap(), Duration::from_secs(10))
                .await.unwrap_err();
            assert!(error.to_string().contains(&format!("{stream} exceeded")), "{skill}: {error}");
        }
    }
}

#[tokio::test]
async fn office_successful_leader_exit_terminates_descendants_retaining_pipes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("descendant");
    let source = TREE.replace("time.sleep(60)\n", "");
    let mut command = python(&source);
    command.arg(&path).arg("exit");
    let result = collect(ManagedChild::spawn(&mut command).unwrap(), Duration::from_secs(10)).await.unwrap();
    assert!(result.status.success());
    assert_terminated(descendant(&path).await).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn office_linux_address_space_limit_refuses_large_virtual_allocation() {
    let result = output("import mmap\ntry:\n    mmap.mmap(-1, 3 * 1024**3)\nexcept OSError:\n    print('limited')\nelse:\n    raise RuntimeError('address-space limit not enforced')").await;
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(result.stdout, b"limited\n");
}
