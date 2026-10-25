//! Personal Google OAuth (B2b): only a verified dashboard login can start
//! and complete its own connection. All token handling stays in gateway.

pub mod access;
pub mod authority;
pub mod flow;
pub mod grants;
pub mod history;
pub mod lifecycle;
#[cfg(test)]
mod mail_tests;
pub mod oauth;
pub mod owner_change;
pub mod pending;
pub mod scopes;
#[cfg(test)]
mod tests;

tokio::task_local! {
    /// Set exclusively from an already-verified access JWT at WS handshake.
    /// Legacy dashboard tokens and older JWTs carry None and cannot connect.
    pub static DASHBOARD_SESSION: Option<(String, String)>;
}

pub fn session_for(user: &str) -> Result<String, String> {
    DASHBOARD_SESSION
        .try_with(|session| {
            session
                .as_ref()
                .filter(|(owner, session)| owner == user && uuid::Uuid::parse_str(session).is_ok())
                .map(|(_, session)| session.clone())
        })
        .ok()
        .flatten()
        .ok_or_else(|| "請重新登入後再連結個人 Google；目前的登入憑證沒有可確認的工作階段。".into())
}
