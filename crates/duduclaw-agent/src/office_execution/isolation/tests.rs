use super::*;

#[test]
fn every_missing_capability_refuses_without_explicit_opt_out() {
    for bits in 0..8 {
        let caps = Capabilities { filesystem: bits & 1 != 0, network: bits & 2 != 0, memory: bits & 4 != 0 };
        assert_eq!(decide(caps, false), if bits == 7 { Decision::Isolated } else { Decision::Refuse });
        assert_eq!(decide(caps, true), if bits == 7 { Decision::Isolated } else { Decision::Unsandboxed });
    }
}

#[test]
fn only_the_existing_script_escape_hatch_opts_out() {
    assert!(opt_out("[container.sandbox]\nscript_when_unavailable = 'run_unsandboxed'"));
    // Matches the existing script policy even if another sandbox setting is invalid.
    assert!(opt_out("[container.sandbox]\nscript_when_unavailable = 'run_unsandboxed'\npids = 0"));
    for config in ["", "broken [", "container = false", "[container]\nsandbox = false",
        "[container]\nscript_when_unavailable = 'run_unsandboxed'",
        "[container]\nsandbox_enabled = false",
        "[container.sandbox]\nwhen_unavailable = 'run_unsandboxed'",
        "[container.sandbox]\nscript_when_unavailable = true",
        "[container.sandbox]\nscript_when_unavailable = 'RUN_UNSANDBOXED'",
        "[container.sandbox]\nscript_when_unavailable = 'fail'"] {
        assert!(!opt_out(config), "{config}");
    }
}

#[test]
fn profile_paths_cannot_inject_rules() {
    let injection = Path::new("/private/work/\") (allow default) ; \\path");
    let quoted = quote_path(injection).unwrap();
    assert_eq!(quoted, "\"/private/work/\\\") (allow default) ; \\\\path\"");
    // Decode the SBPL string independently; a quote must only occur at its end.
    let mut chars = quoted.chars();
    assert_eq!(chars.next(), Some('"'));
    let mut decoded = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => decoded.push(chars.next().unwrap()),
            '"' => { assert!(chars.next().is_none()); break; }
            _ => decoded.push(ch),
        }
    }
    assert_eq!(decoded, injection.to_str().unwrap());
    for path in ["relative", "/tmp/../home", "/tmp/line\nbreak", "/tmp/tab\t", "/tmp/\0", "/tmp/\r"] {
        assert!(quote_path(Path::new(path)).is_err(), "{path:?}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        assert!(quote_path(Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff"))).is_err());
    }
}

#[test]
fn profile_denies_default_and_network_and_scopes_reads_and_writes() {
    let profile = macos_profile(&builder_paths("/Users/user/.duduclaw")).unwrap();
    assert!(profile.starts_with("(version 1)\n(deny default)\n(deny network*)\n"));
    assert!(!profile.contains("(allow network"));
    assert!(!profile.contains("(subpath \"/\")"));
    assert!(profile.contains("(allow file-read* (literal \"/Users/user/.local/bin/uv\"))"));
    assert!(!profile.contains("(subpath \"/Users"));
    let writes: Vec<_> = profile.lines().filter(|line| line.starts_with("(allow file-write")).collect();
    assert_eq!(writes.len(), 1);
    assert!(writes[0].contains("(subpath \"/private/work\")"));
    assert!(writes[0].contains("(require-not (literal \"/private/work/input.pdf\"))"));
    assert!(writes[0].contains("(require-not (literal \"/private/work/script.py\"))"));

}

#[cfg(unix)]
fn fixture() -> (tempfile::TempDir, PreparedOffice) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let agent = home.join("agents/bot");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("input.md"), "test").unwrap();
    let prepared = PreparedOffice::new(&home, &agent, "pdf", "create",
        &["input.md".into(), "--out".into(), "output.pdf".into()]).unwrap();
    (temp, prepared)
}

