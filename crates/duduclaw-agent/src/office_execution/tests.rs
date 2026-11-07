use super::*;

fn strings(args: &[&str]) -> Vec<String> { args.iter().map(|s| s.to_string()).collect() }

#[test]
fn office_manifest_covers_exactly_all_embedded_scripts() {
    let mut count = 0;
    for (skill, files) in crate::builtin_skills::BUILTIN_SKILL_FILES {
        for (path, body) in *files {
            if path.starts_with("scripts/") {
                manifest::verify_script(skill, path, body.as_bytes()).unwrap();
                let metadata = body.lines().find(|line| line.starts_with("# dependencies = ")).unwrap();
                let value: toml::Value = toml::from_str(metadata.strip_prefix("# ").unwrap()).unwrap();
                for dependency in value["dependencies"].as_array().unwrap() {
                    let pin = dependency.as_str().unwrap();
                    let (name, version) = pin.split_once("==").expect("every direct dependency is pinned");
                    assert!(!name.is_empty() && !version.is_empty());
                    assert!(version.bytes().all(|c| c.is_ascii_digit() || c == b'.'));
                }
                assert!(manifest::verify_script(skill, path, b"print('tampered')").is_err());
                count += 1;
            }
        }
    }
    assert_eq!(count, manifest::MANIFEST.len());
    assert_eq!(manifest::EXCLUDE_NEWER, "2026-03-01T00:00:00Z");
    assert!(manifest::embedded_script("pdf", "to_pdf").is_err());
    assert!(manifest::embedded_script("pdf", "../create").is_err());
}

