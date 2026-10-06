use super::*;

#[derive(Debug, Clone)]
pub(crate) enum HandlerObservation {
    RejectedBeforeEffect,
    Unknown,
    Committed(Value),
}
#[derive(Debug, Clone)]
pub(crate) enum ReadObservation {
    Unobserved,
    Observed { output: Value, evidence: Value },
}
tokio::task_local! {
    pub(crate) static EFFECT_OBSERVATION: Arc<Mutex<HandlerObservation>>;
    pub(crate) static READ_OBSERVATION: Arc<Mutex<ReadObservation>>;
}
pub(crate) fn effect_start() {
    let _ = EFFECT_OBSERVATION.try_with(|o| {
        if let Ok(mut o) = o.lock() {
            *o = HandlerObservation::Unknown;
        }
    });
}
pub(crate) fn effect_committed(evidence: Value) {
    let _ = EFFECT_OBSERVATION.try_with(|o| {
        if let Ok(mut o) = o.lock() {
            *o = HandlerObservation::Committed(evidence);
        }
    });
}
pub(crate) fn read_observed(output: Value, evidence: Value) {
    let _ = READ_OBSERVATION.try_with(|o| {
        if let Ok(mut o) = o.lock() {
            *o = ReadObservation::Observed { output, evidence };
        }
    });
}
pub(crate) fn is_workflow_read() -> bool {
    READ_OBSERVATION.try_with(|_| true).unwrap_or(false)
}

/// Available only in an explicitly selected non-shipping test build. There is
/// no tool argument or production environment switch for this checkpoint.
#[cfg(feature = "workflow-test-checkpoints")]
pub(super) async fn host_test_checkpoint(
    home: &Path,
    operation_id: &str,
    phase: &str
) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = home.join(".workflow-test-checkpoint");
        let metadata = match std::fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err("test checkpoint unavailable".into()),
            Ok(m) => m,
        };
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err("test checkpoint host ownership refused".into());
        }
        let request = dir.join("request");
        let metadata = std::fs::symlink_metadata(&request)
            .map_err(|_| "test checkpoint request unavailable")?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o600
            || metadata.len() > 128
        {
            return Err("test checkpoint request refused".into());
        }
        let requested =
            std::fs::read_to_string(request).map_err(|_| "test checkpoint request unavailable")?;
        // Preserve the original after-begin kill fixture protocol.
        let legacy = requested == operation_id && phase == "after_begin";
        if !legacy && requested != format!("{phase}:{operation_id}") {
            return Ok(());
        }
        let marker = dir.join(if legacy { "executing" } else { phase });
        let mut options = std::fs::OpenOptions::new();
        use std::os::unix::fs::OpenOptionsExt;
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW);
        let mut file = options
            .open(marker)
            .map_err(|_| "test checkpoint marker unavailable")?;
        use std::io::Write;
        file.write_all(operation_id.as_bytes())
            .map_err(|_| "test checkpoint marker unavailable")?;
        file.sync_all()
            .map_err(|_| "test checkpoint marker unavailable")?;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !dir.join("release").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "test checkpoint bounded pause expired")?;
    }
    Ok(())
}
