//! Both file routes bind live authority to the exact bytes they return.
use super::*;
use crate::chat_attachments::{limits, snapshot};
use axum::http::StatusCode;

pub(super) struct Snapshot {
    pub bytes: Vec<u8>,
    hash: String,
    pub admission: Arc<limits::Permit>,
}

pub(super) fn error(status: StatusCode, message: &str) -> axum::response::Response {
    (status, Json(serde_json::json!({"error": message}))).into_response()
}

pub(super) async fn load(state: &AppState, ctx: &UserContext, agent: Option<&str>, name: &str, max: u64)
    -> Result<Snapshot, axum::response::Response> {
    if !crate::files_api::is_safe_filename(name) || crate::files_api::attachments_dir(&state.home_dir, agent).is_none() {
        return Err(error(StatusCode::BAD_REQUEST, "invalid file name or agent"));
    }
    let admission = Arc::new(limits::download(&state.home_dir, &ctx.user_id, max)
        .map_err(|_| error(StatusCode::TOO_MANY_REQUESTS, "attachment service busy"))?);
    // Refuse inaccessible private artifacts before exposing existence/size or
    // doing the bounded read. A second check below binds the actual FD bytes.
    authorize(state, ctx, agent, name, None, admission.clone()).await?;
    let home = state.home_dir.clone();
    let worker_agent = agent.map(str::to_owned);
    let worker_name = name.to_owned();
    let worker_admission = admission.clone();
    let (bytes, hash) = tokio::task::spawn_blocking(move || {
        let _admission = worker_admission;
        let file = crate::files_api::open_attachment(&home, worker_agent.as_deref(), &worker_name)
            .map_err(|e| match e {
                crate::files_api::ResolveError::BadRequest => StatusCode::BAD_REQUEST,
                crate::files_api::ResolveError::NotFound => StatusCode::NOT_FOUND,
                crate::files_api::ResolveError::Denied => StatusCode::FORBIDDEN,
            })?;
        snapshot(file, max).map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)
    }).await.map_err(|_| error(StatusCode::NOT_FOUND, "attachment unavailable"))?
        .map_err(|status| error(status, "attachment unavailable"))?;
    authorize(state, ctx, agent, name, Some((&hash, bytes.len() as u64)), admission.clone()).await?;
    Ok(Snapshot { bytes, hash, admission })
}

async fn authorize(state: &AppState, ctx: &UserContext, agent: Option<&str>, name: &str, content: Option<(&str, u64)>, admission: Arc<limits::Permit>)
    -> Result<(), axum::response::Response> {
    let denied = || error(StatusCode::FORBIDDEN, "access denied");
    let home = state.home_dir.clone();
    let worker_agent = agent.map(str::to_owned);
    let index = tokio::task::spawn_blocking(move || {
        let _admission = admission;
        crate::artifacts::HistoryIndex::load(&home, worker_agent.as_deref())
    }).await.map_err(|_| denied())?;
    let store = state.handler.workflow_store().await.map_err(|_| denied())?;
    if let Some((hash, size)) = content {
        crate::review_evidence::download::authorize_artifact_access_snapshot(
            &state.home_dir, &store, ctx, agent, name, hash, size, &index,
        ).await.map_err(|_| denied())
    } else {
        crate::review_evidence::download::authorize_artifact_access_indexed(
            &state.home_dir, &store, ctx, agent, name, &index,
        ).await.map_err(|_| denied())
    }
}

