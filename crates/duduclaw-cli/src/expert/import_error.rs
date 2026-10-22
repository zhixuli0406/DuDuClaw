//! Machine-readable failure codes for `duduclaw pack install`.
//!
//! Every way an import can stop carries a stable code plus parameters, so
//! the dashboard can explain it in the viewer's language with a suggested
//! fix (`experts.error.<code>` in the web catalogues) instead of showing the
//! installer's zh-TW console text. The console still prints that text.
//!
//! The failure rides inside the ordinary [`DuDuClawError`] (boxed in an
//! `io::Error`, which is the only variant that can carry a typed payload),
//! so no caller signature changes; [`failure_of`] gets it back out.
//!
//! Two channels reach the gateway: the `--report-json` file (`error`
//! object) and, for runs without a report, one stderr line starting with
//! [`MARKER`] followed by `{"code":…,"params":…}`.

use serde_json::{Value, json};

use duduclaw_core::error::DuDuClawError;

/// Prefix of the one stderr line that carries the failure as JSON.
pub const MARKER: &str = "DUDUCLAW_IMPORT_ERROR ";

/// Code used when a failure has no specific class.
pub const UNKNOWN: &str = "import.unknown";

/// A classified import failure.
#[derive(Debug)]
pub struct ImportFailure {
    pub code: &'static str,
    pub params: Value,
    /// zh-TW console text.
    pub message: String,
}

impl std::fmt::Display for ImportFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ImportFailure {}

/// Build a coded error. `message` is what the terminal shows.
pub fn import_err(code: &'static str, params: Value, message: impl Into<String>) -> DuDuClawError {
    DuDuClawError::Io(std::io::Error::other(ImportFailure {
        code,
        params,
        message: message.into(),
    }))
}

/// The classified failure inside `e`, if it carries one.
pub fn failure_of(e: &DuDuClawError) -> Option<&ImportFailure> {
    match e {
        DuDuClawError::Io(io) => io.get_ref().and_then(|r| r.downcast_ref::<ImportFailure>()),
        _ => None,
    }
}

/// `(code, params)` for any error; unclassified ones become
/// [`UNKNOWN`] with the raw text as `detail`.
pub fn code_and_params(e: &DuDuClawError) -> (&'static str, Value) {
    match failure_of(e) {
        Some(f) => (f.code, f.params.clone()),
        None => (UNKNOWN, json!({ "detail": duduclaw_core::truncate_chars(&e.to_string(), 600) })),
    }
}

/// The human text without the error-kind prefix.
pub fn plain_message(e: &DuDuClawError) -> String {
    match failure_of(e) {
        Some(f) => f.message.clone(),
        None => e.to_string(),
    }
}

/// The `error` object written into the report JSON.
pub fn error_json(e: &DuDuClawError) -> Value {
    let (code, params) = code_and_params(e);
    json!({ "code": code, "params": params, "message": plain_message(e) })
}

/// The single stderr marker line (no trailing newline).
pub fn marker_line(e: &DuDuClawError) -> String {
    let (code, params) = code_and_params(e);
    let message = duduclaw_core::truncate_chars(&plain_message(e), 600);
    format!("{MARKER}{}", json!({ "code": code, "params": params, "message": message }))
}

/// The uploaded files are there but their content cannot be read as the
/// format they claim (a manifest that does not parse, an invalid name).
pub fn bad_content(message: String) -> DuDuClawError {
    import_err(
        "import.bad_content",
        json!({ "detail": duduclaw_core::truncate_chars(&message, 600) }),
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coded_error_round_trips_through_the_core_error() {
        let e = import_err("import.too_large", json!({ "limit_mb": 50 }), "壓縮檔過大");
        let f = failure_of(&e).expect("classified");
        assert_eq!(f.code, "import.too_large");
        assert_eq!(f.params["limit_mb"], 50);
        assert_eq!(plain_message(&e), "壓縮檔過大");
        let line = marker_line(&e);
        let body = line.strip_prefix(MARKER).expect("marker prefix");
        let v: Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["code"], "import.too_large");
    }

    #[test]
    fn unclassified_errors_become_unknown_with_detail() {
        let e = DuDuClawError::Config("boom".into());
        let (code, params) = code_and_params(&e);
        assert_eq!(code, UNKNOWN);
        assert!(params["detail"].as_str().unwrap().contains("boom"));
        let e = bad_content("壞掉的設定".into());
        assert_eq!(failure_of(&e).unwrap().code, "import.bad_content");
    }
}