#[cfg(unix)]
#[tokio::test]
async fn native_probe_never_silently_downgrades_and_rereads_operator_policy() {
    let (_temp, prepared) = fixture();
    let program = resolve_program(duduclaw_core::platform::python3_command()).unwrap();
    match select(&prepared, &program).await {
        Ok(sandbox) => assert!(sandbox.is_isolated()),
        Err(error) => {
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains("full isolation unavailable"));
            eprintln!("Native default refusal verified: {error}");
            std::fs::write(prepared.home.join("config.toml"),
                "[container.sandbox]\nscript_when_unavailable = 'run_unsandboxed'").unwrap();
            assert!(!select(&prepared, &program).await.unwrap().is_isolated());
            std::fs::write(prepared.home.join("config.toml"), "invalid [").unwrap();
            assert!(select(&prepared, &program).await.is_err());
        }
    }
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires native sandbox support; run explicitly for platform acceptance"]
async fn native_sandbox_denies_host_read_write_and_network() {
    let (_temp, prepared) = fixture();
    let program = resolve_program(duduclaw_core::platform::python3_command()).unwrap();
    let sandbox = select(&prepared, &program).await.expect("full isolation must be available");
    assert!(sandbox.is_isolated());
    let outside = prepared.home.join("private-secret");
    std::fs::write(&outside, "secret").unwrap();
    std::fs::write(prepared.base_cache().join("package.py"), "trusted").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut cmd = sandbox.command(&prepared, &program).unwrap();
    cmd.args(["-I", "-c", r#"import pathlib, socket, sys
outside, work, original, base, cache, port = sys.argv[1:]
assert pathlib.Path(base, "package.py").read_text() == "trusted"
pathlib.Path(cache, "package.py").write_text("per-run mutation")
assert pathlib.Path(original).read_text() == 'test'
pathlib.Path(work, 'allowed-output').write_text('ok')
for operation in (lambda: pathlib.Path(outside).read_text(),
                  lambda: pathlib.Path(outside + '-write').write_text('bad'),
                  lambda: pathlib.Path(original).write_text('bad'),
                  lambda: pathlib.Path(base, "package.py").write_text("poison"),
                  lambda: pathlib.Path(base, "channel").write_text("secret"),
                  lambda: socket.create_connection(('127.0.0.1', int(port)), timeout=1)):
    try:
        operation()
    except OSError:
        pass
    else:
        raise RuntimeError('sandbox escape succeeded')
"#]).arg(&outside).arg(prepared.workspace()).arg(prepared.input())
        .arg(prepared.base_cache()).arg(prepared.run_cache())
        .arg(listener.local_addr().unwrap().port().to_string());
    probe(&prepared, cmd, sandbox.memory()).await.unwrap();
    assert!(!outside.with_file_name("private-secret-write").exists());
    assert_eq!(std::fs::read_to_string(prepared.base_cache().join("package.py")).unwrap(), "trusted");
    assert!(!prepared.base_cache().join("channel").exists());
}

#[cfg(unix)]
#[test]
fn selected_memory_backend_failure_never_falls_back() {
    #[cfg(target_os = "macos")]
    let backends = [MemoryLimit::Data, MemoryLimit::Rss];
    #[cfg(not(target_os = "macos"))]
    let backends = [MemoryLimit::AddressSpace];
    for backend in backends {
        let mut calls = 0;
        let result = backend.install(|_| {
            calls += 1;
            Err(io::Error::from_raw_os_error(libc::EINVAL))
        });
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EINVAL));
        assert_eq!(calls, 1, "a failed selected limit must not try another resource");
    }
}

#[cfg(unix)]
#[test]
fn linux_command_requires_namespaces_and_never_mounts_host_root() {
    let (_temp, prepared) = fixture();
    let sandbox = Sandbox { wrapper: Some(PathBuf::from("/usr/bin/bwrap")), memory: None, profile: None };
    let program = Path::new("/usr/bin/python3");
    let cmd = sandbox.command(&prepared, program).unwrap();
    let args: Vec<_> = cmd.as_std().get_args().map(|v| v.to_string_lossy().into_owned()).collect();
    for flag in ["--unshare-all", "--unshare-net", "--die-with-parent", "--new-session"] {
        assert!(args.iter().any(|arg| arg == flag));
    }
    assert!(!args.iter().any(|arg| arg == "--share-net" || arg == "--not-a-security-boundary"));
    for pair in args.windows(3) {
        if pair[0] == "--bind" {
            assert!(Path::new(&pair[1]) == prepared.workspace());
            assert_eq!(pair[1], pair[2]);
        }
        if pair[0] == "--bind" || pair[0] == "--ro-bind" {
            assert_ne!(pair[1], "/");
            assert_ne!(Path::new(&pair[1]), prepared.home);
        }
    }
    for path in [prepared.script(), prepared.input(), prepared.base_cache()] {
        assert!(args.windows(3).any(|triple| triple[0] == "--ro-bind"
            && Path::new(&triple[1]) == path && triple[1] == triple[2]));
    }
    assert_eq!(args[args.len() - 2], "--");
    assert_eq!(Path::new(&args[args.len() - 1]), program.canonicalize().unwrap());
}

