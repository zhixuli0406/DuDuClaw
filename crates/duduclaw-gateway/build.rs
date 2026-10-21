//! Gateway build script: compiles the optional distributor notice.
//!
//! `DUDUCLAW_DISTRIBUTOR_NOTICE_PATH` unset (every public build) generates
//! `COMPILED_NOTICE = None`. When set, the HTML file is read, sanitized with
//! the same allowlist the gateway applies at runtime, and embedded; anything
//! the allowlist removed is printed as a cargo warning.

#[allow(dead_code)]
#[path = "src/image_data.rs"]
mod image_data;

#[allow(dead_code)]
#[path = "src/distributor_notice/sanitize.rs"]
mod sanitize;

#[path = "src/distributor_notice/build_input.rs"]
mod build_input;

fn main() {
    println!("cargo:rerun-if-env-changed={}", build_input::NOTICE_PATH_ENV);
    println!("cargo:rerun-if-changed=src/image_data.rs");
    println!("cargo:rerun-if-changed=src/distributor_notice/sanitize.rs");
    println!("cargo:rerun-if-changed=src/distributor_notice/build_input.rs");

    let notice = std::env::var_os(build_input::NOTICE_PATH_ENV).map(|value| {
        let value = value
            .to_str()
            .unwrap_or_else(|| fail("DUDUCLAW_DISTRIBUTOR_NOTICE_PATH must be UTF-8"));
        let path = build_input::validate_notice_path(value).unwrap_or_else(|e| fail(&e));
        // Rebuild whenever the operator re-exports the notice.
        println!("cargo:rerun-if-changed={}", path.display());
        let raw = build_input::read_notice_file(path, sanitize::MAX_NOTICE_BYTES)
            .unwrap_or_else(|e| fail(&e));
        let report = sanitize::sanitize_notice(&raw).unwrap_or_else(|e| fail(&e.to_string()));
        warn_removals(&report);
        report.html
    });

    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    std::fs::write(
        out_dir.join("distributor_notice.rs"),
        build_input::generate(notice.as_deref()),
    )
    .expect("write generated distributor notice");
}

fn warn_removals(report: &sanitize::NoticeReport) {
    if !report.has_removals() {
        return;
    }
    println!("cargo:warning=distributor notice: the sanitizer removed content (informational list)");
    for element in &report.removed_elements {
        println!("cargo:warning=  removed element <{element}>");
    }
    for attribute in &report.removed_attributes {
        println!("cargo:warning=  removed attribute {attribute}");
    }
    for url in &report.rejected_urls {
        println!("cargo:warning=  refused URL {url}");
    }
}

fn fail(reason: &str) -> ! {
    panic!("Distributor notice build input refused: {reason}");
}
