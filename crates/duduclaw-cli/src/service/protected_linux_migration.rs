//! Installer handoff to G's durable transaction. No old data is copied in place.
use super::{refused, Result};
use std::{io::IsTerminal, path::Path};
use duduclaw_migration::{contract::MigrationState, root_host::{load_plan, RootMigration,
    pipeline::InstalledIntegration, bootstrap::GatewayBootstrapIssuer}};

const PLAN: &str = "/var/lib/duduclaw-migrate/plan.json";

fn crosses_boundary(installed: Option<&str>, target: &str) -> Result<bool> {
    let target = semver::Version::parse(target).map_err(|_| refused("invalid target version"))?;
    let installed = installed.map(semver::Version::parse).transpose()
        .map_err(|_| refused("invalid installed version"))?;
    Ok((target.major, target.minor) >= (1, 80)
        && installed.is_none_or(|value| (value.major, value.minor) < (1, 80)))
}

/// Returns true once the request belongs to the migration flow, including the
/// successful awaiting-administrator checkpoint. The caller must then return;
/// it must never continue the legacy single-binary installation or F apply call.
pub(super) fn run_if_required(installed: Option<&str>, target: &str) -> Result<bool> {
    if !crosses_boundary(installed, target)? { return Ok(false); }
    match std::fs::symlink_metadata(PLAN) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound =>
            return Err(refused("缺少已驗證安裝套件建立的遷移計畫；尚未停止舊服務。請先完成新版套件驗證與準備。")),
        Err(error) => return Err(error.into()),
        Ok(_) => (),
    }
    if !std::io::stdout().is_terminal() {
        return Err(refused("資料遷移須在管理員終端機執行，才能安全領取首位管理員設定連結；尚未停止舊服務。"));
    }
    let plan = load_plan(Path::new(PLAN)).map_err(|_| refused("遷移計畫無法驗證；尚未停止舊服務。"))?;
    // The protected v1.79.3 launcher fixes HOME=/var/lib/duduclaw and never
    // accepts an inherited DUDUCLAW_HOME. This installer cannot redirect G.
    if plan.state_dir != Path::new("/var/lib/duduclaw-migrate")
        || plan.legacy_home != Path::new("/var/lib/duduclaw/.duduclaw")
        || plan.root_key_path != Path::new("/var/lib/duduclaw-protected/root.key") {
        return Err(refused("遷移計畫與受保護安裝位置不符；尚未停止舊服務。"));
    }
    let mut flow = RootMigration::open(plan, InstalledIntegration::default())
        .map_err(|_| refused("無法取得遷移鎖或讀取進度；未執行舊版升級。"))?;
    // G preflights the signed platform before its first stop/quarantine effect.
    let state = flow.run().map_err(|_| refused("遷移未完成；請檢查已驗證安裝套件與遷移進度後重試。未改用舊版升級。"))?;
    match state {
        MigrationState::AwaitingAdministrator => {
            println!("資料匯入已進入待確認階段；尚未核准的舊權限不會啟用。");
            match flow.new_setup_token(&mut GatewayBootstrapIssuer) {
                Ok(link) => {
                    println!("請使用下列一次性連結建立管理員，再於儀表板逐項確認。");
                    link.print_once().map_err(|_| refused("設定連結無法安全輸出；請在管理員終端機重新產生。"))?;
                    println!("連結僅供本機使用；遠端主機請先建立對應連接埠的 SSH 通道。");
                }
                Err(duduclaw_migration::Error::Conflict) =>
                    println!("新服務已有帳號，未重新發出首位管理員連結。請登入儀表板繼續確認遷移項目。"),
                Err(_) => return Err(refused("尚未取得首位管理員設定連結；資料遷移仍待確認。請確認新服務後重試遷移命令。")),
            }
        }
        MigrationState::Complete => println!("資料遷移與確認已完成；本次未執行舊版原地升級。"),
        _ => return Err(refused("遷移停在未完成階段；未執行舊版升級。")),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_protected_migration_routes_boundary_and_unknown_floor() {
        for target in ["1.80.0", "1.80.0-rc.1", "1.80.1", "2.0.0"] {
            assert!(crosses_boundary(Some("1.79.3"), target).unwrap());
            assert!(crosses_boundary(None, target).unwrap());
        }
        assert!(!crosses_boundary(Some("1.79.3"), "1.79.4").unwrap());
        assert!(!crosses_boundary(Some("1.80.0"), "1.80.1").unwrap());
        assert!(crosses_boundary(Some("bad"), "1.80.0").is_err());
        assert!(crosses_boundary(Some("1.79.3"), "bad").is_err());
    }
}
