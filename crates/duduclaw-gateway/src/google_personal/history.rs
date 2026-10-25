//! A metadata-only view of retained Google calls. Never return input,
//! output, summaries, channel identities, or arbitrary audit extras.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

pub fn recent(home: &Path, user: &str, email: &str) -> Result<Vec<Value>, String> {
    let since = chrono::Utc::now() - chrono::Duration::days(30);
    let mut out = Vec::new();
    for file in ["tool_calls.jsonl.old", "tool_calls.jsonl"] {
        let input = match std::fs::File::open(home.join(file)) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("使用紀錄暫時無法讀取。".into()),
        };
        // The shared log rotates at 16 MiB. Bound exceptional/corrupt files
        // and individual lines without allocating an arbitrary JSON row.
        let mut input = BufReader::new(input.take(32 * 1024 * 1024));
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = input
                .by_ref()
                .take(64 * 1024 + 1)
                .read_until(b'\n', &mut line)
                .map_err(|_| "使用紀錄暫時無法讀取。")?;
            if read == 0 {
                break;
            }
            if read > 64 * 1024 {
                let mut discarded = Vec::new();
                while !line.ends_with(b"\n") {
                    discarded.clear();
                    if input
                        .by_ref()
                        .take(64 * 1024)
                        .read_until(b'\n', &mut discarded)
                        .map_err(|_| "使用紀錄暫時無法讀取。")?
                        == 0
                    {
                        break;
                    }
                    if discarded.ends_with(b"\n") {
                        break;
                    }
                }
                continue;
            }
            let Ok(row) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            let owns = row.get("google_identity_user").and_then(Value::as_str) == Some(user)
                || (row.get("google_identity").and_then(Value::as_str) == Some("delegate")
                    && row
                        .get("google_account")
                        .and_then(Value::as_str)
                        .is_some_and(|a| a.eq_ignore_ascii_case(email)));
            if !owns {
                continue;
            }
            let Some(timestamp) = row.get("timestamp").and_then(Value::as_str) else {
                continue;
            };
            let Ok(time) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
                continue;
            };
            if time < since {
                continue;
            }
            let (Some(agent), Some(tool), Some(success)) = (
                row.get("agent_id").and_then(Value::as_str),
                row.get("tool_name").and_then(Value::as_str),
                row.get("success").and_then(Value::as_bool),
            ) else {
                continue;
            };
            out.push(
                json!({"timestamp":timestamp,"agent_id":agent,"tool_name":tool,"success":success}),
            );
        }
    }
    out.sort_by(|a, b| b["timestamp"].as_str().cmp(&a["timestamp"].as_str()));
    out.truncate(500);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_own_retained_rows_and_safe_columns_are_returned() {
        let home = tempfile::tempdir().unwrap();
        let base = json!({"timestamp":chrono::Utc::now().to_rfc3339(),"agent_id":"alice-agent","tool_name":"gmail_read","success":true,"google_identity_user":"alice","params_summary":"NEVER_RETURN","input":"NEVER_RETURN","output":"NEVER_RETURN"});
        let mut other = base.clone();
        other["google_identity_user"] = json!("bob");
        let mut old = base.clone();
        old["timestamp"] = json!((chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339());
        std::fs::write(
            home.path().join("tool_calls.jsonl"),
            [base, other, old]
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let rows = recent(home.path(), "alice", "alice@example.com").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].as_object().unwrap().len(), 4);
        assert!(
            !serde_json::to_string(&rows)
                .unwrap()
                .contains("NEVER_RETURN")
        );
    }
}
