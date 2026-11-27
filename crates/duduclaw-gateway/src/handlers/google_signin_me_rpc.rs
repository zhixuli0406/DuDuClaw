//! Self-service methods accept only the transport's verified dashboard session.
use super::*;
use super::google_signin_rpc::signin_error;
use crate::google_signin::routes::{me, SessionBinding};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Password { password: String }
impl Drop for Password {
    fn drop(&mut self) { zeroize::Zeroize::zeroize(&mut self.password); }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ticket { ticket: String }
fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, &'static str> {
    serde_json::from_value(params).map_err(|_| "signin_invalid_params")
}

impl MethodHandler {
    pub(crate) async fn handle_google_signin_me_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        if ctx.member_key_id.is_some() || ctx.user_id.is_empty() || ctx.user_id == "system" || ctx.must_change_password {
            return signin_error("signin_dashboard_required");
        }
        let session = match crate::google_personal::session_for(&ctx.user_id) {
            Ok(session) => session, Err(_) => return signin_error("signin_dashboard_required"),
        };
        let personal = self.resolve_edition_profile().await.is_personal();
        if personal && method != "me.google_signin.status" { return signin_error("signin_personal_edition"); }
        let Some(db) = self.user_db.read().await.clone() else { return signin_error("signin_storage_unavailable"); };
        if method != "me.google_signin.status" && !crate::server::check_login_rate_limit(
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), &ctx.email) {
            return signin_error("signin_rate_limited");
        }
        // Serialize the personal-sub snapshot with personal connect/disconnect.
        let lease = if matches!(method, "me.google_signin.link_start" | "me.google_signin.link_complete") {
            match crate::google_personal::grants::acquire(&self.home_dir).await {
                Ok(lease) => Some(lease), Err(_) => return signin_error("signin_storage_unavailable"),
            }
        } else { None };
        let binding = SessionBinding { user_id: ctx.user_id.clone(), session_id: session, epoch: ctx.credential_epoch };
        let home = self.home_dir.clone();
        let method = method.to_owned();
        let cache = self.live_ctx_cache.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<Value, &'static str> {
            let _lease = lease;
            me::live_user(&db, &binding)?;
            let now = chrono::Utc::now().timestamp();
            match method.as_str() {
                "me.google_signin.status" => {
                    let _: Empty = parse(params)?;
                    let link = db.google_signin_link_for_user(&binding.user_id).map_err(|_| "signin_storage_unavailable")?;
                    let has_password = db.has_local_password(&binding.user_id).map_err(|_| "signin_storage_unavailable")?;
                    let mut value = json!({"available": !personal && me::available(&home), "linked": link.is_some(), "has_password": has_password});
                    if let Some(link) = link {
                        value["hd"] = json!(link.hd);
                        value["email_at_link"] = json!(link.email_at_link);
                        value["linked_at"] = json!(link.linked_at);
                    }
                    Ok(value)
                }
                "me.google_signin.link_start" => {
                    let input: Password = parse(params)?;
                    Ok(json!({"authorization_url": me::start(&home, &db, binding, false, &input.password)?}))
                }
                "me.google_signin.reauth_start" => {
                    let _: Empty = parse(params)?;
                    Ok(json!({"authorization_url": me::start(&home, &db, binding, true, "")?}))
                }
                "me.google_signin.link_complete" | "me.google_signin.reauth_complete" => {
                    let input: Ticket = parse(params)?;
                    me::complete(&home, &db, &binding, &input.ticket, method == "me.google_signin.reauth_complete", now)?;
                    Ok(json!({}))
                }
                "me.google_signin.unlink" => {
                    let input: Password = parse(params)?;
                    db.self_unlink_google_signin(&binding.user_id, binding.epoch, &input.password, now)?;
                    cache.invalidate_user(&binding.user_id);
                    Ok(json!({"unlinked": true}))
                }
                _ => Err("signin_unknown_method"),
            }
        }).await;
        match result {
            Ok(Ok(value)) => WsFrame::ok_response("", value),
            Ok(Err(code)) => signin_error(code),
            Err(_) => signin_error("signin_storage_unavailable"),
        }
    }
}
