//! Durable privacy floor for the shared Agent Mail ledger. Changing the
//! current Google selector must never expose already-ingested personal mail.
//! The floor only narrows and survives switching off Gmail or changing owners.

use std::io::Write;
use std::path::Path;

const FILE: &str = "google-account-privacy.jsonl";
const MAX_BYTES: u64 = 64 * 1024;

/// Dashboard receipt access. Personal data is checked per received row,
/// never by the employee's current selector or the installation-wide floor.
#[derive(Clone)]
pub enum Reader {
    Admin,
    Manager { email: Option<String> },
    Person { email: String },
}

impl Reader {
    pub fn allows(&self, mail: &crate::mail::InboxItem) -> bool {
        if matches!(self, Self::Admin) {
            return true;
        }
        match mail.google_account.as_deref() {
            Some(account) => {
                if mail.source != "gmail"
                    || duduclaw_core::google_identity::normalize_email(account).is_err()
                {
                    return false;
                }
                match self {
                    Self::Manager { email: Some(email) } | Self::Person { email } => {
                        email.eq_ignore_ascii_case(account)
                    }
                    _ => false,
                }
            }
            None => matches!(self, Self::Manager { .. }),
        }
    }
}

pub fn accounts(home: &Path) -> Result<Vec<String>, String> {
    let path = home.join(crate::mail::MAIL_DIR).join(FILE);
    match std::fs::metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Ok(m) if m.is_file() && m.len() <= MAX_BYTES => {}
        _ => return Err("信箱帳號保護紀錄無法讀取，請聯絡管理員。".into()),
    }
    let text =
        std::fs::read_to_string(path).map_err(|_| "信箱帳號保護紀錄無法讀取，請聯絡管理員。")?;
    text.lines()
        .map(|line| {
            let email: String =
                serde_json::from_str(line).map_err(|_| "信箱帳號保護紀錄損壞，請聯絡管理員。")?;
            duduclaw_core::google_identity::normalize_email(&email)
        })
        .collect()
}

/// Persist before fetching personal mail. Failure stops the poll.
pub fn protect(home: &Path, email: &str) -> Result<(), String> {
    let email = duduclaw_core::google_identity::normalize_email(email)?;
    let dir = home.join(crate::mail::MAIL_DIR);
    std::fs::create_dir_all(&dir).map_err(|_| "無法保存信箱帳號保護紀錄。")?;
    let path = dir.join(FILE);
    duduclaw_core::with_file_lock(&path, || {
        if accounts(home)
            .map_err(std::io::Error::other)?
            .contains(&email)
        {
            return Ok(());
        }
        let row = format!("{}\n", serde_json::to_string(&email)?);
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        file.write_all(row.as_bytes())?;
        file.sync_all()?;
        #[cfg(unix)]
        std::fs::File::open(&dir)?.sync_all()?;
        Ok(())
    })
    .map_err(|_| "無法保存信箱帳號保護紀錄。".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_floor_is_monotonic_and_corruption_refuses() {
        let home = tempfile::tempdir().unwrap();
        assert!(accounts(home.path()).unwrap().is_empty());
        protect(home.path(), "Alice@c.test").unwrap();
        protect(home.path(), "alice@c.test").unwrap();
        assert_eq!(accounts(home.path()).unwrap(), ["alice@c.test"]);
        std::fs::write(
            home.path().join("config.toml"),
            "[mail]\ngmail_enabled=false\n",
        )
        .unwrap();
        assert_eq!(accounts(home.path()).unwrap(), ["alice@c.test"]);
        protect(home.path(), "bob@c.test").unwrap();
        assert_eq!(
            accounts(home.path()).unwrap(),
            ["alice@c.test", "bob@c.test"]
        );
        std::fs::write(home.path().join("mail").join(FILE), "broken").unwrap();
        assert!(accounts(home.path()).is_err());
        assert!(protect(home.path(), "charlie@c.test").is_err());
    }
}