// Pure builder fixtures represent canonical paths, independent of host OS/layout.
fn builder_paths(home: &str) -> SandboxPaths {
    SandboxPaths {
        home: home.into(), workspace: "/private/work".into(), base_cache: "/private/base-cache".into(),
        run_cache: "/private/work/uv-cache".into(),
        script: "/private/work/script.py".into(), input: "/private/work/input.pdf".into(),
        program: "/Users/user/.local/bin/uv".into(),
        roots: ["/usr", "/opt/homebrew"].into_iter().map(|p| (p.into(), p.into())).collect(),
    }
}

fn assert_builders_refuse(paths: &SandboxPaths) {
    for error in [bwrap_args(paths).unwrap_err(), macos_profile(paths).unwrap_err()] {
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("Office isolation path overlap"));
    }
}

#[test]
fn pure_builders_grant_writes_only_to_the_disposable_workspace() {
    let paths = builder_paths("/Users/user/.duduclaw");
    let args = bwrap_args(&paths).unwrap();
    let mounts: Vec<_> = args.windows(3).filter(|a| a[0] == "--bind").collect();
    assert_eq!(mounts.len(), 1);
    assert_eq!(Path::new(&mounts[0][1]), paths.workspace);
    assert_eq!(mounts[0][1], mounts[0][2]);
    assert!(args.windows(3).any(|a| a[0] == "--ro-bind"
        && Path::new(&a[1]) == paths.base_cache && a[1] == a[2]));
    let profile = macos_profile(&paths).unwrap();
    let writes: Vec<_> = profile.lines().filter(|line| line.contains("(allow file-write")).collect();
    assert_eq!(writes.len(), 1);
    assert!(writes[0].contains(&format!("(subpath {})", quote_path(&paths.workspace).unwrap())));
    assert!(!writes[0].contains(paths.base_cache.to_str().unwrap()));
    assert!(profile.contains(&format!("(allow file-read* (subpath {}))", quote_path(&paths.base_cache).unwrap())));
}

#[test]
fn both_builders_refuse_base_workspace_overlap_and_relocated_run_cache() {
    for base in ["/private/work", "/private/work/base", "/private"] {
        let mut paths = builder_paths("/Users/user/.duduclaw");
        paths.base_cache = base.into();
        assert_builders_refuse(&paths);
    }
    for cache in ["/private/work", "/private", "/private/shared", "/private/work/script.py", "/private/work/input.pdf"] {
        let mut paths = builder_paths("/Users/user/.duduclaw");
        paths.run_cache = cache.into();
        assert_builders_refuse(&paths);
    }
}

#[test]
fn both_builders_refuse_custom_home_under_runtime_roots() {
    for home in ["/usr/local/share/duduclaw", "/opt/homebrew/var/duduclaw",
        "/usr", "/opt/homebrew", "/", "/opt"] {
        assert_builders_refuse(&builder_paths(home));
    }
    // Path-component comparison must not confuse sibling prefixes.
    for home in ["/usr-private/duduclaw", "/opt/homebrew-private/duduclaw"] {
        let paths = builder_paths(home);
        assert!(bwrap_args(&paths).is_ok());
        assert!(macos_profile(&paths).is_ok());
    }
}

#[test]
fn both_builders_refuse_private_grants_overlapping_runtime_roots() {
    for private in ["/usr/local/private", "/opt/homebrew/private", "/usr", "/opt"] {
        for field in 0..5 {
            let mut paths = builder_paths("/Users/user/.duduclaw");
            match field {
                0 => paths.workspace = private.into(), // Includes staged output.
                1 => paths.base_cache = private.into(),
                4 => paths.run_cache = private.into(),
                2 => paths.input = private.into(),
                _ => paths.script = private.into(),
            }
            assert_builders_refuse(&paths);
        }
    }
}

