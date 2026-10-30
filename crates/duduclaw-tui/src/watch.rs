//! Plain, bounded one-employee tail; never enters raw mode or alternate screen.
use crate::{
    Error, Result,
    client::{Client, ConnectionState, RpcReply},
    sanitize::TerminalText,
    strings,
};
use serde_json::{Value, json};
use std::{
    collections::{HashSet, VecDeque},
    hash::{Hash, Hasher},
    io::Write,
    time::Duration,
};
const BUDGET: Duration = Duration::from_secs(15);
pub struct WatchOptions {
    pub client: Client,
    pub agent: String,
}
#[derive(Default)]
struct Tail {
    seen: HashSet<u64>,
    order: VecDeque<u64>,
}
impl Tail {
    fn new_item(&mut self, scope: &str, value: &Value) -> bool {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        scope.hash(&mut hash);
        value.to_string().hash(&mut hash);
        let key = hash.finish();
        if !self.seen.insert(key) {
            // Keep active windows recent so old runs do not replay as keys age.
            self.order.retain(|entry| *entry != key);
            self.order.push_back(key);
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > 2_048
            && let Some(old) = self.order.pop_front()
        {
            self.seen.remove(&old);
        }
        true
    }
    async fn poll<W: Write>(&mut self, options: &WatchOptions, output: &mut W) -> Result<()> {
        let value = options
            .client
            .call(
                "runs.list",
                json!({"agent_id":options.agent,"limit":4}),
                BUDGET,
            )
            .await?;
        let runs = value
            .get("runs")
            .and_then(Value::as_array)
            .ok_or(Error::Protocol)?;
        for run in runs[..runs.len().min(4)].iter().rev() {
            let id = run
                .get("id")
                .and_then(Value::as_str)
                .ok_or(Error::Protocol)?;
            if self.new_item("run", run) {
                write_line(output, &format_run(run))?;
            }
            let detail = options
                .client
                .call("runs.get", json!({"run_id":id}), BUDGET)
                .await?;
            let events = detail
                .get("events")
                .and_then(Value::as_array)
                .ok_or(Error::Protocol)?;
            if events.len() > 256 && self.new_item(id, &json!({"tail_window":256})) {
                write_line(output, strings::WATCH_TRUNCATED)?;
            }
            for event in events.iter().skip(events.len().saturating_sub(256)) {
                if self.new_item(id, event) {
                    write_line(output, &format_event(event))?;
                }
            }
        }
        Ok(())
    }
}
async fn reconcile<W: Write>(
    tail: &mut Tail,
    options: &WatchOptions,
    output: &mut W,
) -> Result<()> {
    match tail.poll(options, output).await {
        Err(Error::Disconnected | Error::Timeout) => write_line(output, strings::STALE),
        result => result,
    }
}
fn field(value: &Value, name: &str) -> String {
    TerminalText::new(
        value
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    )
    .plain()
    .replace('\n', " ")
}
pub fn format_run(value: &Value) -> String {
    format!(
        "{} [{}] {} {}",
        field(value, "started_at"),
        field(value, "status"),
        field(value, "agent_id"),
        field(value, "preview")
    )
}
pub fn format_event(value: &Value) -> String {
    format!(
        "{} [{}] {} {}",
        field(value, "ts"),
        field(value, "kind"),
        field(
            value,
            if value.get("label").is_some() {
                "label"
            } else if value.get("tool").is_some() {
                "tool"
            } else {
                "role"
            }
        ),
        field(value, "preview")
    )
}
fn write_line<W: Write>(output: &mut W, line: &str) -> Result<()> {
    writeln!(output, "{}", TerminalText::new(line.to_owned()).plain())
        .map_err(|_| Error::Output)?;
    output.flush().map_err(|_| Error::Output)
}
pub async fn run_watch(options: WatchOptions) -> Result<()> {
    run_watch_to(options, &mut std::io::stdout()).await
}
pub async fn run_watch_to<W: Write>(options: WatchOptions, output: &mut W) -> Result<()> {
    let shutdown = crate::ui::terminal::shutdown_signal();
    tokio::pin!(shutdown);
    tokio::select! {
        result = watch_loop(&options, output) => result,
        result = &mut shutdown => result,
    }
}
async fn watch_loop<W: Write>(options: &WatchOptions, output: &mut W) -> Result<()> {
    let mut connection = options.client.connection();
    let mut events = options.client.events();
    let mut generation = None;
    let mut tail = Tail::default();
    let mut poll = tokio::time::interval(Duration::from_secs(5));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut subscribed = false;
    loop {
        let state = connection.borrow_and_update().clone();
        match state {
            ConnectionState::LoginRequired(error) => return Err(error),
            ConnectionState::Connected(info) => {
                if generation != Some(info.generation) {
                    let subscription = options
                        .client
                        .call_detailed("runs.subscribe", json!({"agent_id":options.agent}), BUDGET)
                        .await;
                    match subscription {
                        Err(Error::Disconnected | Error::Timeout) => {
                            write_line(output, strings::STALE)?;
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                        Err(error) => return Err(error),
                        Ok(RpcReply::Accepted(_)) => {
                            subscribed = true;
                            write_line(output, strings::WATCH_SUBSCRIBED)?;
                        }
                        Ok(RpcReply::Rejected { message, .. })
                            if message.is("Unknown method: runs.subscribe") =>
                        {
                            subscribed = false;
                            write_line(output, strings::WATCH_POLLING)?;
                        }
                        Ok(RpcReply::Rejected { .. }) => return Err(Error::RpcRejected),
                    }
                    generation = Some(info.generation);
                    reconcile(&mut tail, options, output).await?;
                }
            }
            _ => {
                if generation.take().is_some() {
                    write_line(output, strings::STALE)?;
                }
                if connection.changed().await.is_err() {
                    return Err(Error::Disconnected);
                }
                continue;
            }
        }
        tokio::select! {
            changed = connection.changed() => if changed.is_err() { return Err(Error::Disconnected); },
            _ = poll.tick() => {
                // Also reconcile subscriptions: persistence is the source of truth.
                reconcile(&mut tail, options, output).await?;
            }
            event = events.recv() => match event {
                Ok(event) if event.name == "run.step" && subscribed => {
                    if event.payload.get("agent_id").and_then(Value::as_str) == Some(options.agent.as_str()) {
                        // Subscription frames are additive. Render fields we know;
                        // use the sanitized JSON for newer shapes rather than inventing a schema.
                        write_line(output, &TerminalText::new(event.payload.to_string()).plain())?;
                    }
                }
                Ok(event) if event.name == "events_lost" => {
                    write_line(output, strings::WATCH_GAP)?;
                    reconcile(&mut tail, options, output).await?;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    write_line(output, strings::WATCH_GAP)?;
                    reconcile(&mut tail, options, output).await?;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Err(Error::Disconnected),
                _ => {},
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_watch_output_is_safe_and_deduplicated() {
        let run = json!({"id":"s:1","agent_id":"adu\u{1b}]9;evil\u{7}","started_at":"09:00","status":"running","preview":"你好\n世界\u{202e}"});
        let event = json!({"kind":"tool_step","label":"Read","ts":"09:01","seq":1,"preview":"\u{1b}]8;;https://evil\u{7}file"});
        let mut output = Vec::new();
        write_line(&mut output, strings::WATCH_POLLING).unwrap();
        write_line(&mut output, &format_run(&run)).unwrap();
        write_line(&mut output, &format_event(&event)).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(!text.chars().any(crate::sanitize::forbidden));
        insta::assert_snapshot!("watch_plain", text);
        let mut tail = Tail::default();
        assert!(tail.new_item("s:1", &event));
        assert!(!tail.new_item("s:1", &event));
        assert!(tail.new_item("s:2", &event));
        for n in 0..3_000 {
            tail.new_item("s", &json!(n));
        }
        assert_eq!(tail.order.len(), 2_048);
        assert_eq!(tail.seen.len(), 2_048);
    }
}
