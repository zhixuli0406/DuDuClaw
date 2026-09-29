//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Read the last `n` non-empty lines of a JSONL file, parsing each as a JSON
/// value (unparseable lines skipped). Oldest-first within the returned tail.
/// Missing file ⇒ empty. Used by `os.gate.recent`.
pub(crate) fn read_jsonl_tail(path: &std::path::Path, n: usize) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..]
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

/// Whether an autopilot rule row was produced by PBD rule induction (P4-1) —
/// its `metadata` JSON carries `induced == true` or an induction `source`.
/// Used by `os.status` to count a fleet's induced rules.
pub(crate) fn rule_is_induced(row: &AutopilotRuleRow) -> bool {
    let Some(meta) = row.metadata.as_deref() else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<Value>(meta) else {
        return false;
    };
    v.get("induced").and_then(|x| x.as_bool()) == Some(true)
        || v.get("source")
            .and_then(|x| x.as_str())
            .map(|s| s.contains("induction") || s.contains("pbd"))
            .unwrap_or(false)
}

/// Whether an autopilot rule's `conditions` reference `agent_id` as a value —
/// a best-effort attribution for the `os.status` induced-rule count. Walks the
/// parsed conditions JSON for a string leaf equal to `agent_id`.
pub(crate) fn rule_targets_agent(row: &AutopilotRuleRow, agent_id: &str) -> bool {
    if agent_id.is_empty() {
        return false;
    }
    let Ok(v) = serde_json::from_str::<Value>(&row.conditions) else {
        return false;
    };
    fn walk(v: &Value, needle: &str) -> bool {
        match v {
            Value::String(s) => s == needle,
            Value::Array(a) => a.iter().any(|x| walk(x, needle)),
            Value::Object(o) => o.values().any(|x| walk(x, needle)),
            _ => false,
        }
    }
    walk(&v, agent_id)
}
