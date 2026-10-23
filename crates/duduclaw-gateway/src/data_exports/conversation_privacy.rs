//! Restricted conversation exports use the same typed source and masker as
//! interactive reads. Recollection invalidates old raw files after downgrade.
use super::{
    ExportRequest, ExportResult, fresh,
    source::{self, Table},
    writer::{Cell, LogicalRow},
};
use crate::history_privacy::{self as privacy, handles, projection};
use duduclaw_auth::UserContext;
use std::path::Path;
pub fn collect(
    home: &Path,
    original: &UserContext,
    request: &ExportRequest,
    mut checkpoint: impl FnMut() -> ExportResult<()>,
) -> ExportResult<Table> {
    let agent = request
        .agent_id
        .as_deref()
        .ok_or("EXPORT_PERMISSION_DENIED")?;
    let live = fresh(home, original)?;
    privacy::mode(home, &live, agent)?;
    let mut table = Table {
        headers: vec![
            "message_id",
            "session_id",
            "agent_id",
            "channel",
            "role",
            "content",
            "timestamp_utc",
            "mask_notice",
        ],
        rows: vec![],
    };
    let mut bytes = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let sessions = projection::load(
        home,
        &live.user_id,
        agent,
        crate::history_privacy::read::permit()?,
    )?;
    for s in sessions {
        if std::time::Instant::now() >= deadline {
            return Err("EXPORT_SOURCE_TIMEOUT".into());
        }
        checkpoint()?;
        let current = fresh(home, original)?;
        privacy::mode(home, &current, agent)?;
        let external = projection::external(&s.metadata.class);
        let transcript =
            projection::project_sync(home, projection::clone_rows(&s.rows), external, agent)?;
        if transcript.messages.len() != s.rows.len() {
            return Err("EXPORT_SOURCE_UNAVAILABLE".into());
        }
        for (m, raw) in transcript.messages.iter().zip(s.rows.iter()) {
            if !source::in_range(request, &m.timestamp)? {
                continue;
            }
            let id = handles::reference(
                home,
                &live.user_id,
                &format!("{}:{}", s.metadata.id, raw.id),
            )?;
            let session = if external {
                handles::issue(home, &live.user_id, &s.metadata.id)?
            } else {
                s.metadata.id.clone()
            };
            source::push(
                &mut table,
                LogicalRow {
                    id: id.clone(),
                    cells: vec![
                        Cell::from(id),
                        Cell::from(session),
                        Cell::from(agent),
                        Cell::from(projection::channel(&s.metadata.class)),
                        Cell::from(m.role.clone()),
                        Cell::from(privacy::plain_parts(&m.parts)),
                        Cell::from(m.timestamp.clone()),
                        Cell::from(if external {
                            if transcript.ner {
                                "masked; regex and NER"
                            } else {
                                "masked; regex only; NER not installed"
                            }
                        } else {
                            "own WebChat; full text"
                        }),
                    ],
                },
                &mut bytes,
            )?;
        }
    }
    // A downgrade to unbound denies even if it occurs during inference.
    let current = fresh(home, original)?;
    let _ = privacy::mode(home, &current, agent)?;
    checkpoint()?;
    Ok(table)
}
