use super::*;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

fn prepare(home: &Path, agent: &str) -> PreparedOffice {
    let agent = home.join("agents").join(agent);
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("private.md"), "secret user content").unwrap();
    PreparedOffice::new(home, &agent, "pdf", "create",
        &["private.md".into(), "--out".into(), "out.pdf".into()]).unwrap()
}

fn fake_uv(home: &Path, fail: bool) -> PathBuf {
    let uv = home.join("fake-uv");
    let python = isolation::resolve_program(duduclaw_core::platform::python3_command()).unwrap();
    let body = format!(r#"#!{python}
import os, pathlib, sys
assert sys.argv[-1] == '--help'
assert sys.argv[-3] == '--script'
assert len(sys.argv) == 12
assert sorted(p.name for p in pathlib.Path.cwd().iterdir()) == ['script.py']
assert pathlib.Path(sys.argv[-2]).parent == pathlib.Path.cwd()
assert os.environ['HOME'] == os.environ['TMPDIR'] == str(pathlib.Path.cwd())
assert os.environ['UV_LINK_MODE'] == 'copy'
assert 'UV_OFFLINE' not in os.environ
base = pathlib.Path(os.environ['UV_CACHE_DIR'])
assert base.name == 'uv-base-v1'
(base / 'package.py').write_text('trusted')
(base / 'warm-cwd').write_text(str(pathlib.Path.cwd()))
sys.exit({code})
"#, python = python.display(), code = if fail { 9 } else { 0 });
    std::fs::write(&uv, body).unwrap();
    std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o700)).unwrap();
    uv
}

#[tokio::test]
async fn warm_has_no_user_input_and_parser_mutation_cannot_reach_next_agent() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let first = prepare(&home, "one");
    let uv = fake_uv(&home, false);
    first.seed_cache(&uv).await.unwrap();
    let base = first.base_cache().to_path_buf();
    let warm_cwd = std::fs::read_to_string(base.join("warm-cwd")).unwrap();
    assert!(!Path::new(&warm_cwd).exists(), "host warm workspace is disposed");
    assert!(first.cache_is_warm_at(&base).unwrap());
    assert!(!first.cache_is_warm().unwrap(), "legacy cache is separate");
    let first_cache = first.run_cache().to_path_buf();
    assert_ne!(std::fs::metadata(base.join("package.py")).unwrap().ino(),
        std::fs::metadata(first_cache.join("package.py")).unwrap().ino());
    std::fs::write(first_cache.join("package.py"), "poison").unwrap();
    std::fs::write(first_cache.join("covert-channel"), "secret user content").unwrap();
    std::fs::write(first.workspace().join("output.pdf"), "published output").unwrap();
    let published = first.publish().unwrap().unwrap();
    let second = prepare(&home, "two");
    assert_ne!(second.run_cache(), first_cache);
    // A successful host warm is reused; no parser can create its marker.
    second.seed_cache(&home.join("absent-uv")).await.unwrap();
    assert_eq!(std::fs::read_to_string(second.run_cache().join("package.py")).unwrap(), "trusted");
    assert!(!second.run_cache().join("covert-channel").exists());
    assert_eq!(std::fs::read_to_string(base.join("package.py")).unwrap(), "trusted");
    let second_cache = second.run_cache().to_path_buf();
    drop(first);
    assert!(!first_cache.exists());
    assert!(published.exists());
    drop(second);
    assert!(!second_cache.exists());
    assert!(base.exists());
}

#[tokio::test]
async fn failed_warm_never_marks_or_seeds_the_cache() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let prepared = prepare(&home, "one");
    let error = prepared.seed_cache(&fake_uv(&home, true)).await.unwrap_err();
    assert!(error.to_string().contains("warm failed"), "{error}");
    assert!(!prepared.cache_is_warm_at(prepared.base_cache()).unwrap());
    assert!(SafeDir::open(prepared.run_cache()).unwrap().list().unwrap().is_empty());
    let cache = prepared.run_cache().to_path_buf();
    drop(prepared);
    assert!(!cache.exists());
}

