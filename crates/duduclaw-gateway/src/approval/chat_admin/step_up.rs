use std::path::Path;
use crate::{approval::ApprovalRecord, trigger_route::texts::Lang};
use super::press::{PressOutcome, words};

/// TODO(P15/P16): replace this dashboard-only stub with verified step-up tickets.
/// This function must never call decide or mint a proof.
pub(crate) async fn begin(home: &Path, row: &ApprovalRecord, lang: Lang) -> PressOutcome {
    let (home, id) = (home.to_path_buf(), row.id.to_string());
    let link = tokio::task::spawn_blocking(move || crate::deep_link::public_deep_link(
        &home, crate::deep_link::DeepLinkKind::Approval, &id)).await.ok().flatten();
    let mut text = words(lang, "這類審批請到儀表板核准", "Please approve this request on the dashboard.", "この依頼はダッシュボードで承認してください。");
    if let Some(link) = link { text.push(' '); text.push_str(&link); }
    PressOutcome::refused(text, "step_up_required", true)
}
