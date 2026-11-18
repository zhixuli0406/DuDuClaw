//! Fixed-vocabulary start-up diagnostics: which step of the updater's
//! start-up failed (`main` prints it after the error code) and, for a
//! read-back, which component and why. Only literals of this crate are ever
//! stored: no paths, hashes, ids or OS error text.
use std::sync::Mutex;

/// Why one component's read-back record was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadbackVerdict { Unanswered, NotReady, Missing, Duplicated, Mismatched, WrongUid, Unexpected }
impl ReadbackVerdict {
    pub const ALL: [Self; 7] = [Self::Unanswered, Self::NotReady, Self::Missing, Self::Duplicated, Self::Mismatched,
        Self::WrongUid, Self::Unexpected];
    pub fn name(self) -> &'static str {
        match self {
            Self::Unanswered => "unanswered",
            Self::NotReady => "not ready",
            Self::Missing => "missing",
            Self::Duplicated => "duplicated",
            Self::Mismatched => "mismatched",
            Self::WrongUid => "wrong uid",
            Self::Unexpected => "unexpected",
        }
    }
}

struct Marker { phase: &'static str, step: &'static str, component: Option<(&'static str, ReadbackVerdict)> }
static MARKER: Mutex<Marker> = Mutex::new(Marker { phase: "", step: "", component: None });

/// A new phase (`start-up`, `committed`, `serving`); clears step and component.
pub fn phase(phase: &'static str) {
    if let Ok(mut marker) = MARKER.lock() { *marker = Marker { phase, step: "", component: None }; }
}
/// The step about to run; clears the component.
pub fn step(step: &'static str) {
    if let Ok(mut marker) = MARKER.lock() { marker.step = step; marker.component = None; }
}
/// The component record (fixed component id) the current step refused.
pub fn component(id: &'static str, verdict: ReadbackVerdict) {
    if let Ok(mut marker) = MARKER.lock() { marker.component = Some((id, verdict)); }
}
/// No component refused (yet) in the current step.
pub fn clear_component() {
    if let Ok(mut marker) = MARKER.lock() { marker.component = None; }
}
/// `<phase>: <step>[ <component> <verdict>]`; `None` before any phase.
pub fn describe() -> Option<String> {
    let marker = MARKER.lock().ok()?;
    render(marker.phase, marker.step, marker.component)
}
fn render(phase: &str, step: &str, component: Option<(&str, ReadbackVerdict)>) -> Option<String> {
    if phase.is_empty() { return None; }
    let mut text = if step.is_empty() { phase.to_string() } else { format!("{phase}: {step}") };
    if let Some((id, verdict)) = component { text.push_str(&format!(" {id} {}", verdict.name())); }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stage_text_is_composed_of_fixed_parts_only() {
        assert_eq!(render("", "lock", None), None);
        assert_eq!(render("start-up", "", None).as_deref(), Some("start-up"));
        assert_eq!(render("start-up", "lock", None).as_deref(), Some("start-up: lock"));
        assert_eq!(render("committed", "read-back", Some(("gateway", ReadbackVerdict::NotReady))).as_deref(),
            Some("committed: read-back gateway not ready"));
        let names: std::collections::BTreeSet<_> = ReadbackVerdict::ALL.iter().map(|verdict| verdict.name()).collect();
        assert_eq!(names.len(), ReadbackVerdict::ALL.len());
        assert!(names.iter().all(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b == b' ')));
    }
}
