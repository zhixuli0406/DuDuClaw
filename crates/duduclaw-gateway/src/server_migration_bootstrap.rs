//! Migration-only first-admin bootstrap. Tokens never enter a URL query or log.
use super::{AppState, Arc, ConnectInfo, IntoResponse, Json, SocketAddr, State, UserDb};
use axum::{http::{HeaderMap, StatusCode}, response::{Html, Response}};
use duduclaw_migration::{Error, bootstrap_protocol::{SETUP_PATH, SOCKET_NAME, MAX_SETUP_MESSAGE, SetupRequest, SetupReply}};
use std::path::{Path, PathBuf};

pub(super) struct BootstrapService {
    task: tokio::task::JoinHandle<()>,
    socket: PathBuf,
}
impl Drop for BootstrapService {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket);
    }
}

const PAGE_CSP: &str = "default-src 'none'; script-src 'sha256-g+LKubYGnKxwg+wlRSkGChyQmUmMvKzSR9j50nOLKNw='; connect-src 'self'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'";

fn refused() -> duduclaw_core::error::DuDuClawError {
    duduclaw_core::error::DuDuClawError::Gateway("migration bootstrap unavailable".into())
}

pub(super) async fn start(home: &Path, users: Arc<UserDb>, address: SocketAddr)
    -> duduclaw_core::error::Result<Option<BootstrapService>> {
    let Some(manifest) = crate::migration_startup::completed_manifest(home).map_err(|_| refused())? else { return Ok(None); };
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
        let port = address.port();
        // The root-issued route is intentionally loopback HTTP (remote operators
        // use an SSH tunnel). Never print a link this listener cannot serve.
        if !matches!(address.ip(), std::net::IpAddr::V4(ip) if ip.is_unspecified() || ip == std::net::Ipv4Addr::LOCALHOST) {
            return Err(refused());
        }
        let meta = std::fs::symlink_metadata(home)?;
        if !meta.is_dir() || meta.uid() != manifest.gateway_uid || meta.gid() != nix::unistd::getegid().as_raw() || meta.mode() & 0o077 != 0
            || nix::unistd::geteuid().as_raw() != manifest.gateway_uid || port == 0
            || !duduclaw_core::gateway_instance::held(home) {
            return Err(refused());
        }
        let socket = home.join(SOCKET_NAME);
        match std::fs::symlink_metadata(&socket) {
            Ok(value) if value.file_type().is_socket() && value.uid() == manifest.gateway_uid && value.gid() == meta.gid()
                && value.mode() & 0o7777 == 0o600 => {
                // The gateway instance lock is held before this startup point.
                // A live listener is never replaced, even from the trusted uid.
                if tokio::net::UnixStream::connect(&socket).await.is_ok() { return Err(refused()); }
                std::fs::remove_file(&socket)?;
            }
            Ok(_) => return Err(refused()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let listener = tokio::net::UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break; };
                // One bounded request at a time; failures never reveal details.
                let _ = tokio::time::timeout(std::time::Duration::from_secs(10),
                    serve(stream, &manifest, &users, port)).await;
            }
        });
        Ok(Some(BootstrapService { task, socket }))
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = (manifest, users, address); Err(refused()) }
}