#[test]
fn office_argument_schema_rejects_inline_unknown_duplicate_and_extra_arguments() {
    for args in [
        vec!["in.md", "--out=/abs/out.pdf"],
        vec!["in.md", "--out", "out.pdf", "--format=md"],
        vec!["in.md", "--out", "out.pdf", "--out", "other.pdf"],
        vec!["in.md", "--out", "out.pdf", "--format", "json"],
        vec!["in.md", "--out", "out.pdf", "--output", "evil.pdf"],
        vec!["in.md", "--out", "out.pdf", "another.md"],
        vec!["in.md", "--out", "--format"],
        vec!["in.md"],
    ] {
        assert!(schema::parse("pdf", "create", &strings(&args)).is_err(), "{args:?}");
    }
    for (skill, formats) in [("pdf", vec!["md", "text"]), ("docx", vec!["md", "json"]),
        ("pptx", vec!["md", "json"]), ("xlsx", vec!["csv", "json"])] {
        for format in formats {
            schema::parse(skill, "create", &strings(&["in", "--out", "out", "--format", format])).unwrap();
        }
        schema::parse(skill, "extract", &strings(&["in", "--format", "json"])).unwrap();
        assert!(schema::parse(skill, "extract", &strings(&["in", "--out", "out"])).is_err());
    }
    for skill in ["docx", "xlsx", "pptx"] {
        schema::parse(skill, "to_pdf", &strings(&["in", "--outdir", "out"])).unwrap();
        assert!(schema::parse(skill, "to_pdf", &strings(&["in", "--format", "md"])).is_err());
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    struct Fixture { _temp: tempfile::TempDir, home: PathBuf, agent: PathBuf }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().canonicalize().unwrap();
            let agent = home.join("agents/bot");
            std::fs::create_dir_all(&agent).unwrap();
            std::fs::create_dir_all(home.join("attachments")).unwrap();
            std::fs::write(agent.join("in.md"), "# safe input").unwrap();
            Self { _temp: temp, home, agent }
        }
        fn create(&self, args: &[&str]) -> io::Result<PreparedOffice> {
            PreparedOffice::new(&self.home, &self.agent, "pdf", "create", &strings(args))
        }
    }

    #[test]
    fn office_employee_and_company_scripts_cannot_execute() {
        let fixture = Fixture::new();
        let marker = fixture.home.join("EMPLOYEE_CODE_EXECUTED");
        for layer in [fixture.agent.join("SKILLS"), fixture.home.join("skills")] {
            let dir = layer.join("xlsx/scripts");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("extract.py"), format!("open({:?}, 'w').write('bad')", marker)).unwrap();
            std::fs::write(dir.parent().unwrap().join("SKILL.md"), "# Employee prompt").unwrap();
        }
        std::fs::write(fixture.agent.join("in.csv"), "name,value\ntrusted,42\n").unwrap();
        // An employee module must not shadow even stdlib imports.
        std::fs::write(fixture.agent.join("csv.py"), format!("open({:?}, 'w').write('bad')", marker)).unwrap();
        let prepared = PreparedOffice::new(&fixture.home, &fixture.agent, "xlsx", "extract", &strings(&["in.csv"])).unwrap();
        assert_eq!(std::fs::read(prepared.script()).unwrap(), manifest::embedded_script("xlsx", "extract").unwrap());
        // CSV extraction needs only Python's standard library: no network or pip/uv install.
        let result = std::process::Command::new(duduclaw_core::platform::python3_command())
            .arg("-I").arg(prepared.script()).args(prepared.args())
            .current_dir(prepared.workspace()).env_clear().output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        assert!(String::from_utf8_lossy(&result.stdout).contains("trusted"));
        assert!(!marker.exists());
        assert!(prepared.publish().unwrap().is_none());
        assert_eq!(std::fs::metadata(prepared.script()).unwrap().permissions().mode() & 0o777, 0o500);
        assert_eq!(std::fs::metadata(prepared.workspace()).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn office_private_cache_is_reused_but_symlinked_cache_is_refused() {
        let f = Fixture::new();
        let p = f.create(&["in.md", "--out", "one.pdf"]).unwrap();
        assert!(!p.cache_is_warm().unwrap());
        p.mark_cache_warm().unwrap();
        assert!(p.cache_is_warm().unwrap());
        let cache = p.cache().to_path_buf();
        std::fs::write(cache.join("cached-package"), "cache").unwrap();
        drop(p);
        let p = f.create(&["in.md", "--out", "two.pdf"]).unwrap();
        assert_eq!(p.cache(), cache);
        assert!(p.cache_is_warm().unwrap());
        assert!(cache.join("cached-package").exists());
        drop(p);
        std::fs::rename(&cache, cache.with_file_name("old-cache")).unwrap();
        symlink(f.home.join("attachments"), &cache).unwrap();
        assert!(f.create(&["in.md", "--out", "three.pdf"]).is_err());
    }

    #[test]
    fn office_spawn_rechecks_script_bytes_links_and_permissions() {
        for attack in ["bytes", "symlink", "hardlink", "mode"] {
            let f = Fixture::new();
            let p = f.create(&["in.md", "--out", "out.pdf"]).unwrap();
            assert!(p.verify_before_spawn().is_ok());
            assert_eq!(p.dependency_pins().unwrap(), vec!["reportlab==4.4.10"]);
            match attack {
                "bytes" => {
                    let mut bytes = std::fs::read(p.script()).unwrap();
                    let last = bytes.len() - 1;
                    bytes[last] ^= 1; // Equal length, valid owner/mode: hash must catch it.
                    std::fs::set_permissions(p.script(), std::fs::Permissions::from_mode(0o700)).unwrap();
                    std::fs::write(p.script(), bytes).unwrap();
                    std::fs::set_permissions(p.script(), std::fs::Permissions::from_mode(0o500)).unwrap();
                }
                "symlink" => {
                    let saved = p.workspace().join("saved.py");
                    std::fs::rename(p.script(), &saved).unwrap();
                    symlink(saved, p.script()).unwrap();
                }
                "hardlink" => { std::fs::hard_link(p.script(), p.workspace().join("alias.py")).unwrap(); }
                "mode" => { std::fs::set_permissions(p.script(), std::fs::Permissions::from_mode(0o700)).unwrap(); }
                _ => unreachable!(),
            }
            assert!(p.verify_before_spawn().is_err(), "{attack}");
        }
    }

    #[test]
    fn office_spawn_rechecks_private_directories() {
        for target in ["root", "cache", "base_cache", "run_cache", "workspace"] {
            for symlink_attack in [false, true] {
                let f = Fixture::new();
                let p = f.create(&["in.md", "--out", "out.pdf"]).unwrap();
                let target = match target {
                    "root" => p.cache().parent().unwrap(),
                    "cache" => p.cache(),
                    "base_cache" => p.base_cache(),
                    "run_cache" => p.run_cache(),
                    _ => p.workspace(),
                };
                if symlink_attack {
                    let old = target.with_file_name(format!("old-{}", target.file_name().unwrap().to_str().unwrap()));
                    std::fs::rename(target, &old).unwrap();
                    symlink(&old, target).unwrap();
                    assert!(p.verify_before_spawn().is_err());
                    std::fs::remove_file(target).unwrap();
                    std::fs::rename(old, target).unwrap();
                } else {
                    std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755)).unwrap();
                    assert!(p.verify_before_spawn().is_err());
                }
            }
        }
    }

    #[test]
    fn office_oversized_output_is_refused_without_partial_destination() {
        let f = Fixture::new();
        let p = f.create(&["in.md", "--out", "out.pdf"]).unwrap();
        std::fs::File::create(p.workspace().join("output.pdf")).unwrap().set_len(MAX_OUTPUT + 1).unwrap();
        assert!(p.publish().is_err());
        assert!(!f.agent.join("out.pdf").exists());
    }

    #[test]
    fn office_inputs_are_snapshotted_and_outputs_are_published_only_under_agent() {
        let f = Fixture::new();
        let p = f.create(&["in.md", "--out", "out.pdf"]).unwrap();
        std::fs::write(f.agent.join("in.md"), "changed after validation").unwrap();
        assert_eq!(std::fs::read_to_string(p.input()).unwrap(), "# safe input");
        assert!(!f.agent.join("out.pdf").exists());
        std::fs::write(p.workspace().join("output.pdf"), b"pdf-output").unwrap();
        assert_eq!(p.publish().unwrap().unwrap(), f.agent.join("out.pdf"));
        assert_eq!(std::fs::read(f.agent.join("out.pdf")).unwrap(), b"pdf-output");
        assert!(p.publish().is_err(), "never overwrite an existing output");
    }

    #[test]
    fn office_argument_paths_reject_escape_protected_and_shared_outputs() {
        let f = Fixture::new();
        for output in ["../victim.pdf", "/tmp/escape.pdf", "SOUL.md", "agent.toml", ".mcp.json", "x.py"] {
            assert!(f.create(&["in.md", "--out", output]).is_err(), "{output}");
        }
        let shared = f.home.join("attachments/out.pdf");
        assert!(f.create(&["in.md", "--out", shared.to_str().unwrap()]).is_err());
        assert!(f.create(&["in.md", "--out=/abs/out.pdf"]).is_err());
        assert!(f.create(&["../in.md", "--out", "out.pdf"]).is_err());
        std::fs::write(f.home.join("attachments/shared.md"), "shared input").unwrap();
        let shared_input = f.home.join("attachments/shared.md");
        let p = f.create(&[shared_input.to_str().unwrap(), "--out", "out.pdf"]).unwrap();
        assert_eq!(std::fs::read_to_string(p.input()).unwrap(), "shared input");
    }

    #[test]
    fn office_input_and_output_symlinks_and_hardlinks_are_refused() {
        let f = Fixture::new();
        symlink("in.md", f.agent.join("link.md")).unwrap();
        assert!(f.create(&["link.md", "--out", "out.pdf"]).is_err());
        std::fs::hard_link(f.agent.join("in.md"), f.agent.join("hard.md")).unwrap();
        assert!(f.create(&["hard.md", "--out", "out.pdf"]).is_err());
        std::fs::remove_file(f.agent.join("hard.md")).unwrap();
        symlink(f.home.join("attachments"), f.agent.join("linkdir")).unwrap();
        std::fs::write(f.home.join("attachments/in.md"), "outside").unwrap();
        assert!(f.create(&["linkdir/in.md", "--out", "out.pdf"]).is_err());
        assert!(f.create(&["in.md", "--out", "linkdir/out.pdf"]).is_err());
        symlink(f.home.join("victim.pdf"), f.agent.join("out.pdf")).unwrap();
        assert!(f.create(&["in.md", "--out", "out.pdf"]).is_err());
    }

    #[test]
    fn office_output_rewalk_refuses_directory_swap_and_late_symlink() {
        let f = Fixture::new();
        std::fs::create_dir(f.agent.join("out")).unwrap();
        let p = f.create(&["in.md", "--out", "out/report.pdf"]).unwrap();
        std::fs::write(p.workspace().join("output.pdf"), "safe").unwrap();
        std::fs::rename(f.agent.join("out"), f.agent.join("old-out")).unwrap();
        symlink(f.home.join("attachments"), f.agent.join("out")).unwrap();
        assert!(p.publish().is_err());
        assert!(!f.home.join("attachments/report.pdf").exists());
        let p = f.create(&["in.md", "--out", "late.pdf"]).unwrap();
        std::fs::write(p.workspace().join("output.pdf"), "safe").unwrap();
        symlink(f.home.join("victim.pdf"), f.agent.join("late.pdf")).unwrap();
        assert!(p.publish().is_err());
        assert!(!f.home.join("victim.pdf").exists());
    }

    #[test]
    fn office_execution_root_and_agent_root_must_not_be_symlinks() {
        let f = Fixture::new();
        symlink(f.home.join("attachments"), f.home.join(".office-execution")).unwrap();
        assert!(f.create(&["in.md", "--out", "out.pdf"]).is_err());
        std::fs::remove_file(f.home.join(".office-execution")).unwrap();
        std::fs::rename(&f.agent, f.home.join("moved-bot")).unwrap();
        symlink(f.home.join("moved-bot"), &f.agent).unwrap();
        assert!(f.create(&["in.md", "--out", "out.pdf"]).is_err());
    }

    #[test]
    fn office_conversion_uses_private_outdir_and_fences_default_destination() {
        let f = Fixture::new();
        std::fs::write(f.agent.join("slides.pptx"), "input").unwrap();
        let p = PreparedOffice::new(&f.home, &f.agent, "pptx", "to_pdf", &strings(&["slides.pptx"])).unwrap();
        assert_eq!(p.args()[1], "--outdir");
        assert_eq!(Path::new(&p.args()[2]), p.workspace());
        std::fs::write(p.workspace().join("input.pdf"), "pdf").unwrap();
        assert_eq!(p.publish().unwrap().unwrap(), f.agent.join("slides.pdf"));
        let shared = f.home.join("attachments/shared.pptx");
        std::fs::write(&shared, "input").unwrap();
        assert!(PreparedOffice::new(&f.home, &f.agent, "pptx", "to_pdf", &strings(&[shared.to_str().unwrap()])).is_err());
        PreparedOffice::new(&f.home, &f.agent, "pptx", "to_pdf", &strings(&[shared.to_str().unwrap(), "--outdir", f.agent.to_str().unwrap()])).unwrap();
    }
}
