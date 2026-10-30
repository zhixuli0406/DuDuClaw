use super::*;

impl MethodHandler {
    pub(crate) async fn handle_about_get(&self) -> WsFrame {
        let tier = match crate::license_runtime::global() {
            Some(runtime) => runtime.snapshot().await.tier,
            None => duduclaw_license::LicenseTier::OpenSource,
        };
        WsFrame::ok_response(
            "",
            about_payload(
                &tier.to_string(),
                crate::distributor_notice::effective_notice(),
            ),
        )
    }
}

/// `about.get` body. The vendor block is fixed; the only
/// optional part is `distributor_notice`, the sanitized HTML from the
/// distributor (signed runtime copy, else compiled into a distributor build).
/// The key is omitted entirely when there is none.
pub(crate) fn about_payload(tier: &str, distributor_notice: Option<String>) -> Value {
    const VENDOR_ZH: &str = "嘟嘟數位科技有限公司";
    const VENDOR_EN: &str = "Dudu Technology Ltd.";
    const VENDOR_URL: &str = "https://duduclaw.dudustudio.monster";
    let mut payload = json!({
        "product_name": "DuDuClaw",
        "vendor": {
            "name_zh": VENDOR_ZH,
            "name_en": VENDOR_EN,
            "url": VENDOR_URL,
        },
        "version": env!("CARGO_PKG_VERSION"),
        "tier": tier,
    });
    if let Some(brand) = crate::distributor_brand::effective() {
        payload["brand"] = json!(brand);
    }
    if let Some(html) = distributor_notice {
        payload["distributor_notice"] = Value::String(html);
    }
    payload
}
