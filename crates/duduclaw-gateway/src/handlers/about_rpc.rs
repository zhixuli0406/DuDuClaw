use super::*;

impl MethodHandler {
    pub(crate) async fn handle_about_get(&self) -> WsFrame {
        let tier = match crate::license_runtime::global() {
            Some(runtime) => runtime.snapshot().await.tier,
            None => duduclaw_license::LicenseTier::OpenSource,
        };
        WsFrame::ok_response(
            "",
            json!({
                "product_name": "DuDuClaw",
                "vendor": {
                    "name_zh": "嘟嘟數位科技有限公司",
                    "name_en": "DuDu Digital Technology Co., Ltd.",
                    "url": "https://duduclaw.dudustudio.monster",
                },
                "version": env!("CARGO_PKG_VERSION"),
                "tier": tier.to_string(),
            }),
        )
    }
}
