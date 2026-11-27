//! Shared company destination validation and member credential fingerprint.
use std::path::Path;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub(crate) const MEMBER_CREDENTIALS_OFF: &str = "公司尚未開放同仁連接自己的 Odoo 帳號，請洽管理員。";

/// Only a literal boolean true grants access. Missing/unreadable/malformed is off.
pub(crate) fn member_credentials_enabled(home: &Path) -> bool {
    std::fs::read_to_string(home.join("config.toml")).ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
        .is_some_and(|doc| member_credentials_enabled_from_document(&doc))
}
pub(crate) fn member_credentials_enabled_from_document(doc: &toml::Value) -> bool {
    doc.get("odoo").and_then(|v| v.get("member_credentials"))
        .and_then(toml::Value::as_bool) == Some(true)
}

const CONFIG: &str = "公司還沒有設定 Odoo 連線，請找管理員。";

#[derive(Deserialize)]
struct ServerFields {
    #[serde(default)] url: String,
    #[serde(default)] db: String,
    #[serde(default)] allow_private_network: bool,
}
pub(crate) struct Server {
    pub(crate) url: String,
    pub(crate) db: String,
    pub(crate) destination: String,
    pub(crate) fp: String,
    pub(crate) allow_private: bool,
}
pub(crate) fn company_server(home: &Path) -> Result<Option<Server>, String> {
    let text = std::fs::read_to_string(home.join("config.toml")).map_err(|_| CONFIG.to_owned())?;
    let doc: toml::Value = toml::from_str(&text).map_err(|_| CONFIG.to_owned())?;
    company_server_from_document(&doc)
}

/// Parse the same snapshot used to validate the access policy.
pub(crate) fn company_server_from_document(doc: &toml::Value) -> Result<Option<Server>, String> {
    let Some(section) = doc.get("odoo") else { return Ok(None); };
    let fields: ServerFields = section.clone().try_into().map_err(|_| CONFIG.to_owned())?;
    if fields.url.is_empty() && fields.db.is_empty() { return Ok(None); }
    if fields.url != fields.url.trim() || !duduclaw_odoo::url_guard::is_valid_odoo_db(&fields.db) {
        return Err(CONFIG.into());
    }
    duduclaw_odoo::url_guard::check_url(&fields.url, fields.allow_private_network).map_err(|_| CONFIG.to_owned())?;
    let parsed = url::Url::parse(&fields.url).map_err(|_| CONFIG.to_owned())?;
    if parsed.query().is_some() || parsed.fragment().is_some() { return Err(CONFIG.into()); }
    // check_url rejects userinfo; query/fragment are rejected above. Include
    // the effective port and base path so same-host changes remain visible.
    let destination = format!("{}://{}:{}{}", parsed.scheme(), parsed.host_str().ok_or(CONFIG)?,
        parsed.port_or_known_default().ok_or(CONFIG)?, parsed.path());
    let fp = format!("{:x}", Sha256::digest(format!("{}|{}", fields.url, fields.db).as_bytes()));
    Ok(Some(Server { url: fields.url, db: fields.db, destination, fp, allow_private: fields.allow_private_network }))
}

