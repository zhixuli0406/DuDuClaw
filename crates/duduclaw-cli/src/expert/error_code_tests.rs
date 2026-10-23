//! Every import failure class carries its code (what the dashboard turns
//! into plain-language text): archive problems, layout problems, the
//! front door's gates and the report file the dashboard reads.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::import_error::failure_of;
use super::install::{PipelineOpts, cmd_install_with};
use super::safe_zip::{extract_to, test_support::zip_with_raw_names};

fn code_of(res: duduclaw_core::error::Result<impl std::fmt::Debug>) -> &'static str {
    let err = res.expect_err("must fail");
    failure_of(&err).map(|f| f.code).unwrap_or("unclassified")
}

fn write_zip(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn plain_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts: zip::write::SimpleFileOptions =
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in entries {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(body).unwrap();
        }
        zw.finish().unwrap();
    }
    buf.into_inner()
}

#[test]
fn archive_problems_have_their_own_codes() {
    let tmp = tempfile::tempdir().unwrap();
    let out = |n: &str| tmp.path().join(format!("out-{n}"));

    let garbage = write_zip(tmp.path(), "garbage.zip", b"PK not really");
    assert_eq!(code_of(extract_to(&garbage, &out("g"))), "import.not_zip");

    // Sparse file: over the size ceiling without writing 50 MB.
    let big = tmp.path().join("big.zip");
    let f = std::fs::File::create(&big).unwrap();
    f.set_len(super::safe_zip::MAX_UNPACK_BYTES + 1).unwrap();
    let err = extract_to(&big, &out("b")).unwrap_err();
    let fail = failure_of(&err).unwrap();
    assert_eq!(fail.code, "import.too_large");
    assert_eq!(fail.params["limit_mb"], 50);

    let bomb = write_zip(tmp.path(), "bomb.zip", &plain_zip(&[("payload.bin", &vec![0u8; 4 * 1024 * 1024])]));
    assert_eq!(code_of(extract_to(&bomb, &out("z"))), "import.zip_bomb");

    let slip = write_zip(tmp.path(), "slip.zip", &zip_with_raw_names(&[(b"../evil.md".to_vec(), &b"x"[..])]));
    assert_eq!(code_of(extract_to(&slip, &out("s"))), "import.unsafe_path");

    let case = write_zip(
        tmp.path(),
        "case.zip",
        &zip_with_raw_names(&[(b"pack/SKILL.md".to_vec(), &b"a"[..]), (b"pack/skill.md".to_vec(), &b"b"[..])]),
    );
    let err = extract_to(&case, &out("c")).unwrap_err();
    let fail = failure_of(&err).unwrap();
    assert_eq!(fail.code, "import.case_duplicate");
    assert!(fail.params["path"].as_str().unwrap().contains("skill.md"));

    let cp437 = write_zip(
        tmp.path(),
        "cp437.zip",
        &zip_with_raw_names(&[(vec![b'c', b'a', b'f', 0x82, b'.', b'm', b'd'], &b"x"[..])]),
    );
    let err = extract_to(&cp437, &out("e")).unwrap_err();
    let fail = failure_of(&err).unwrap();
    assert_eq!(fail.code, "import.name_encoding_unknown");
    assert!(fail.params["sample"].as_str().unwrap().contains("\\x82"));
}

#[tokio::test]
async fn an_unrecognised_layout_lists_what_was_found_and_where_a_pack_is() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    // Three wrapper folders: one more than the importer steps through.
    let deep = write_zip(
        src.path(),
        "deep.zip",
        &plain_zip(&[("a/b/c/AGENTS.md", b"# rules\n"), ("a/b/readme.txt", b"hi")]),
    );
    let rpt = src.path().join("r.json");
    let o = PipelineOpts { dry_run: true, report_json: Some(rpt.clone()), ..Default::default() };
    let err = cmd_install_with(home.path(), deep.to_str().unwrap(), o).await.unwrap_err();
    let f = failure_of(&err).unwrap();
    assert_eq!(f.code, "import.unrecognized");
    let report: Value = serde_json::from_str(&std::fs::read_to_string(&rpt).unwrap()).unwrap();
    let params = &report["error"]["params"];
    assert_eq!(params["entries"], serde_json::json!(["c/", "readme.txt"]), "{report}");
    assert_eq!(params["found"][0]["path"], "a/b/c/", "{report}");
    assert_eq!(params["found"][0]["format"], "instructions");
    assert_eq!(report["error"]["code"], "import.unrecognized");
}

#[tokio::test]
async fn two_wrapper_folders_and_macos_clutter_still_import() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let zip = write_zip(
        src.path(),
        "wrapped.zip",
        &plain_zip(&[
            ("下載/my-advisor/AGENTS.md", "# 工作規則\n\n先讀資料。\n".as_bytes()),
            ("__MACOSX/下載/my-advisor/._AGENTS.md", b"junk"),
            ("下載/.DS_Store", b"junk"),
        ]),
    );
    let o = PipelineOpts { dry_run: true, ..Default::default() };
    cmd_install_with(home.path(), zip.to_str().unwrap(), o).await.expect("two wrappers are stepped through");
}

#[tokio::test]
async fn pipeline_gates_have_codes() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let missing = src.path().join("nope.zip");
    let o = PipelineOpts { dry_run: true, ..Default::default() };
    assert_eq!(code_of(cmd_install_with(home.path(), missing.to_str().unwrap(), o).await), "import.source_missing");

    let pack = src.path().join("my-advisor");
    std::fs::create_dir_all(&pack).unwrap();
    std::fs::write(pack.join("AGENTS.md"), "# rules\n").unwrap();
    let o = PipelineOpts { dry_run: true, attach_under: Some("ghost-boss".into()), ..Default::default() };
    assert_eq!(code_of(cmd_install_with(home.path(), pack.to_str().unwrap(), o).await), "import.attach_under_missing");

    let bad = src.path().join("bad-pack");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("expert.toml"), "this is = not [valid toml").unwrap();
    let o = PipelineOpts { dry_run: true, ..Default::default() };
    assert_eq!(code_of(cmd_install_with(home.path(), bad.to_str().unwrap(), o).await), "import.bad_content");
}

/// The front door writes a report even when it stops before the pipeline
/// (here: the zip cannot be opened), so the dashboard always has a code.
#[tokio::test]
async fn front_door_failures_still_write_a_coded_report() {
    let home = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let garbage = write_zip(src.path(), "garbage.zip", b"PK not really");
    let rpt = src.path().join("r.json");
    let opts = crate::pack_cmd::InstallOptions {
        dry_run: true,
        report_json: Some(rpt.clone()),
        ..Default::default()
    };
    let err = crate::pack_cmd::install_pack(home.path(), garbage.to_str().unwrap(), opts).await.unwrap_err();
    assert_eq!(failure_of(&err).unwrap().code, "import.not_zip");
    let report: Value = serde_json::from_str(&std::fs::read_to_string(&rpt).unwrap()).unwrap();
    assert_eq!(report["error"]["code"], "import.not_zip");
    let line = super::import_marker_line(&err);
    assert!(line.starts_with(super::import_error::MARKER) && line.contains("import.not_zip"));
}
