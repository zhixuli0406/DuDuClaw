use super::*;

pub const PROMPT_BUDGET: usize = 6_000;
use duduclaw_security::group_context::{OPEN, CLOSE, one_line, envelope, is_safe, row_is_safe, ScanRow};

/// Messages arrive newest first. Only lines after the newest bot reply are
/// candidates. Keep the newest safe lines within the complete block's budget.
pub fn format_context(messages: &[BufferedMessage], budget: usize) -> String {
    let overhead = OPEN.chars().count() + CLOSE.chars().count();
    if budget <= overhead { return String::new(); }
    let mut remaining = budget - overhead;
    let mut lines = Vec::new();
    let mut fields = Vec::new();
    for row in messages {
        if row.direction == "bot_reply" { break; }
        let name = one_line(&row.sender_name);
        let text = one_line(&row.text);
        let time = chrono::DateTime::from_timestamp(row.timestamp,0)
            .map(|at| at.format("%H:%M").to_string()).unwrap_or_else(|| "--:--".into());
        let line = format!("[{time}] {name}: {text}");
        if !row_is_safe(&ScanRow { rendered: &line, sender_name: &name, text: &text, metadata: &[] }) {
            tracing::warn!("group context prompt line omitted by input guard");
            continue;
        }
        let size = line.chars().count() + 1;
        if size > remaining { break; }
        remaining -= size;
        lines.push(line);
        fields.push((name, text));
    }
    if lines.is_empty() { return String::new(); }
    lines.reverse();
    fields.reverse();
    let block = envelope(&lines.join("\n"));
    let rows: Vec<_> = fields.iter().zip(&lines).map(|((name, text), line)|
        ScanRow { rendered: line, sender_name: name, text, metadata: &[] }).collect();
    if !is_safe(&rows, &block) {
        tracing::warn!("group context block omitted by input guard");
        return String::new();
    }
    block
}

pub async fn prompt_context(ctx: &ReplyContext, channel: &str, group: &str, thread: Option<&str>) -> String {
    let home=ctx.home_dir.clone();
    let channel=channel.to_owned(); let group=group.to_owned(); let thread=thread.map(str::to_owned);
    blocking(move || read(&home,&channel,&group,thread.as_deref(),100,None)
        .map(|rows| format_context(&rows,PROMPT_BUDGET))).await.unwrap_or_default()
}

/// Bind prompt history to the triggering inbound row, excluding that row and
/// anything received later while this reply waited in the conversation queue.
pub async fn prompt_context_before(ctx: &ReplyContext, channel: &str, group: &str, thread: Option<&str>, message_id: &str) -> String {
    let home=ctx.home_dir.clone();
    let channel=channel.to_owned(); let group=group.to_owned(); let thread=thread.map(str::to_owned);
    let message_id=message_id.to_owned();
    blocking(move || {
        let rows = read(&home,&channel,&group,thread.as_deref(),100,None)?;
        let db = buffer_db(&home,false)?;
        let anchor: Option<i64> = db.query_row(
            "SELECT id FROM group_context_messages WHERE channel=?1 AND group_id=?2 AND thread_id IS ?3 AND message_id=?4 AND direction='inbound'",
            params![channel,group,thread,message_id], |r| r.get(0),
        ).optional().map_err(|_| UNAVAILABLE)?;
        let anchor = anchor.ok_or(UNAVAILABLE)?;
        let rows=rows.into_iter().filter(|row| row.id < anchor).collect::<Vec<_>>();
        Ok(format_context(&rows,PROMPT_BUDGET))
    }).await.unwrap_or_default()
}