#[cfg(target_os = "linux")]
async fn serve(mut stream: tokio::net::UnixStream,
    manifest: &duduclaw_migration::import_manifest::ImportManifest, users: &UserDb, port: u16) -> Result<(), Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let peer_uid = stream.peer_cred().map_err(|_| Error::Authentication)?.uid();
    if peer_uid != 0 { return Err(Error::Authentication); }
    let current = crate::migration_startup::completed_manifest(&manifest.home)?.ok_or(Error::Authentication)?;
    if !current.same_plan(manifest) || !duduclaw_core::gateway_instance::held(&manifest.home) {
        return Err(Error::Conflict);
    }
    let mut bytes = Vec::new();
    (&mut stream).take((MAX_SETUP_MESSAGE + 1) as u64).read_to_end(&mut bytes).await.map_err(|_| Error::Storage)?;
    if bytes.len() > MAX_SETUP_MESSAGE { return Err(Error::Limit); }
    let request: SetupRequest = serde_json::from_slice(&bytes)?;
    request.authorize(manifest.migration_id, peer_uid)?;
    let token = users.issue_setup_token(false).map_err(|_| Error::Authentication)?;
    let reply = SetupReply { version: 1, migration: manifest.migration_id, port, already_claimed: token.is_none(), token };
    stream.write_all(&serde_json::to_vec(&reply)?).await.map_err(|_| Error::Storage)?;
    stream.shutdown().await.map_err(|_| Error::Storage)?;
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Claim {
    token: String,
    email: String,
    display_name: String,
    password: String,
}
fn available(home: &Path, address: SocketAddr) -> bool {
    address.ip().is_loopback() && crate::migration_startup::completed_manifest(home).is_ok_and(|value| value.is_some())
}
pub(super) async fn page(State(state): State<Arc<AppState>>, ConnectInfo(address): ConnectInfo<SocketAddr>) -> Response {
    if !available(&state.home_dir, address) { return StatusCode::NOT_FOUND.into_response(); }
    (
        [("Cache-Control", "no-store"), ("Referrer-Policy", "no-referrer"),
         ("X-Content-Type-Options", "nosniff"),
         ("Content-Security-Policy", PAGE_CSP)],
        Html(include_str!("migration_setup.html")),
    ).into_response()
}
pub(super) async fn claim(State(state): State<Arc<AppState>>, ConnectInfo(address): ConnectInfo<SocketAddr>,
    headers: HeaderMap, Json(body): Json<Claim>) -> Response {
    // The random setup token is the CSRF credential. In addition require the
    // browser's same-origin JSON fetch; no cookie/session grants authority here.
    let origin = headers.get(axum::http::header::ORIGIN).and_then(|value| value.to_str().ok());
    let host = headers.get(axum::http::header::HOST).and_then(|value| value.to_str().ok());
    if !available(&state.home_dir, address) || !same_origin(origin, host) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.user_db.claim_first_admin_setup_token(&body.token, &body.email, &body.display_name, &body.password) {
        Ok(_) => (
            [("Cache-Control", "no-store")], Json(serde_json::json!({"ok":true})),
        ).into_response(),
        Err(_) => (
            StatusCode::BAD_REQUEST, [("Cache-Control", "no-store")],
            Json(serde_json::json!({"ok":false})),
        ).into_response(),
    }
}
fn same_origin(origin: Option<&str>, host: Option<&str>) -> bool {
    match (origin, host) {
        (Some(origin), Some(host)) => {
            let Some(port) = host.strip_prefix("127.0.0.1:").and_then(|value| value.parse::<u16>().ok()) else { return false; };
            port > 0 && origin == format!("http://127.0.0.1:{port}") && host == format!("127.0.0.1:{port}")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_setup_origin_requires_exact_loopback_origin() {
        assert!(same_origin(Some("http://127.0.0.1:18889"), Some("127.0.0.1:18889")));
        for (origin, host) in [(None, Some("127.0.0.1:18889")),
            (Some("http://127.0.0.1.evil:18889"), Some("127.0.0.1:18889")),
            (Some("http://127.0.0.1:18889"), Some("127.0.0.1:18890")),
            (Some("http://localhost:18889"), Some("localhost:18889"))] { assert!(!same_origin(origin, host)); }
    }
    #[test]
    fn migration_setup_page_has_no_token_query_or_remote_assets() {
        let page = include_str!("migration_setup.html");
        assert!(page.contains("location.hash.slice(1)"));
        assert!(page.contains("history.replaceState"));
        assert!(!page.contains("http://"));
        assert_eq!(SETUP_PATH, "/api/migration/setup");
        use sha2::Digest;
        use base64::Engine;
        let script = page.split_once("<script>").unwrap().1.split_once("</script>").unwrap().0;
        let digest = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(script.as_bytes()));
        assert!(PAGE_CSP.split_whitespace().any(|part| part == format!("'sha256-{digest}';")));
    }
}
