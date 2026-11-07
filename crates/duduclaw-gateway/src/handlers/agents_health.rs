//! Manager health snapshot: metadata only, restricted to the live employee ACL.
use super::*;

impl MethodHandler {
    pub(crate) async fn handle_agents_health(&self, ctx: &UserContext, params: Value) -> WsFrame {
        let days = match params.get("window_days") {
            None => 1,
            Some(value) => match value.as_u64() {
                Some(1) => 1,
                Some(7) => 7,
                _ => return WsFrame::error_response("", "window_days must be 1 or 7"),
            },
        };
        let home = self.home_dir.clone();
        let snapshot = match tokio::task::spawn_blocking(move || crate::employee_health::failures(&home, days)).await {
            Ok(snapshot) => snapshot,
            Err(_) => return WsFrame::error_response("", "health snapshot unavailable"),
        };
        let mut partial = snapshot.partial;
        let accounts = self.cached_rotator().await.status().await;
        let reg = self.registry.read().await;
        let mut agents = Vec::new();
        for agent in reg.list() {
            let id = &agent.config.agent.name;
            if !agent.config.agent.status.is_listable(false) || !ctx.has_agent_access(id, AccessLevel::Viewer) { continue; }
            let s = match crate::employee_health::state(&self.home_dir, id) {
                Ok(s) => s,
                Err(_) => { partial = true; Default::default() },
            };
            if s.tracking_since.is_none() { partial = true; }
            // A CLI's login cannot be proven by another provider's account pool.
            // Only explicitly named pool accounts can be summarized here.
            let pool = &agent.config.model.account_pool;
            let candidates: Vec<_> = accounts.iter().filter(|a| pool.iter().any(|key|
                a.id.eq_ignore_ascii_case(key.trim()) || a.label.eq_ignore_ascii_case(key.trim()))).collect();
            let account_status = if candidates.is_empty() { "unknown" }
                else if candidates.iter().any(|a| a.is_available) { "available" } else { "degraded" };
            let paused = paused_goals(&self.home_dir, id);
            if paused.is_err() { partial = true; }
            agents.push(json!({
                "id": id,
                "name": if agent.config.agent.display_name.is_empty() { id } else { &agent.config.agent.display_name },
                "last_success_at": s.last_success_at,
                "consecutive_failures": s.consecutive_failures,
                "failures": crate::employee_health::failure_counts(&snapshot, id),
                "paused_goals": paused.unwrap_or(0),
                "account_status": account_status,
            }));
        }
        WsFrame::ok_response("", json!({ "agents": agents, "window_days": days, "partial": partial }))
    }
}

fn paused_goals(home: &Path, agent: &str) -> Result<u64, ()> {
    type Cache = std::collections::HashMap<(PathBuf, String), (std::time::Instant, Result<u64, ()>)>;
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Cache>> = std::sync::OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().map_err(|_| ())?;
    let key = (home.to_path_buf(), agent.to_owned());
    if let Some((at, value)) = cache.get(&key) && at.elapsed().as_secs() < 30 { return *value; }
    let result = read_paused_goals(home, agent);
    if cache.len() >= 1024 { cache.clear(); }
    cache.insert(key, (std::time::Instant::now(), result));
    result
}
fn read_paused_goals(home: &Path, agent: &str) -> Result<u64, ()> {
    let path = home.join("tasks.db");
    if !path.exists() { return Ok(0); }
    let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| ())?;
    conn.busy_timeout(std::time::Duration::from_millis(100)).map_err(|_| ())?;
    // Counts are board metadata (no titles, criteria, pause reasons or audiences).
    // Limit the indexed employee/status lookup, flag incomplete rather than scan forever.
    let count: u64 = conn.query_row(
        "SELECT count(*) FROM (SELECT 1 FROM tasks WHERE assigned_to=?1 AND goal_mode=1 AND status IN ('needs_human','paused') LIMIT 1001)",
        [agent], |r| r.get(0),
    ).map_err(|_| ())?;
    if count > 1000 { return Err(()); }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn employee_health_paused_goal_counts_are_employee_scoped() {
        let home = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        db.execute_batch("CREATE TABLE tasks(assigned_to TEXT, goal_mode INTEGER, status TEXT); INSERT INTO tasks VALUES('a',1,'needs_human'),('a',0,'paused'),('b',1,'paused');").unwrap();
        assert_eq!(paused_goals(home.path(), "a"), Ok(1));
        assert_eq!(paused_goals(home.path(), "b"), Ok(1));
    }
}
