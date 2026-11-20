//! Operator-only diagnostic; deliberately separate from the sandboxed fd worker.
fn run() -> duduclaw_import::Result<bool> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 4 || args[2] != "--created-with" { return Err(duduclaw_import::Error::InvalidRequest); }
    let version = args[3].to_str().ok_or(duduclaw_import::Error::InvalidRequest)?;
    let report = duduclaw_import::capture::capture(std::path::Path::new(&args[1]), version)?;
    use std::io::Write;
    std::io::stdout().lock().write_all(&report.encode()?)?;
    Ok(report.has_refusals())
}
fn main() {
    match run() {
        Ok(false) => {},
        Ok(true) => std::process::exit(4),
        Err(_) => { eprintln!("無法擷取結構；請確認參數、唯讀存取權及來源限制。"); std::process::exit(3); },
    }
}