#[test]
fn both_builders_check_runtime_alias_and_canonical_target() {
    let mut paths = builder_paths("/srv/duduclaw");
    paths.roots = vec![("/opt/runtime".into(), "/srv".into())];
    assert_builders_refuse(&paths);
    paths.roots = vec![("/srv".into(), "/opt/runtime".into())];
    assert_builders_refuse(&paths);
    paths.roots = vec![("/opt/runtime".into(), "/srv/duduclaw/runtime".into())];
    assert_builders_refuse(&paths);
}

#[cfg(unix)]
#[test]
fn resolver_canonicalizes_home_private_paths_and_runtime_roots() {
    use std::os::unix::fs::symlink;
    let (temp, mut prepared) = fixture();
    let alias = temp.path().join("home-alias");
    symlink(&prepared.home, &alias).unwrap();
    prepared.home = alias;
    let runtime = temp.path().join("runtime-alias");
    symlink(&prepared.home, &runtime).unwrap();
    let paths = SandboxPaths::resolve(&prepared, Path::new("/usr/bin/true"),
        &[runtime.to_str().unwrap()]).unwrap();
    assert_eq!(paths.home, temp.path().canonicalize().unwrap());
    assert_eq!(paths.workspace, prepared.workspace().canonicalize().unwrap());
    assert_eq!(paths.base_cache, prepared.base_cache().canonicalize().unwrap());
    assert_eq!(paths.run_cache, prepared.run_cache().canonicalize().unwrap());
    assert_eq!(paths.input, prepared.input().canonicalize().unwrap());
    assert_eq!(paths.roots[0].1, paths.home);
    assert_builders_refuse(&paths);
    let dangling = temp.path().join("dangling");
    symlink(temp.path().join("absent"), &dangling).unwrap();
    assert!(SandboxPaths::resolve(&prepared, Path::new("/usr/bin/true"),
        &[dangling.to_str().unwrap()]).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn path_overlap_preflight_failure_uses_existing_refusal() {
    let (_temp, mut prepared) = fixture();
    // A conservative reverse overlap does not need real writes beneath /usr.
    prepared.home = PathBuf::from("/");
    // Test policy integration without ever running the untrusted document.
    let error = select(&prepared, Path::new("/usr/bin/true")).await.err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("full isolation unavailable"));
    assert!(error.to_string().contains("path overlap"));
}

#[cfg(unix)]
#[tokio::test]
async fn canonicalization_failure_only_downgrades_with_explicit_opt_out() {
    let (_temp, prepared) = fixture();
    let absent_program = prepared.home.join("missing-runtime");
    let error = select(&prepared, &absent_program).await.err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("full isolation unavailable"));
    std::fs::write(prepared.home.join("config.toml"),
        "[container.sandbox]\nscript_when_unavailable = 'run_unsandboxed'").unwrap();
    assert!(!select(&prepared, &absent_program).await.unwrap().is_isolated());
}

#[test]
fn production_runtime_allowlists_protect_custom_homes() {
    let mut paths = builder_paths("/usr/local/share/duduclaw");
    for roots in [LINUX_RUNTIME_ROOTS, MACOS_RUNTIME_ROOTS] {
        paths.roots = roots.iter().map(|p| (PathBuf::from(p), PathBuf::from(p))).collect();
        assert_builders_refuse(&paths);
    }
    paths.home = "/opt/homebrew/var/duduclaw".into();
    paths.roots = MACOS_RUNTIME_ROOTS.iter().map(|p| (PathBuf::from(p), PathBuf::from(p))).collect();
    assert_builders_refuse(&paths);
    // Linux grants no /opt/homebrew subtree. A home there is safe with the
    // production Linux allowlist; an added runtime grant must trigger refusal.
    paths.roots = LINUX_RUNTIME_ROOTS.iter().map(|p| (PathBuf::from(p), PathBuf::from(p))).collect();
    assert!(bwrap_args(&paths).is_ok());
    paths.roots.push(("/opt/homebrew".into(), "/opt/homebrew".into()));
    assert_builders_refuse(&paths);
}
