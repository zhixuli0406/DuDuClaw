//! Report lines produced by the shared slot writer, in a shape both callers
//! (the pack importer's report and `migrate from`'s report) can absorb.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Imported,
    Skipped,
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub status: Status,
    /// Machine kind (`instructions`, `user-profile`, `memory`, …) — the
    /// dashboard translates it; never shown raw to end users.
    pub kind: &'static str,
    pub name: String,
    /// zh-TW detail.
    pub detail: String,
    /// Stable message code + parameters for the dashboard (see
    /// `Report::tag`); `None` for lines that only carry free text.
    pub code: Option<&'static str>,
    pub params: serde_json::Value,
}

impl Outcome {
    pub fn new(
        status: Status,
        kind: &'static str,
        name: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Outcome {
            status,
            kind,
            name: name.into(),
            detail: detail.into(),
            code: None,
            params: serde_json::Value::Null,
        }
    }

    /// Attach a message code and parameters.
    pub fn with(mut self, code: &'static str, params: serde_json::Value) -> Self {
        self.code = Some(code);
        self.params = params;
        self
    }
    pub fn imported(
        kind: &'static str,
        name: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self::new(Status::Imported, kind, name, detail)
    }
    pub fn skipped(kind: &'static str, name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(Status::Skipped, kind, name, detail)
    }
    pub fn warning(kind: &'static str, name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(Status::Warning, kind, name, detail)
    }
}
