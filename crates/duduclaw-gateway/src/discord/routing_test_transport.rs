//! Test-only, canonical-home scoped routing for the previously unmapped sends.
use super::*;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};
const PREFIX: &str = "p0-test-issue210-";
tokio::task_local! { static SENDER_HOME: PathBuf; }
fn routes() -> &'static Mutex<HashMap<(PathBuf, String), String>> {
    static ROUTES: OnceLock<Mutex<HashMap<(PathBuf, String), String>>> = OnceLock::new();
    ROUTES.get_or_init(Default::default)
}
pub(super) struct ScopedTransport {
    home: PathBuf,
    token: String,
    origin: String,
    _alias: crate::test_channel_provider::LocalProviderRoute,
}
pub(super) fn install(home: &Path, provider_token: &str) -> (String, ScopedTransport) {
    let home = home.canonicalize().expect("owned home must exist");
    let mapped = crate::test_channel_provider::url(provider_token, DISCORD_API);
    let parsed = reqwest::Url::parse(&mapped).unwrap();
    assert_eq!(parsed.scheme(), "http");
    assert!(matches!(
        parsed.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]")
    ));
    let origin = parsed.origin().ascii_serialization();
    let token = format!("{PREFIX}{}", uuid::Uuid::new_v4());
    let alias = crate::test_channel_provider::register_local_route(&token, &origin);
    assert!(
        routes()
            .lock()
            .unwrap()
            .insert((home.clone(), token.clone()), origin.clone())
            .is_none()
    );
    let guard = ScopedTransport {
        home,
        token: token.clone(),
        origin,
        _alias: alias,
    };
    (token, guard)
}
pub(super) fn url(home: &Path, token: &str, original: &str) -> String {
    if !token.starts_with(PREFIX) {
        return original.to_owned();
    }
    let home = home
        .canonicalize()
        .expect("owned Discord home must remain available");
    let origin = routes()
        .lock()
        .unwrap()
        .get(&(home, token.to_owned()))
        .cloned();
    let origin = origin.expect("dedicated fixture token is outside its owned home scope");
    let parsed = reqwest::Url::parse(original).unwrap();
    assert_eq!(parsed.scheme(), "https");
    assert_eq!(parsed.host_str(), Some("discord.com"));
    assert_eq!(parsed.port(), None);
    assert!(parsed.path().starts_with("/api/v10/"));
    let mapped = crate::test_channel_provider::url(token, original);
    let mapped_url = reqwest::Url::parse(&mapped).unwrap();
    assert_eq!(mapped_url.origin().ascii_serialization(), origin);
    mapped
}
pub(super) async fn scope_sender<F: std::future::Future>(home: &Path, future: F) -> F::Output {
    let home = home
        .canonicalize()
        .expect("owned document sender home must exist");
    SENDER_HOME.scope(home, future).await
}
pub(super) fn sender_url(token: &str, original: &str) -> String {
    if !token.starts_with(PREFIX) {
        return original.to_owned();
    }
    let home = SENDER_HOME
        .try_with(Clone::clone)
        .expect("dedicated document sender lacks its caller home scope");
    url(&home, token, original)
}
impl Drop for ScopedTransport {
    fn drop(&mut self) {
        assert_eq!(
            routes()
                .lock()
                .unwrap()
                .remove(&(self.home.clone(), self.token.clone())),
            Some(self.origin.clone())
        );
    }
}
