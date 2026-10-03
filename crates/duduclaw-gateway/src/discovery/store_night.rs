//! Discovery defaults and their version history share discovery.db.
use super::{DiscoveryStore, StoreError};
use crate::discovery::night::{
    DefaultsNamespace, FrozenDefaults, HoldoutReceipt, VersionedDefaults,
};
use rusqlite::{OptionalExtension, params};

pub(in crate::discovery) struct NightWorld {
    pub run: super::RunRecord,
    pub tree: Option<crate::discovery::tree::WorldTree>,
    pub exclusion: Option<String>,
}
pub(in crate::discovery) struct NightSnapshot {
    pub observed_worlds: usize,
    pub observed_tasks: usize,
    pub worlds: Vec<NightWorld>,
    pub window_tasks: usize,
    pub selection_exclusions: std::collections::BTreeMap<String, usize>,
}

impl DiscoveryStore {
    pub(in crate::discovery) fn night_snapshot(
        &self,
        agent: &str,
    ) -> Result<NightSnapshot, StoreError> {
        self.conn
            .busy_timeout(std::time::Duration::from_millis(100))?;
        self.ensure_night_schema()?;
        let tx = self.conn.unchecked_transaction()?;
        let (worlds, tasks): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*),COUNT(DISTINCT u.task_id)
            FROM discovery_rounds r JOIN discovery_runs u USING(run_id) WHERE u.agent_id=?1",
            params![agent],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut snapshot = NightSnapshot {
            observed_worlds: usize::try_from(worlds)
                .map_err(|_| StoreError::Corrupt("world count".into()))?,
            observed_tasks: usize::try_from(tasks)
                .map_err(|_| StoreError::Corrupt("task count".into()))?,
            worlds: Vec::new(),
            window_tasks: 0,
            selection_exclusions: Default::default(),
        };
        let ids = if snapshot.observed_worlds > crate::discovery::night::MAX_WORLDS {
            self.night_task_window(agent, &mut snapshot)?
        } else {
            let mut statement=self.conn.prepare("SELECT r.run_id,r.round FROM discovery_rounds r
                JOIN discovery_runs u USING(run_id) WHERE u.agent_id=?1 ORDER BY u.created_at,r.run_id,r.round")?;
            let ids = statement
                .query_map(params![agent], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            snapshot.window_tasks = snapshot.observed_tasks;
            ids
        };
        for (id, round) in ids {
            let round = super::to_u32(round, "round")?;
            let run = self
                .load_run(&id)?
                .ok_or_else(|| StoreError::Corrupt("night run missing".into()))?;
            let world = self
                .load_world(&id, round)?
                .ok_or_else(|| StoreError::Corrupt("night world missing".into()))?;
            let metadata = self
                .load_round_metadata(&id, round)?
                .ok_or_else(|| StoreError::Corrupt("night round missing".into()))?;
            let exclusion = if run.status == "imported" {
                Some("imported")
            } else if run.status != "complete" || metadata.completion != "complete" {
                Some("partial")
            } else if !metadata.full_grid {
                Some("sparse")
            } else if run.has_unconfined {
                Some("unconfined")
            } else if !run.provenance_verified
                || !metadata.comparison_available
                || run.task_id.is_none()
                || run.approved_root_id.is_none()
                || run.runtime.is_none()
                || run.configured_model.is_none()
            {
                Some("provenance_unknown")
            } else {
                None
            };
            let mut exclusion = exclusion.map(str::to_string);
            let tree = if exclusion.is_none() {
                let nodes = self.load_nodes(&id, round)?;
                let expected = u64::from(world.branch_count) * (u64::from(world.refine_count) + 1);
                if nodes.len() as u64 != expected || nodes.iter().any(|node| !node.evaluated) {
                    exclusion = Some("sparse".into());
                    None
                } else {
                    match crate::discovery::tree::WorldTree::new(world.clone(), nodes) {
                        Ok(tree) => Some(tree),
                        Err(_) => {
                            exclusion = Some("invalid_tree".into());
                            None
                        }
                    }
                }
            } else {
                None
            };
            snapshot.worlds.push(NightWorld {
                run,
                tree,
                exclusion,
            });
        }
        let mut contexts: std::collections::BTreeMap<&str, std::collections::BTreeSet<String>> =
            Default::default();
        for world in &snapshot.worlds {
            if let (Some(task), Some(namespace)) = (
                world.run.task_id.as_deref(),
                crate::discovery::night::namespace_for(&world.run),
            ) {
                contexts
                    .entry(task)
                    .or_default()
                    .insert(namespace.key().map_err(StoreError::Corrupt)?);
            }
        }
        let mixed = contexts
            .into_iter()
            .filter(|(_, contexts)| contexts.len() > 1)
            .map(|(task, _)| task.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        for world in &mut snapshot.worlds {
            if world
                .run
                .task_id
                .as_ref()
                .is_some_and(|task| mixed.contains(task))
            {
                world.exclusion = Some("task_group_context_mismatch".into());
                world.tree = None;
            }
        }
        tx.commit()?;
        Ok(snapshot)
    }

    /// Recent task window followed by frozen task-hash order. No score,
    /// policy reward, branch result or held-out win is read for membership.
    /// A task includes every DB world from every run bearing that task ID.
    fn night_task_window(
        &self,
        agent: &str,
        snapshot: &mut NightSnapshot,
    ) -> Result<Vec<(String, i64)>, StoreError> {
        use sha2::{Digest, Sha256};
        use std::collections::BTreeMap;
        struct TaskGroup {
            task: String,
            worlds: usize,
            hash: String,
        }
        let mut statement=self.conn.prepare("SELECT u.task_id,MIN(u.run_id),COUNT(*),
            SUM(CASE WHEN u.status!='complete' OR u.provenance_verified!=1 OR u.has_unconfined!=0
                OR r.completion!='complete' OR r.full_grid!=1 OR r.policy_params IS NULL
                OR u.task_id IS NULL OR u.approved_root_id IS NULL OR u.runtime IS NULL OR u.configured_model IS NULL
                THEN 1 ELSE 0 END),
            (MIN(u.scorer_name)=MAX(u.scorer_name) AND MIN(u.scorer_hash)=MAX(u.scorer_hash)
                AND MIN(u.direction)=MAX(u.direction) AND MIN(u.approved_root_id)=MAX(u.approved_root_id)
                AND MIN(u.runtime)=MAX(u.runtime) AND MIN(u.configured_model)=MAX(u.configured_model))
            FROM discovery_rounds r JOIN discovery_runs u USING(run_id) WHERE u.agent_id=?1
            GROUP BY COALESCE(u.task_id,'unattributed:'||u.run_id)
            ORDER BY MAX(u.created_at) DESC,COALESCE(u.task_id,u.run_id) LIMIT 64")?;
        let rows = statement
            .query_map(params![agent], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<bool>>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        snapshot.window_tasks = rows
            .iter()
            .filter(|(task, _, _, _, _)| task.is_some())
            .count();
        drop(statement);
        let mut groups: BTreeMap<String, Vec<TaskGroup>> = BTreeMap::new();
        for (task, run, count, bad, context_matches) in rows {
            let count = usize::try_from(count)
                .map_err(|_| StoreError::Corrupt("task world count".into()))?;
            let mut exclude = |reason: &str| {
                *snapshot
                    .selection_exclusions
                    .entry(reason.into())
                    .or_default() += count;
            };
            let Some(task) = task else {
                exclude("provenance_unknown");
                continue;
            };
            if crate::discovery::night::validate_task_ids(&[task.clone()]).is_err() {
                exclude("invalid_task_identity");
                continue;
            }
            if count > crate::discovery::night::MAX_WORLDS {
                exclude("task_group_exceeds_window");
                continue;
            }
            if bad != 0 {
                exclude("task_group_ineligible");
                continue;
            }
            if context_matches != Some(true) {
                exclude("task_group_context_mismatch");
                continue;
            }
            let run = self
                .load_run(&run)?
                .ok_or_else(|| StoreError::Corrupt("window run missing".into()))?;
            let Some(namespace) = crate::discovery::night::namespace_for(&run) else {
                exclude("provenance_unknown");
                continue;
            };
            let key = namespace.key().map_err(StoreError::Corrupt)?;
            if crate::discovery::night::is_heldout(&task) {
                let used: bool = self.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM discovery_night_holdout_tasks
                    WHERE namespace=?1 AND task_id=?2)",
                    params![key, task],
                    |row| row.get(0),
                )?;
                if used {
                    exclude("heldout_already_consumed");
                    continue;
                }
            }
            groups.entry(key).or_default().push(TaskGroup {
                hash: format!("{:x}", Sha256::digest(task.as_bytes())),
                task,
                worlds: count,
            });
        }
        let mut selected = Vec::new();
        let mut capacity = crate::discovery::night::MAX_WORLDS;
        for (_, mut tasks) in groups {
            tasks.sort_by(|left, right| {
                left.hash
                    .cmp(&right.hash)
                    .then_with(|| left.task.cmp(&right.task))
            });
            let mut training = tasks
                .iter()
                .filter(|task| !crate::discovery::night::is_heldout(&task.task))
                .collect::<Vec<_>>();
            let mut heldout = tasks
                .iter()
                .filter(|task| crate::discovery::night::is_heldout(&task.task))
                .collect::<Vec<_>>();
            let mut chosen = Vec::new();
            let mut remaining = capacity;
            let mut train_count = 0usize;
            let mut held_count = 0usize;
            // Reserve one world per remaining required task before accepting
            // a larger whole group. A failed namespace consumes no capacity.
            for required in 0..16 {
                let held = required % 2 == 1;
                let pool = if held { &mut heldout } else { &mut training };
                let reserve = 15 - required;
                let Some(position) = pool
                    .iter()
                    .position(|task| task.worlds <= remaining.saturating_sub(reserve))
                else {
                    break;
                };
                let task = pool.remove(position);
                remaining -= task.worlds;
                chosen.push(task);
                if held {
                    held_count += 1;
                } else {
                    train_count += 1;
                }
            }
            if train_count < 8 || held_count < 8 {
                *snapshot
                    .selection_exclusions
                    .entry(if capacity < 16 {
                        "task_group_window_capacity".into()
                    } else {
                        "insufficient_fresh_task_window".into()
                    })
                    .or_default() += tasks.iter().map(|task| task.worlds).sum::<usize>();
                continue;
            }
            // Additional complete tasks remain balanced by partition counts;
            // hash order breaks all ties and scores never enter this choice.
            loop {
                let held = held_count <= train_count;
                let (first, second) = if held {
                    (&mut heldout, &mut training)
                } else {
                    (&mut training, &mut heldout)
                };
                let mut partition = held;
                let task = if let Some(position) =
                    first.iter().position(|task| task.worlds <= remaining)
                {
                    Some(first.remove(position))
                } else if let Some(position) =
                    second.iter().position(|task| task.worlds <= remaining)
                {
                    partition = !held;
                    Some(second.remove(position))
                } else {
                    None
                };
                let Some(task) = task else { break };
                remaining -= task.worlds;
                chosen.push(task);
                if partition {
                    held_count += 1;
                } else {
                    train_count += 1;
                }
            }
            let chosen_ids = chosen
                .iter()
                .map(|task| task.task.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            let unselected = tasks
                .iter()
                .filter(|task| !chosen_ids.contains(task.task.as_str()))
                .map(|task| task.worlds)
                .sum::<usize>();
            if unselected > 0 {
                *snapshot
                    .selection_exclusions
                    .entry("task_group_window_capacity".into())
                    .or_default() += unselected;
            }
            for task in chosen {
                let mut statement=self.conn.prepare("SELECT r.run_id,r.round FROM discovery_rounds r
                    JOIN discovery_runs u USING(run_id) WHERE u.agent_id=?1 AND u.task_id=?2 ORDER BY r.run_id,r.round")?;
                let ids = statement
                    .query_map(params![agent, task.task], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                if ids.len() != task.worlds {
                    return Err(StoreError::Corrupt("task window changed".into()));
                }
                selected.extend(ids);
            }
            capacity = remaining;
        }
        Ok(selected)
    }

    pub(in crate::discovery) fn night_candidates(
        &self,
        namespace: &DefaultsNamespace,
    ) -> Result<Vec<FrozenDefaults>, StoreError> {
        // Occurrence/revision/run fields are deliberately absent from these
        // small deployment keys. Raw source blobs are fetched one at a time
        // only after task provenance passes, never as a corpus-sized result.
        const PAGE: usize = 8;
        const MAX_METADATA: usize = 32;
        const MAX_RAW_READS: usize = 16;
        const MAX_PARAMS_BYTES: i64 = 1024 * 1024;
        let tx = self.conn.unchecked_transaction()?;
        let mut metadata = Vec::new();
        let mut metadata_truncated = false;
        for offset in (0..MAX_METADATA).step_by(PAGE) {
            let limit = if offset + PAGE == MAX_METADATA {
                PAGE + 1
            } else {
                PAGE
            };
            let mut statement=self.conn.prepare("WITH eligible AS (
                SELECT e.id,e.policy_id,e.source_origin_task_ids,
                    COALESCE(json_extract(e.policy_params,'$.source_sha256'),json_extract(e.policy_params,'$.version.source_sha256')) AS source_hash,
                    CASE WHEN e.policy_id=?8 THEN 0.6 ELSE CAST(json_extract(e.policy_params,'$.beta') AS REAL) END AS effective_beta,
                    COALESCE(json_extract(e.policy_params,'$.knobs'),'{}') AS knobs
                FROM discovery_policy_evals e JOIN discovery_runs u USING(run_id)
                WHERE u.agent_id=?1 AND u.scorer_name=?2 AND u.scorer_hash=?3 AND u.direction=?4
                AND u.approved_root_id=?5 AND u.runtime=?6 AND u.configured_model=?7
                AND u.status='complete' AND u.provenance_verified=1 AND u.has_unconfined=0
                AND e.valid=1 AND e.violation IS NULL AND e.comparison_available=1 AND e.out_of_support=0
                AND e.context_mismatch_rate=0 AND e.evaluation_origin IN ('task_development','incumbent','night_replay','builtin')
                AND length(CAST(e.policy_params AS BLOB))<=?9 AND json_valid(e.policy_params)
                AND length(CAST(e.source_origin_task_ids AS BLOB))<=40960
                AND (e.policy_id=?8 OR json_type(e.policy_params,'$.beta') IN ('real','integer'))
                AND e.policy_id=json_extract(e.policy_params,'$.policy_id'))
                SELECT MIN(id),policy_id,source_hash,effective_beta,source_origin_task_ids
                FROM eligible WHERE length(source_hash)=64 AND knobs='{}' AND effective_beta BETWEEN 0 AND 1
                GROUP BY policy_id,source_hash,effective_beta,source_origin_task_ids
                ORDER BY policy_id,source_hash,effective_beta,source_origin_task_ids LIMIT ?10 OFFSET ?11")?;
            let page = statement
                .query_map(
                    params![
                        namespace.agent_id,
                        namespace.scorer_name,
                        namespace.scorer_hash,
                        namespace.direction.as_str(),
                        namespace.approved_root_id,
                        namespace.runtime,
                        namespace.configured_model,
                        crate::discovery::policy::BASELINE_POLICY_ID,
                        MAX_PARAMS_BYTES,
                        limit as i64,
                        offset as i64
                    ],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, f64>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            let exhausted = page.len() < limit;
            if page.len() > PAGE {
                metadata_truncated = true;
            }
            metadata.extend(page.into_iter().take(PAGE));
            if exhausted {
                break;
            }
        }
        // The extra small-key probe distinguishes an exact boundary from a
        // truncated catalog. Hitting a bound never claims a partial catalog
        // was the complete set of qualified alternatives.
        let metadata_at_cap = metadata_truncated;
        let mut candidates = std::collections::BTreeMap::new();
        let mut raw_reads = 0usize;
        for (row_id, id, _hash, _beta, origins) in metadata {
            let Ok(origins) = serde_json::from_str::<Vec<String>>(&origins) else {
                continue;
            };
            if crate::discovery::night::validate_task_ids(&origins).is_err() {
                continue;
            }
            if id == crate::discovery::policy::BASELINE_POLICY_ID {
                if !origins.is_empty() {
                    continue;
                }
            } else {
                if origins.is_empty()
                    || origins
                        .iter()
                        .any(|task| crate::discovery::night::is_heldout(task))
                {
                    continue;
                }
                if !self.night_origins_verified(namespace, &origins)? {
                    continue;
                }
            }
            if raw_reads == MAX_RAW_READS {
                return Err(StoreError::Corrupt(
                    "night candidate raw-read limit exceeded".into(),
                ));
            }
            raw_reads += 1;
            let json: String = self.conn.query_row(
                "SELECT policy_params FROM discovery_policy_evals WHERE id=?1
                AND length(CAST(policy_params AS BLOB))<=?2",
                params![row_id, MAX_PARAMS_BYTES],
                |row| row.get(0),
            )?;
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) else {
                continue;
            };
            if id == crate::discovery::policy::BASELINE_POLICY_ID {
                let source = value.get("source").and_then(|v| v.as_str());
                let hash = value.get("source_sha256").and_then(|v| v.as_str());
                use sha2::{Digest, Sha256};
                let expected = format!(
                    "{:x}",
                    Sha256::digest(crate::discovery::policy_runner::BASELINE_SOURCE.as_bytes())
                );
                if origins.is_empty()
                    && source == Some(crate::discovery::policy_runner::BASELINE_SOURCE)
                    && hash == Some(expected.as_str())
                {
                    let baseline = FrozenDefaults::default();
                    candidates.insert(
                        baseline.fingerprint().map_err(StoreError::Corrupt)?,
                        baseline,
                    );
                }
                if candidates.len() > crate::discovery::night::MAX_CANDIDATES {
                    return Err(StoreError::Corrupt(
                        "night candidate catalog exceeds the complete-set limit".into(),
                    ));
                }
                continue;
            }
            if origins.is_empty()
                || origins
                    .iter()
                    .any(|task| crate::discovery::night::is_heldout(task))
            {
                continue;
            }
            let candidate = FrozenDefaults {
                policy_id: id,
                source: value
                    .get("source")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                source_sha256: value
                    .get("source_sha256")
                    .or_else(|| value.pointer("/version/source_sha256"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                beta: value
                    .get("beta")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(f64::NAN),
                knobs: value
                    .get("knobs")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default(),
                origin_task_ids: origins,
            };
            if candidate.validate().is_err() {
                continue;
            }
            candidates.insert(
                candidate.fingerprint().map_err(StoreError::Corrupt)?,
                candidate,
            );
            if candidates.len() > crate::discovery::night::MAX_CANDIDATES {
                return Err(StoreError::Corrupt(
                    "night candidate catalog exceeds the complete-set limit".into(),
                ));
            }
        }
        if metadata_at_cap {
            return Err(StoreError::Corrupt(
                "night candidate metadata window exceeded".into(),
            ));
        }
        tx.commit()?;
        Ok(candidates.into_values().collect())
    }

    fn night_origins_verified(
        &self,
        namespace: &DefaultsNamespace,
        origins: &[String],
    ) -> Result<bool, StoreError> {
        for task in origins {
            let (all,matching):(i64,i64)=self.conn.query_row("SELECT COUNT(*),COALESCE(SUM(
                agent_id=?2 AND scorer_name=?3 AND scorer_hash=?4 AND direction=?5 AND approved_root_id=?6
                AND runtime=?7 AND configured_model=?8 AND status='complete' AND provenance_verified=1 AND has_unconfined=0),0)
                FROM discovery_runs WHERE task_id=?1",params![task,namespace.agent_id,namespace.scorer_name,
                    namespace.scorer_hash,namespace.direction.as_str(),namespace.approved_root_id,namespace.runtime,
                    namespace.configured_model],|row|Ok((row.get(0)?,row.get(1)?)))?;
            if all == 0 || all != matching {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(in crate::discovery) fn fresh_night_tasks(
        &self,
        namespace: &DefaultsNamespace,
        tasks: &[String],
    ) -> Result<Vec<String>, StoreError> {
        self.ensure_night_schema()?;
        let key = namespace.key().map_err(StoreError::Corrupt)?;
        let mut fresh = Vec::new();
        for task in tasks {
            let used: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM discovery_night_holdout_tasks
                WHERE namespace=?1 AND task_id=?2)",
                params![key, task],
                |row| row.get(0),
            )?;
            if !used {
                fresh.push(task.clone());
            }
        }
        Ok(fresh)
    }

    pub(in crate::discovery) fn save_night_report(
        &self,
        report: &crate::discovery::night::NightReport,
    ) -> Result<(), StoreError> {
        self.ensure_night_schema()?;
        self.conn.execute("INSERT INTO discovery_night_reports(id,agent_id,report,created_at) VALUES(?1,?2,?3,?4)",
            params![uuid::Uuid::new_v4().to_string(),report.agent_id,serde_json::to_string(&report.public_view())?,
                chrono::Utc::now().to_rfc3339()])?;
        Ok(())
    }

    /// Development origin is stamped by the host writer. Merely replaying a
    /// source on a task does not establish that task as its development origin.
    pub fn set_policy_eval_origin(
        &self,
        id: i64,
        origin_task_ids: &[String],
        evaluation_origin: &str,
    ) -> Result<(), StoreError> {
        crate::discovery::night::validate_task_ids(origin_task_ids).map_err(StoreError::Corrupt)?;
        if !matches!(
            evaluation_origin,
            "task_development" | "incumbent" | "night_replay" | "builtin" | "unknown"
        ) {
            return Err(StoreError::Corrupt(
                "unknown replay evaluation origin".into(),
            ));
        }
        let mut origins = origin_task_ids.to_vec();
        origins.sort();
        let changed = self.conn.execute(
            "UPDATE discovery_policy_evals SET source_origin_task_ids=?2,evaluation_origin=?3
            WHERE id=?1 AND evaluation_origin='unknown'",
            params![id, serde_json::to_string(&origins)?, evaluation_origin],
        )?;
        if changed != 1 {
            return Err(StoreError::Corrupt(
                "policy evaluation origin already bound or missing".into(),
            ));
        }
        Ok(())
    }
    fn ensure_night_schema(&self) -> Result<(), StoreError> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS discovery_defaults(
            namespace TEXT PRIMARY KEY, version INTEGER NOT NULL, bundle TEXT NOT NULL, updated_at TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS discovery_default_versions(
            namespace TEXT NOT NULL,version INTEGER NOT NULL,bundle TEXT NOT NULL,report TEXT NOT NULL,
            created_at TEXT NOT NULL,PRIMARY KEY(namespace,version));
            CREATE TABLE IF NOT EXISTS discovery_night_reports(
            id TEXT PRIMARY KEY,agent_id TEXT NOT NULL,report TEXT NOT NULL,created_at TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS discovery_night_holdout_tasks(
            namespace TEXT NOT NULL,task_id TEXT NOT NULL,receipt_id TEXT NOT NULL,
            PRIMARY KEY(namespace,task_id));
            CREATE TABLE IF NOT EXISTS discovery_night_receipts(
            id TEXT PRIMARY KEY,namespace TEXT NOT NULL,default_version INTEGER NOT NULL,
            candidate_hash TEXT NOT NULL,task_ids TEXT NOT NULL,status TEXT NOT NULL,
            report TEXT,created_at TEXT NOT NULL);")?;
        Ok(())
    }
    pub fn load_discovery_default(
        &self,
        namespace: &DefaultsNamespace,
    ) -> Result<VersionedDefaults, StoreError> {
        self.ensure_night_schema()?;
        let key = namespace.key().map_err(StoreError::Corrupt)?;
        let row: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT version,bundle FROM discovery_defaults WHERE namespace=?1",
                params![key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match row {
            None => Ok(VersionedDefaults::default()),
            Some((version, bundle)) => {
                let bundle: FrozenDefaults = serde_json::from_str(&bundle)?;
                bundle.validate().map_err(StoreError::Corrupt)?;
                Ok(VersionedDefaults {
                    version: super::to_u64(version, "default version")?,
                    bundle,
                })
            }
        }
    }
    pub fn compare_and_swap_discovery_default(
        &self,
        namespace: &DefaultsNamespace,
        expected: u64,
        bundle: &FrozenDefaults,
        report: &serde_json::Value,
    ) -> Result<Option<VersionedDefaults>, StoreError> {
        self.ensure_night_schema()?;
        bundle.validate().map_err(StoreError::Corrupt)?;
        let key = namespace.key().map_err(StoreError::Corrupt)?;
        let frozen = serde_json::to_string(bundle)?;
        let at = chrono::Utc::now().to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;
        // The first write obtains SQLite's writer authority before version
        // lookup. Both creation and update then compare the expected version.
        self.conn.execute(
            "INSERT OR IGNORE INTO discovery_defaults(namespace,version,bundle,updated_at)
            VALUES(?1,0,?2,?3)",
            params![key, serde_json::to_string(&FrozenDefaults::default())?, at],
        )?;
        let version = expected
            .checked_add(1)
            .ok_or_else(|| StoreError::Corrupt("default version overflow".into()))?;
        let changed = self.conn.execute(
            "UPDATE discovery_defaults SET version=?2,bundle=?3,updated_at=?4
            WHERE namespace=?1 AND version=?5",
            params![
                key,
                super::to_i64(version, "default version")?,
                frozen,
                at,
                super::to_i64(expected, "expected version")?
            ],
        )?;
        if changed != 1 {
            tx.rollback()?;
            return Ok(None);
        }
        self.conn.execute(
            "INSERT INTO discovery_default_versions(namespace,version,bundle,report,created_at)
            VALUES(?1,?2,?3,?4,?5)",
            params![
                key,
                super::to_i64(version, "default version")?,
                frozen,
                serde_json::to_string(report)?,
                at
            ],
        )?;
        tx.commit()?;
        Ok(Some(VersionedDefaults {
            version,
            bundle: bundle.clone(),
        }))
    }

    pub(in crate::discovery) fn finish_night_holdout(
        &self,
        namespace: &DefaultsNamespace,
        receipt: &HoldoutReceipt,
        candidate: &FrozenDefaults,
        evidence: &crate::discovery::night::PromotionEvidence,
        authorized: impl Fn() -> bool,
    ) -> Result<Option<VersionedDefaults>, StoreError> {
        self.ensure_night_schema()?;
        if !authorized() {
            return Ok(None);
        }
        let key = namespace.key().map_err(StoreError::Corrupt)?;
        if receipt.namespace_key != key
            || receipt.candidate_hash != candidate.fingerprint().map_err(StoreError::Corrupt)?
            || evidence.heldout_tasks != receipt.task_ids.len()
        {
            return Err(StoreError::Corrupt(
                "held-out receipt identity mismatch".into(),
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        let changed=self.conn.execute("UPDATE discovery_night_receipts SET status=?2,report=?3
            WHERE id=?1 AND namespace=?4 AND default_version=?5 AND candidate_hash=?6 AND status='claimed'",
            params![receipt.id,if evidence.eligible {"adopted"} else {"rejected"},serde_json::to_string(evidence)?,key,
                super::to_i64(receipt.expected_version,"default version")?,receipt.candidate_hash])?;
        if changed != 1 {
            tx.rollback()?;
            return Ok(None);
        }
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM discovery_night_holdout_tasks
            WHERE namespace=?1 AND receipt_id=?2",
            params![key, receipt.id],
            |row| row.get(0),
        )?;
        if usize::try_from(count).ok() != Some(receipt.task_ids.len()) {
            return Err(StoreError::Corrupt("held-out reservation changed".into()));
        }
        if !evidence.eligible {
            tx.commit()?;
            return Ok(None);
        }
        let version = receipt
            .expected_version
            .checked_add(1)
            .ok_or_else(|| StoreError::Corrupt("default version overflow".into()))?;
        let frozen = serde_json::to_string(candidate)?;
        let at = chrono::Utc::now().to_rfc3339();
        let changed = self.conn.execute(
            "UPDATE discovery_defaults SET version=?2,bundle=?3,updated_at=?4
            WHERE namespace=?1 AND version=?5",
            params![
                key,
                super::to_i64(version, "default version")?,
                frozen,
                at,
                super::to_i64(receipt.expected_version, "default version")?
            ],
        )?;
        if changed != 1 {
            self.conn.execute(
                "UPDATE discovery_night_receipts SET status='cas_conflict' WHERE id=?1",
                params![receipt.id],
            )?;
            tx.commit()?;
            return Ok(None);
        }
        self.conn.execute(
            "INSERT INTO discovery_default_versions(namespace,version,bundle,report,created_at)
            VALUES(?1,?2,?3,?4,?5)",
            params![
                key,
                super::to_i64(version, "default version")?,
                frozen,
                serde_json::to_string(evidence)?,
                at
            ],
        )?;
        if !authorized() {
            tx.rollback()?;
            return Ok(None);
        }
        tx.commit()?;
        Ok(Some(VersionedDefaults {
            version,
            bundle: candidate.clone(),
        }))
    }

    pub fn claim_night_holdout(
        &self,
        namespace: &DefaultsNamespace,
        expected_version: u64,
        candidate: &FrozenDefaults,
        tasks: &[String],
    ) -> Result<Option<HoldoutReceipt>, StoreError> {
        self.ensure_night_schema()?;
        crate::discovery::night::validate_task_ids(tasks).map_err(StoreError::Corrupt)?;
        if tasks.len() < 8
            || tasks
                .iter()
                .any(|task| !crate::discovery::night::is_heldout(task))
        {
            return Err(StoreError::Corrupt(
                "held-out reservation requires eight held-out tasks".into(),
            ));
        }
        let key = namespace.key().map_err(StoreError::Corrupt)?;
        let candidate_hash = candidate.fingerprint().map_err(StoreError::Corrupt)?;
        let id = uuid::Uuid::new_v4().to_string();
        let tx = self.conn.unchecked_transaction()?;
        self.conn.execute(
            "INSERT OR IGNORE INTO discovery_defaults(namespace,version,bundle,updated_at)
            VALUES(?1,0,?2,?3)",
            params![
                key,
                serde_json::to_string(&FrozenDefaults::default())?,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        let current: i64 = self.conn.query_row(
            "SELECT version FROM discovery_defaults WHERE namespace=?1",
            params![key],
            |row| row.get(0),
        )?;
        if super::to_u64(current, "default version")? != expected_version {
            tx.rollback()?;
            return Ok(None);
        }
        for task in tasks {
            let changed = self.conn.execute(
                "INSERT OR IGNORE INTO discovery_night_holdout_tasks(namespace,task_id,receipt_id)
                VALUES(?1,?2,?3)",
                params![key, task, id],
            )?;
            if changed != 1 {
                tx.rollback()?;
                return Ok(None);
            }
        }
        self.conn.execute("INSERT INTO discovery_night_receipts(id,namespace,default_version,candidate_hash,task_ids,status,created_at)
            VALUES(?1,?2,?3,?4,?5,'claimed',?6)",params![id,key,super::to_i64(expected_version,"default version")?,
                candidate_hash,serde_json::to_string(tasks)?,chrono::Utc::now().to_rfc3339()])?;
        tx.commit()?;
        Ok(Some(HoldoutReceipt {
            id,
            namespace_key: key,
            expected_version,
            candidate_hash,
            task_ids: tasks.to_vec(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::tree::Direction;
    use sha2::{Digest, Sha256};
    fn namespace() -> DefaultsNamespace {
        DefaultsNamespace {
            agent_id: "worker".into(),
            scorer_name: "score".into(),
            scorer_hash: "a".repeat(64),
            direction: Direction::Max,
            approved_root_id: "approved-project".into(),
            runtime: "claude".into(),
            configured_model: "haiku".into(),
        }
    }
    fn candidate() -> FrozenDefaults {
        let source = "class Policy: pass".to_string();
        FrozenDefaults {
            policy_id: "candidate".into(),
            source_sha256: Some(format!("{:x}", Sha256::digest(source.as_bytes()))),
            source: Some(source),
            beta: 0.6,
            knobs: Default::default(),
            origin_task_ids: vec!["development-task".into()],
        }
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn stale_night_compare_and_swap_never_overwrites_a_newer_durable_default() {
        let home = tempfile::tempdir().unwrap();
        let first = DiscoveryStore::open(home.path()).unwrap();
        let stale = DiscoveryStore::open(home.path()).unwrap();
        let ns = namespace();
        let promoted = first
            .compare_and_swap_discovery_default(
                &ns,
                0,
                &candidate(),
                &serde_json::json!({"test":true}),
            )
            .unwrap()
            .unwrap();
        assert_eq!(promoted.version, 1);
        assert!(
            stale
                .compare_and_swap_discovery_default(
                    &ns,
                    0,
                    &FrozenDefaults::default(),
                    &serde_json::json!({})
                )
                .unwrap()
                .is_none()
        );
        drop(first);
        drop(stale);
        let reopened = DiscoveryStore::open(home.path()).unwrap();
        assert_eq!(reopened.load_discovery_default(&ns).unwrap(), promoted);
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn defaults_do_not_cross_runtime_model_or_approved_root_namespaces() {
        let home = tempfile::tempdir().unwrap();
        let store = DiscoveryStore::open(home.path()).unwrap();
        let original = namespace();
        store
            .compare_and_swap_discovery_default(&original, 0, &candidate(), &serde_json::json!({}))
            .unwrap();
        for field in ["runtime", "model", "root"] {
            let mut other = original.clone();
            match field {
                "runtime" => other.runtime = "codex".into(),
                "model" => other.configured_model = "different".into(),
                _ => other.approved_root_id = "other-root".into(),
            }
            assert_eq!(store.load_discovery_default(&other).unwrap().version, 0);
        }
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn frozen_defaults_refuse_source_hash_mismatch_invalid_beta_and_unknown_knobs() {
        let home = tempfile::tempdir().unwrap();
        let store = DiscoveryStore::open(home.path()).unwrap();
        for mode in ["hash", "source", "beta", "knobs"] {
            let mut bad = candidate();
            match mode {
                "hash" => bad.source_sha256 = Some("f".repeat(64)),
                "source" => bad.source = None,
                "beta" => bad.beta = 2.0,
                _ => {
                    bad.knobs.insert("unbounded".into(), 1);
                }
            }
            assert!(
                store
                    .compare_and_swap_discovery_default(
                        &namespace(),
                        0,
                        &bad,
                        &serde_json::json!({})
                    )
                    .is_err(),
                "accepted {mode}"
            );
        }
        assert_eq!(
            store.load_discovery_default(&namespace()).unwrap().version,
            0
        );
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn fresh_holdout_is_consumed_before_evaluation_even_after_a_crash_or_rejection() {
        let home = tempfile::tempdir().unwrap();
        let store = DiscoveryStore::open(home.path()).unwrap();
        let tasks = (0..1000)
            .map(|n| format!("heldout-{n}"))
            .filter(|task| crate::discovery::night::is_heldout(task))
            .take(8)
            .collect::<Vec<_>>();
        let receipt = store
            .claim_night_holdout(&namespace(), 0, &candidate(), &tasks)
            .unwrap();
        assert!(receipt.is_some());
        drop(store); // Crash before any candidate outcome or default update.
        let restarted = DiscoveryStore::open(home.path()).unwrap();
        assert!(
            restarted
                .claim_night_holdout(&namespace(), 0, &candidate(), &tasks)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            restarted
                .load_discovery_default(&namespace())
                .unwrap()
                .version,
            0
        );
    }
    fn evidence(tasks: &[String]) -> crate::discovery::night::PromotionEvidence {
        crate::discovery::night::PromotionEvidence {
            eligible: true,
            heldout_tasks: tasks.len(),
            training_tasks: 8,
            tasks: 16,
            worlds: 16,
            heldout_mean_lift: Some(0.2),
            training_mean_lift: Some(0.2),
            heldout_strict_wins: 8,
            wilson_lower: Some(0.6),
            bonferroni_candidates: 1,
            reason: "fixture_verified_gate".into(),
        }
    }
    fn heldout_tasks() -> Vec<String> {
        (0..1000)
            .map(|n| format!("heldout-{n}"))
            .filter(|task| crate::discovery::night::is_heldout(task))
            .take(8)
            .collect()
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn night_receipt_cas_conflict_consumes_tasks_and_keeps_the_newer_default() {
        let home = tempfile::tempdir().unwrap();
        let store = DiscoveryStore::open(home.path()).unwrap();
        let tasks = heldout_tasks();
        let ns = namespace();
        let candidate = candidate();
        let receipt = store
            .claim_night_holdout(&ns, 0, &candidate, &tasks)
            .unwrap()
            .unwrap();
        let newer = store
            .compare_and_swap_discovery_default(
                &ns,
                0,
                &FrozenDefaults::default(),
                &serde_json::json!({}),
            )
            .unwrap()
            .unwrap();
        assert!(
            store
                .finish_night_holdout(&ns, &receipt, &candidate, &evidence(&tasks), || true)
                .unwrap()
                .is_none()
        );
        assert_eq!(store.load_discovery_default(&ns).unwrap(), newer);
        assert!(store.fresh_night_tasks(&ns, &tasks).unwrap().is_empty());
        assert!(
            store
                .finish_night_holdout(&ns, &receipt, &candidate, &evidence(&tasks), || true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn cancelled_night_does_not_adopt_or_release_previously_consumed_holdout() {
        let home = tempfile::tempdir().unwrap();
        let store = DiscoveryStore::open(home.path()).unwrap();
        let tasks = heldout_tasks();
        let ns = namespace();
        let candidate = candidate();
        let receipt = store
            .claim_night_holdout(&ns, 0, &candidate, &tasks)
            .unwrap()
            .unwrap();
        assert!(
            store
                .finish_night_holdout(&ns, &receipt, &candidate, &evidence(&tasks), || false)
                .unwrap()
                .is_none()
        );
        assert_eq!(store.load_discovery_default(&ns).unwrap().version, 0);
        assert!(store.fresh_night_tasks(&ns, &tasks).unwrap().is_empty());
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn successful_night_adoption_is_atomic_with_receipt_and_frozen_version_history() {
        let home = tempfile::tempdir().unwrap();
        let store = DiscoveryStore::open(home.path()).unwrap();
        let tasks = heldout_tasks();
        let ns = namespace();
        let candidate = candidate();
        let receipt = store
            .claim_night_holdout(&ns, 0, &candidate, &tasks)
            .unwrap()
            .unwrap();
        let adopted = store
            .finish_night_holdout(&ns, &receipt, &candidate, &evidence(&tasks), || true)
            .unwrap()
            .unwrap();
        assert_eq!(adopted.version, 1);
        drop(store);
        let reopened = DiscoveryStore::open(home.path()).unwrap();
        assert_eq!(reopened.load_discovery_default(&ns).unwrap(), adopted);
        assert!(reopened.fresh_night_tasks(&ns, &tasks).unwrap().is_empty());
        let report: String = reopened
            .conn
            .query_row(
                "SELECT report FROM discovery_default_versions WHERE version=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&report).unwrap()["heldout_tasks"],
            8
        );
    }
    // Host-verified fake worlds: no provider, Docker or filesystem evaluator
    // is involved. Direct SQL marks the fixture authority explicitly.
    fn census_worlds(store: &DiscoveryStore, task: &str, count: u32) {
        let limits = crate::discovery::contracts::RunBudget {
            max_agent_calls: 100,
            max_usd: 1.0,
            max_wall_secs: 30,
            max_rounds: 100,
        };
        let run = format!("run-{task}");
        store
            .create_run(
                &run,
                "census",
                "worker",
                "score",
                &"a".repeat(64),
                Direction::Max,
                &limits,
            )
            .unwrap();
        store.conn.execute("UPDATE discovery_runs SET task_id=?2,creator_id='fixture',creator_origin='fixture',
            approved_root_id='approved-project',runtime='claude',configured_model='haiku',status='complete',
            provenance_verified=1,created_at='2026-09-30T00:00:00Z' WHERE run_id=?1",params![run,task]).unwrap();
        for round in 1..=count {
            store
                .insert_world(&crate::discovery::tree::World {
                    schema: crate::discovery::tree::WORLD_SCHEMA.into(),
                    run_id: run.clone(),
                    round,
                    direction: Direction::Max,
                    baseline_score: 0.0,
                    branch_count: 1,
                    refine_count: 0,
                    max_parallelism: 1,
                    policy_id: crate::discovery::policy::BASELINE_POLICY_ID.into(),
                    beta: 0.6,
                })
                .unwrap();
            let node = serde_json::from_value(serde_json::json!({"run_id":run,"round":round,
                "cell_id":format!("r{round}-b0-a0"),"branch":0,"attempt":0,"seq":1,
                "evaluated":true,"valid":true,"score":1.0,"fail_class":"ok","visible_set":[]}))
            .unwrap();
            store.insert_nodes(&[node]).unwrap();
            store.conn.execute("UPDATE discovery_rounds SET policy_params='{}',full_grid=1,completion='complete'
                WHERE run_id=?1 AND round=?2",params![run,round]).unwrap();
        }
    }
    fn census_tasks(heldout: bool, count: usize) -> Vec<String> {
        (0..10000)
            .map(|n| format!("census-{n}"))
            .filter(|task| crate::discovery::night::is_heldout(task) == heldout)
            .take(count)
            .collect()
    }
    fn selection(snapshot: &NightSnapshot) -> std::collections::BTreeSet<(String, u32)> {
        snapshot
            .worlds
            .iter()
            .filter_map(|world| world.tree.as_ref())
            .map(|tree| (tree.world().run_id.clone(), tree.world().round))
            .collect()
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn long_corpus_uses_bounded_complete_task_sample_independent_of_scores() {
        let home = tempfile::tempdir().unwrap();
        let store = DiscoveryStore::open(home.path()).unwrap();
        let tasks = census_tasks(false, 24)
            .into_iter()
            .chain(census_tasks(true, 24))
            .collect::<Vec<_>>();
        for task in &tasks {
            census_worlds(&store, task, 1);
        }
        let first = store.night_snapshot("worker").unwrap();
        assert_eq!(first.observed_worlds, 48);
        assert!(
            !first.worlds.is_empty(),
            "long corpus must continue learning from a deterministic bounded window"
        );
        assert!(first.worlds.len() <= 32);
        assert_eq!(
            first.selection_exclusions.get("task_group_window_capacity"),
            Some(&16)
        );
        let training = first
            .worlds
            .iter()
            .filter(|world| {
                !crate::discovery::night::is_heldout(world.run.task_id.as_deref().unwrap())
            })
            .count();
        assert!(training >= 8 && first.worlds.len() - training >= 8);
        store
            .conn
            .execute("UPDATE discovery_nodes SET score=1000000-score", [])
            .unwrap();
        let second = store.night_snapshot("worker").unwrap();
        assert_eq!(
            selection(&first),
            selection(&second),
            "scores must never change sample membership"
        );
        drop(store);
        let reopened = DiscoveryStore::open(home.path()).unwrap();
        assert_eq!(
            selection(&first),
            selection(&reopened.night_snapshot("worker").unwrap())
        );
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn long_corpus_never_splits_a_task_group_and_skips_oversized_task() {
        let home = tempfile::tempdir().unwrap();
        let store = DiscoveryStore::open(home.path()).unwrap();
        census_worlds(&store, "oversized", 33);
        for task in census_tasks(false, 8)
            .into_iter()
            .chain(census_tasks(true, 8))
        {
            census_worlds(&store, &task, 2);
        }
        let snapshot = store.night_snapshot("worker").unwrap();
        assert_eq!(snapshot.observed_worlds, 65);
        assert!(!snapshot.worlds.is_empty());
        assert!(snapshot.worlds.len() <= 32);
        let mut counts = std::collections::BTreeMap::new();
        for world in snapshot.worlds {
            *counts.entry(world.run.task_id.unwrap()).or_insert(0usize) += 1;
        }
        assert!(!counts.contains_key("oversized"));
        assert_eq!(counts.len(), 16);
        assert!(
            counts.values().all(|count| *count == 2),
            "a sampled task must retain every world in the window"
        );
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn long_corpus_selection_never_reuses_previously_claimed_heldout_tasks() {
        let home = tempfile::tempdir().unwrap();
        let store = DiscoveryStore::open(home.path()).unwrap();
        for task in census_tasks(false, 24)
            .into_iter()
            .chain(census_tasks(true, 24))
        {
            census_worlds(&store, &task, 1);
        }
        let consumed = census_tasks(true, 8);
        store
            .claim_night_holdout(&namespace(), 0, &candidate(), &consumed)
            .unwrap()
            .unwrap();
        let snapshot = store.night_snapshot("worker").unwrap();
        assert!(!snapshot.worlds.is_empty());
        assert!(
            snapshot
                .worlds
                .iter()
                .all(|world| !consumed.contains(world.run.task_id.as_ref().unwrap()))
        );
        assert_eq!(
            snapshot
                .selection_exclusions
                .get("heldout_already_consumed"),
            Some(&8)
        );
        let heldout = snapshot
            .worlds
            .iter()
            .filter(|world| {
                crate::discovery::night::is_heldout(world.run.task_id.as_deref().unwrap())
            })
            .count();
        assert!(heldout >= 8);
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn candidate_overflow_is_report_only_before_removing_the_builtin_incumbent() {
        let home = tempfile::tempdir().unwrap();
        let store = DiscoveryStore::open(home.path()).unwrap();
        let training = census_tasks(false, 8);
        let heldout = census_tasks(true, 8);
        for task in training.iter().chain(&heldout) {
            census_worlds(&store, task, 1);
        }
        let run = format!("run-{}", training[0]);
        let tree = crate::discovery::tree::WorldTree::new(
            store.load_world(&run, 1).unwrap().unwrap(),
            store.load_nodes(&run, 1).unwrap(),
        ).unwrap();
        let score = crate::discovery::eval::evaluate_world(
            &|_| Box::new(crate::discovery::policy::BaselineParallelRefine),
            &tree,
            &crate::discovery::eval::ReplayConfig::for_world(&tree),
        ).unwrap();
        let evaluation = crate::discovery::policy_runner::CandidateEvaluation {
            valid: true, violation: None, value: Some(score.pareto_reward),
            context_mismatch_rate: Some(0.0), worlds: vec![score],
        };
        let mut builtin = serde_json::to_value(FrozenDefaults::default()).unwrap();
        builtin["source"] = crate::discovery::policy_runner::BASELINE_SOURCE.into();
        builtin["source_sha256"] = format!("{:x}", Sha256::digest(
            crate::discovery::policy_runner::BASELINE_SOURCE.as_bytes())).into();
        store.record_candidate_evaluation_with_origin(
            crate::discovery::policy::BASELINE_POLICY_ID, &builtin.to_string(),
            &[tree.clone()], &evaluation, &[], "builtin",
        ).unwrap();
        for index in 0..9 {
            let beta = 0.1 + index as f64 * 0.08;
            let source = crate::discovery::policy_runner::BASELINE_SOURCE
                .replace("self.beta = 0.6", &format!("self.beta = {beta}"));
            let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
            let candidate = FrozenDefaults {
                policy_id: format!("llm-{}", &hash[..16]), source: Some(source),
                source_sha256: Some(hash), beta, knobs: Default::default(),
                origin_task_ids: vec![training[0].clone()],
            };
            store.record_candidate_evaluation_with_origin(
                &candidate.policy_id, &serde_json::to_string(&candidate).unwrap(),
                &[tree.clone()], &evaluation, &candidate.origin_task_ids, "task_development",
            ).unwrap();
        }
        assert!(store.night_candidates(&namespace()).is_err(),
            "a truncated catalog must fail before the consumer removes the incumbent");
        drop(store);
        let report = crate::discovery::night::run_with_budget(
            home.path(), "worker", crate::discovery::night::night_budget(),
        ).unwrap();
        assert_eq!(report.namespaces.len(), 1);
        let item = &report.namespaces[0];
        assert_eq!(item.reason, "candidate_query_unavailable");
        assert!(!item.adopted);
        assert_eq!(item.candidates, 0);
        assert_eq!(report.cli_invocations, 0);
        let reopened = DiscoveryStore::open(home.path()).unwrap();
        assert_eq!(reopened.load_discovery_default(&namespace()).unwrap().version, 0);
        assert_eq!(reopened.fresh_night_tasks(&namespace(), &heldout).unwrap().len(), 8);
    }
    #[test]
    #[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
    fn repeated_development_occurrences_do_not_hide_a_distinct_frozen_policy() {
        let home = tempfile::tempdir().unwrap();
        let store = DiscoveryStore::open(home.path()).unwrap();
        let task = census_tasks(false, 1).remove(0);
        census_worlds(&store, &task, 1);
        let run = format!("run-{task}");
        let tree = crate::discovery::tree::WorldTree::new(
            store.load_world(&run, 1).unwrap().unwrap(),
            store.load_nodes(&run, 1).unwrap(),
        )
        .unwrap();
        let score = crate::discovery::eval::evaluate_world(
            &|_| Box::new(crate::discovery::policy::BaselineParallelRefine),
            &tree,
            &crate::discovery::eval::ReplayConfig::for_world(&tree),
        )
        .unwrap();
        let evaluation = crate::discovery::policy_runner::CandidateEvaluation {
            valid: true,
            violation: None,
            value: Some(score.pareto_reward),
            context_mismatch_rate: Some(0.0),
            worlds: vec![score],
        };
        let mut deployments = [0.8, 0.9]
            .into_iter()
            .map(|beta| {
                let source = crate::discovery::policy_runner::BASELINE_SOURCE
                    .replace("self.beta = 0.6", &format!("self.beta = {beta}"));
                let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
                FrozenDefaults {
                    policy_id: format!("llm-{}", &hash[..16]),
                    source: Some(source),
                    source_sha256: Some(hash),
                    beta,
                    knobs: Default::default(),
                    origin_task_ids: vec![task.clone()],
                }
            })
            .collect::<Vec<_>>();
        deployments.sort_by(|left, right| left.policy_id.cmp(&right.policy_id));
        for (deployment, occurrences) in [(&deployments[0], 12), (&deployments[1], 1)] {
            for revision in 0..occurrences {
                let occurrence = format!(
                    "{run}:r1:v{revision}:{}",
                    deployment.source_sha256.as_deref().unwrap()
                );
                let mut params = serde_json::to_value(deployment).unwrap();
                params["occurrence_id"] = occurrence.clone().into();
                params["revision"] = revision.into();
                params["development_run_id"] = run.clone().into();
                params["after_round"] = 1.into();
                store
                    .record_candidate_evaluation_with_occurrence(
                        &deployment.policy_id,
                        Some(&occurrence),
                        &params.to_string(),
                        &[tree.clone()],
                        &evaluation,
                        &deployment.origin_task_ids,
                        "task_development",
                    )
                    .unwrap();
            }
        }
        let found = store.night_candidates(&namespace()).unwrap();
        assert_eq!(
            found.len(),
            2,
            "occurrence metadata must not consume distinct deployment slots"
        );
        for deployment in &deployments {
            assert!(found.contains(deployment));
        }
        drop(store);
        let reopened = DiscoveryStore::open(home.path()).unwrap();
        assert_eq!(reopened.night_candidates(&namespace()).unwrap(), found);
    }
}
