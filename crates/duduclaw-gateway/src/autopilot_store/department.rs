//! Department CRUD keeps authorization, validation and the write under one rule fence.

use super::*;

impl RuleFence<'_> {
    /// Persist the already-normalized, validated rule without touching runtime counters.
    /// The caller must read and authorize this row under this same fence.
    pub(crate) fn write_department_rule(&self, row: &AutopilotRuleRow) -> Result<(), String> {
        extensions_require_steps_envelope(row)?;
        self.conn.execute(
            "UPDATE autopilot_rules SET name=?1, enabled=?2, trigger_event=?3,
             conditions=?4, action=?5, sequence=?6, metadata=?7,
             active_window=?8, run_policy=?9, updated_by_user_id=?10, updated_at=?11
             WHERE id=?12",
            params![row.name, row.enabled, row.trigger_event, row.conditions, row.action,
                row.sequence, row.metadata, row.active_window, row.run_policy,
                row.updated_by_user_id, row.updated_at, row.id],
        ).map_err(|e| format!("update department rule: {e}"))?;
        Ok(())
    }

    pub(crate) fn remove_department_rule(&self, id: &str) -> Result<(), String> {
        self.conn.execute("DELETE FROM autopilot_history WHERE rule_id=?1", [id])
            .map_err(|e| format!("remove department history: {e}"))?;
        self.conn.execute("DELETE FROM autopilot_rules WHERE id=?1", [id])
            .map_err(|e| format!("remove department rule: {e}"))?;
        Ok(())
    }

    /// The live row remains locked until its scoped history has been read.
    pub(crate) fn department_history(&self, id: &str, limit: i64)
        -> Result<Vec<AutopilotHistoryRow>, String>
    {
        let mut stmt = self.conn.prepare(
            "SELECT id, rule_id, rule_name, triggered_at, result, details
             FROM autopilot_history WHERE rule_id=?1 ORDER BY triggered_at DESC LIMIT ?2",
        ).map_err(|e| format!("prepare department history: {e}"))?;
        let rows = stmt.query_map(params![id, limit], row_to_history)
            .map_err(|e| format!("read department history: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect department history: {e}"))?;
        Ok(rows)
    }
}