#[test]
fn cache_links_are_rebased_and_virtual_environments_are_not_copied() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let prepared = prepare(&home, "one");
    let base = prepared.base_cache();
    std::fs::create_dir_all(base.join("archive-v0/package")).unwrap();
    std::fs::create_dir(base.join("wheels-v6")).unwrap();
    std::fs::create_dir(base.join("environments-v2")).unwrap();
    std::fs::write(base.join("archive-v0/package/code.py"), "trusted").unwrap();
    symlink(base.join("archive-v0/package"), base.join("wheels-v6/absolute")).unwrap();
    symlink("../archive-v0/package", base.join("wheels-v6/relative")).unwrap();
    symlink("/usr/bin/python3", base.join("environments-v2/python")).unwrap();
    prepared.copy_base_cache().unwrap();
    assert!(!prepared.run_cache().join("environments-v2").exists());
    for link in ["absolute", "relative"] {
        let path = prepared.run_cache().join("wheels-v6").join(link);
        assert!(path.canonicalize().unwrap().starts_with(prepared.run_cache()));
        std::fs::write(path.join("code.py"), "poison").unwrap();
    }
    assert_eq!(std::fs::read_to_string(base.join("archive-v0/package/code.py")).unwrap(), "trusted");
}

#[test]
fn cache_copy_refuses_external_links_hardlinks_and_special_files() {
    for kind in ["symlink", "hardlink", "fifo"] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let prepared = prepare(&home, "one");
        let target = prepared.base_cache().join("bad");
        match kind {
            "symlink" => symlink(prepared.input(), &target).unwrap(),
            "hardlink" => std::fs::hard_link(prepared.input(), &target).unwrap(),
            _ => {
                let name = std::ffi::CString::new(target.to_str().unwrap()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
        }
        assert!(prepared.copy_base_cache().is_err(), "{kind}");
    }
}

#[test]
fn old_parser_writable_cache_and_markers_are_never_promoted() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let prepared = prepare(&home, "one");
    prepared.mark_cache_warm().unwrap();
    std::fs::write(prepared.cache().join("package.py"), "legacy poison").unwrap();
    assert!(!prepared.cache_is_warm_at(prepared.base_cache()).unwrap());
    prepared.copy_base_cache().unwrap();
    assert!(!prepared.run_cache().join("package.py").exists());
}

#[test]
fn disposal_repairs_hostile_directory_permissions_without_following_links() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let prepared = prepare(&home, "one");
    let workspace = prepared.workspace().to_path_buf();
    let cache = prepared.run_cache().to_path_buf();
    let nested = cache.join("locked");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join("secret"), "input copy").unwrap();
    let outside = home.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink(&outside, nested.join("outside-link")).unwrap();
    for path in [&nested, &cache, &workspace] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    drop(prepared);
    assert!(!workspace.exists());
    assert_eq!(std::fs::metadata(outside).unwrap().permissions().mode() & 0o777, 0o755);
}

#[tokio::test]
#[ignore = "requires uv and package-index access for a clean host warm; does not verify the OS sandbox"]
async fn real_uv_warms_then_runs_offline_from_disposable_cache() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let prepared = prepare(&home, "one");
    let uv = isolation::resolve_program("uv").unwrap();
    prepared.seed_cache(&uv).await.unwrap();
    let mut cmd = process::command(&prepared, tokio::process::Command::new(&uv));
    cmd.env("UV_OFFLINE", "1").env("UV_LINK_MODE", "copy")
        .args(["--no-config", "run", "--no-project", "--no-env-file", "--no-build",
            "--no-python-downloads", "--exclude-newer", manifest::EXCLUDE_NEWER, "--script"])
        .arg(prepared.script()).args(prepared.args());
    process::configure(&mut cmd, None).unwrap();
    let output = process::collect(duduclaw_core::process_tree::ManagedChild::spawn(&mut cmd).unwrap(),
        std::time::Duration::from_secs(180)).await.unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(prepared.publish().unwrap().unwrap().exists());
}
