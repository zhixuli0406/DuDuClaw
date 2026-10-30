use std::path::Path;
use duduclaw_core::types::CheckStatus;

pub(crate) fn check(home: &Path) -> (String, CheckStatus, String) {
    let name = "外送防護租約".to_string();
    match duduclaw_gateway::egress_guard::recovery::health(home) {
        Ok((count, oldest)) => (name, if count == 0 { CheckStatus::Pass } else { CheckStatus::Warn },
            format!("可讀；{count} 筆；最舊 {} 秒。復原前先停止 gateway，再執行 duduclaw egress-leases list。", oldest.unwrap_or(0))),
        Err(_) => (name, CheckStatus::Fail, "租約紀錄無法讀取；請檢查檔案與權限，外送防護會保留拒絕狀態。".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unused_install_is_readable_and_creates_nothing() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(check(home.path()).1, CheckStatus::Pass);
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    }

    #[test]
    fn unreadable_database_is_reported_as_failure() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("egress_leases.db"), "not sqlite").unwrap();
        assert_eq!(check(home.path()).1, CheckStatus::Fail);
    }
}
