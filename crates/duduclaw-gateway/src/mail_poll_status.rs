//! Last completed mailbox check. Counters contain no addresses or mail data;
//! an unreadable record is unknown, never silently a successful empty poll.
use serde::{Deserialize, Serialize};
use std::path::Path;

const FILE: &str = "poll-status.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PollStatus {
    pub checked_at: String,
    pub gmail_attempted: bool,
    pub account_unavailable: usize,
    pub transport_errors: usize,
}

impl PollStatus {
    pub fn new(gmail_attempted: bool, report: &crate::mail_worker::PollReport) -> Self {
        Self {
            checked_at: chrono::Utc::now().to_rfc3339(),
            gmail_attempted,
            account_unavailable: report.account_unavailable,
            transport_errors: report.transport_errors,
        }
    }
}

pub(crate) fn record(home: &Path, status: &PollStatus) -> Result<(), String> {
    use std::io::Write;
    let dir = home.join(crate::mail::MAIL_DIR);
    std::fs::create_dir_all(&dir).map_err(|_| "無法保存信箱檢查結果。")?;
    let temporary = dir.join(format!(".poll-status-{}.tmp", uuid::Uuid::new_v4()));
    let write = || -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec(status)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, dir.join(FILE))?;
        #[cfg(unix)]
        std::fs::File::open(&dir)?.sync_all()?;
        Ok(())
    };
    let outcome = write();
    if outcome.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    outcome.map_err(|_| "無法保存信箱檢查結果。".into())
}

pub(crate) fn read(home: &Path) -> Result<Option<PollStatus>, String> {
    let path = home.join(crate::mail::MAIL_DIR).join(FILE);
    match std::fs::metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if metadata.is_file() && metadata.len() <= 16 * 1024 => {}
        _ => return Err("無法讀取上次信箱檢查結果。".into()),
    }
    let status: PollStatus = serde_json::from_str(
        &std::fs::read_to_string(path).map_err(|_| "無法讀取上次信箱檢查結果。")?,
    )
    .map_err(|_| "上次信箱檢查結果已損壞。")?;
    chrono::DateTime::parse_from_rfc3339(&status.checked_at)
        .map_err(|_| "上次信箱檢查時間無法辨識。")?;
    Ok(Some(status))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_counters_survive_reload_and_corruption_is_not_an_empty_success() {
        let home = tempfile::tempdir().unwrap();
        assert!(read(home.path()).unwrap().is_none());
        let report = crate::mail_worker::PollReport {
            account_unavailable: 1,
            ..Default::default()
        };
        record(home.path(), &PollStatus::new(true, &report)).unwrap();
        let status = read(home.path()).unwrap().unwrap();
        assert!(status.gmail_attempted);
        assert_eq!(status.account_unavailable, 1);
        record(home.path(), &PollStatus::new(true, &Default::default())).unwrap();
        assert_eq!(read(home.path()).unwrap().unwrap().account_unavailable, 0);
        std::fs::write(home.path().join(crate::mail::MAIL_DIR).join(FILE), "broken").unwrap();
        assert!(read(home.path()).is_err());
    }
}