pub(super) fn response(bytes: Vec<u8>, name: &str, mime: &'static str, inline: bool, admission: Arc<limits::Permit>)
    -> axum::response::Response {
    let size = bytes.len();
    let disposition = if inline { "inline" } else { "attachment" };
    let cd = format!("{disposition}; filename*=UTF-8''{}", crate::files_api::encode_filename_star(name));
    let mut response = axum::response::Response::new(crate::chat_attachments::response::body(bytes, admission));
    let h = response.headers_mut();
    h.insert("content-length", axum::http::HeaderValue::from_str(&size.to_string()).expect("numeric length"));
    h.insert("content-encoding", axum::http::HeaderValue::from_static("identity"));
    h.insert("content-type", axum::http::HeaderValue::from_static(mime));
    h.insert("content-disposition", axum::http::HeaderValue::from_str(&cd).unwrap_or_else(|_| axum::http::HeaderValue::from_static("attachment")));
    h.insert("x-content-type-options", axum::http::HeaderValue::from_static("nosniff"));
    h.insert("cache-control", axum::http::HeaderValue::from_static("private, no-store"));
    h.insert("content-security-policy", axum::http::HeaderValue::from_static("sandbox; default-src 'none'"));
    response
}

fn prepare_office_source(bytes: &[u8], extension: &str, document_limits: &crate::document_limits::DocumentLimits)
    -> Result<(tempfile::TempDir, crate::fs_safe::SafeDir, String), StatusCode> {
    if extension.is_empty() || !extension.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    crate::document_limits::guard_document_bytes(bytes, document_limits)
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    // Per-request private staging replaces the mtime cache, which could
    // reuse a different file's PDF after a same-name replacement.
    // Only the OS-selected temporary root is trusted for alias resolution
    // (e.g. macOS /var -> /private/var); never canonicalize attachment dirs.
    let temporary_root = std::env::temp_dir().canonicalize().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix("dudu-preview-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let staging = builder.tempdir_in(temporary_root)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let directory = crate::fs_safe::SafeDir::open_nofollow(staging.path(), false)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let input_name = format!("source.{extension}");
    if !directory.create_file_new(&input_name, bytes).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Ok((staging, directory, input_name))
}

pub(super) async fn office_preview(state: &AppState, ctx: &UserContext, agent: Option<&str>, name: &str, source: Snapshot)
    -> axum::response::Response {
    let Some(soffice) = crate::files_api::find_soffice() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "尚未安裝 LibreOffice，無法產生 Office 檔預覽；請下載檔案開啟，或安裝 LibreOffice 後重試");
    };
    let size = source.bytes.len() as u64;
    let document_limits = crate::document_limits::DocumentLimits::from_home(&state.home_dir);
    let extension = std::path::Path::new(name).extension().and_then(|s| s.to_str()).unwrap_or("docx").to_owned();
    let admission = source.admission.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        let _admission = admission;
        prepare_office_source(&source.bytes, &extension, &document_limits)
    }).await;
    let (staging, directory, input_name) = match prepared {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(status)) => return error(status, "預覽檔案無法處理，請下載後開啟"),
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "預覽轉檔失敗"),
    };
    let mut command = tokio::process::Command::new(soffice);
    duduclaw_core::ns_guard::guard_command(&mut command);
    command.kill_on_drop(true).arg("--headless")
        .arg(format!("-env:UserInstallation=file://{}", staging.path().join(".lo_profile").display()))
        .arg("--convert-to").arg("pdf").arg("--outdir").arg(staging.path())
        .arg(staging.path().join(input_name))
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    // The converter runs as a parse worker through the execution gate, so a
    // protected install never spawns it directly from the gateway.
    let result = tokio::time::timeout(std::time::Duration::from_secs(60),
        duduclaw_core::execution_gate::status(&mut command, duduclaw_core::execution_gate::ExecutionClass::ParseWorker)).await;
    if !matches!(result, Ok(Ok(status)) if status.success()) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "預覽轉檔失敗，請下載檔案開啟");
    }
    let admission = source.admission.clone();
    let converted = tokio::task::spawn_blocking(move || {
        let _admission = admission;
        let _staging = staging;
        let file = directory.open_read_checked("source.pdf", true).map_err(|_| "preview unavailable")?
            .ok_or("preview unavailable")?;
        snapshot(file, limits::MAX_PREVIEW)
    }).await;
    let bytes = match converted {
        Ok(Ok((bytes, _))) => bytes,
        _ => return error(StatusCode::PAYLOAD_TOO_LARGE, "預覽檔案無法處理，請下載後開啟"),
    };
    // A conversion can take a minute: recheck live grants against the source
    // snapshot hash, without reopening or authorizing the mutable source name.
    if let Err(response) = authorize(state, ctx, agent, name, Some((&source.hash, size)), source.admission.clone()).await { return response; }
    let stem = std::path::Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or("preview");
    response(bytes, &format!("{stem}.pdf"), "application/pdf", true, source.admission)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn files_preview_stages_verified_bytes_in_private_real_directory() {
        use std::os::unix::fs::PermissionsExt;
        let limits = crate::document_limits::DocumentLimits::default();
        let (staging, directory, name) = prepare_office_source(b"a,b\n1,2", "csv", &limits).unwrap();
        assert_eq!(staging.path(), staging.path().canonicalize().unwrap());
        assert_eq!(std::fs::metadata(staging.path()).unwrap().permissions().mode() & 0o077, 0);
        assert_eq!(std::fs::metadata(staging.path().join(&name)).unwrap().permissions().mode() & 0o777, 0o600);
        let input = directory.open_read_checked(&name, true).unwrap().unwrap();
        assert_eq!(snapshot(input, limits::MAX_PREVIEW).unwrap().0, b"a,b\n1,2");
        assert!(prepare_office_source(b"data", "../outside", &limits).is_err());
    }
}
