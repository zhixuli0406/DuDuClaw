//! The read path: validating a saved original against its bound source before
//! returning it. Moved verbatim out of `ccr.rs`.

use super::*;

impl CcrStore {
    pub(super) fn validated_saved_original(
        &self,
        scope: &CcrScope,
        id: &str,
        source_tool: &str,
        source_call_id: &str,
        original_bytes: usize,
    ) -> Result<Option<String>, CcrError> {
        if !scope.valid()
            || id.trim().is_empty()
            || source_tool.is_empty()
            || source_call_id.is_empty()
        {
            return Ok(None);
        }
        let conn = self.open()?;
        let saved: Option<(String, String, i64, i64)> = conn.query_row(
            "SELECT original, content_sha256, content_bytes, transform_version
             FROM ccr_entries WHERE id=?1 AND tenant_id=?2 AND agent_id=?3
             AND session_id=?4 AND source_acl=?5 AND source_tool=?6 AND source_call_id=?7
             AND expires_at>?8
             AND (binding_required=0 OR EXISTS (SELECT 1 FROM ccr_artifact_bindings b
                  WHERE b.entry_id=ccr_entries.id AND b.tenant_id=ccr_entries.tenant_id))
             AND NOT EXISTS (SELECT 1 FROM ccr_artifact_bindings b
                  WHERE b.entry_id=ccr_entries.id AND b.tenant_id!=ccr_entries.tenant_id)
             AND NOT EXISTS (SELECT 1 FROM ccr_revoked_scopes r WHERE
                r.tenant_id=ccr_entries.tenant_id AND r.agent_id=ccr_entries.agent_id
                AND r.session_id=ccr_entries.session_id AND r.source_acl=ccr_entries.source_acl)
             AND NOT EXISTS (SELECT 1 FROM ccr_revoked_sources r WHERE
                r.tenant_id=ccr_entries.tenant_id AND r.agent_id=ccr_entries.agent_id
                AND r.session_id=ccr_entries.session_id AND r.source_acl=ccr_entries.source_acl
                AND r.source_tool=ccr_entries.source_tool AND r.source_call_id=ccr_entries.source_call_id)
             AND NOT EXISTS (SELECT 1 FROM ccr_artifact_bindings b
                JOIN ccr_revoked_artifact_versions r ON r.tenant_id=b.tenant_id
                 AND r.connector=b.connector AND r.artifact_id=b.artifact_id AND r.version=b.version
                WHERE b.entry_id=ccr_entries.id)",
            params![id, scope.tenant_id, scope.agent_id, scope.session_id, scope.source_acl,
                source_tool, source_call_id, unix_now()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).optional()?;
        Ok(saved.and_then(|(original, digest, bytes, version)| {
            (version == CCR_ENTRY_VERSION
                && i64::try_from(original_bytes).ok() == Some(bytes)
                && bytes == original.len() as i64
                && original.len() <= MAX_ORIGINAL_BYTES
                && digest == format!("{:x}", Sha256::digest(original.as_bytes())))
            .then_some(original)
        }))
    }

    pub(super) fn valid_saved_reference(
        &self,
        scope: &CcrScope,
        id: &str,
        source_tool: &str,
        source_call_id: &str,
        original_bytes: usize,
    ) -> Result<bool, CcrError> {
        Ok(self
            .validated_saved_original(scope, id, source_tool, source_call_id, original_bytes)?
            .is_some())
    }

    /// Return only a bounded UTF-8 fragment. Scope comparison is exact and
    /// happens in SQL, so a guessed or leaked ID grants no cross-scope read.
    pub fn retrieve(
        &self,
        scope: &CcrScope,
        id: &str,
        query: Option<&str>,
        offset: usize,
        max_bytes: usize,
    ) -> Result<RetrievedChunk, CcrError> {
        self.retrieve_with_validated_source(scope, id, query, offset, max_bytes, None)
    }

    /// Only the runtime may supply a binding it has just checked against the
    /// application-owned source. A public store read has no such authority.
    pub(super) fn retrieve_with_validated_source(
        &self,
        scope: &CcrScope,
        id: &str,
        query: Option<&str>,
        offset: usize,
        max_bytes: usize,
        validated_source: Option<&CcrBoundSource>,
    ) -> Result<RetrievedChunk, CcrError> {
        if !scope.valid() {
            return Err(CcrError::InvalidScope);
        }
        if id.trim().is_empty() {
            self.record_retrieval_refusal(scope, id)?;
            return Err(CcrError::InvalidScope);
        }
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let original: Option<(String, String, i64, i64, i64, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT original, content_sha256, content_bytes, transform_version, binding_required,
                        b.tenant_id,b.connector,b.artifact_id,b.version,b.acl_revision
                 FROM ccr_entries LEFT JOIN ccr_artifact_bindings b ON b.entry_id=ccr_entries.id
                 WHERE id = ?1 AND ccr_entries.tenant_id = ?2
                 AND agent_id = ?3 AND session_id = ?4 AND source_acl = ?5 AND expires_at > ?6
                 AND NOT EXISTS (SELECT 1 FROM ccr_revoked_scopes r WHERE
                    r.tenant_id=ccr_entries.tenant_id AND r.agent_id=ccr_entries.agent_id
                    AND r.session_id=ccr_entries.session_id AND r.source_acl=ccr_entries.source_acl)
                 AND NOT EXISTS (SELECT 1 FROM ccr_revoked_sources r WHERE
                    r.tenant_id=ccr_entries.tenant_id AND r.agent_id=ccr_entries.agent_id
                    AND r.session_id=ccr_entries.session_id AND r.source_acl=ccr_entries.source_acl
                    AND r.source_tool=ccr_entries.source_tool AND r.source_call_id=ccr_entries.source_call_id)
                 AND NOT EXISTS (SELECT 1 FROM ccr_artifact_bindings b
                    JOIN ccr_revoked_artifact_versions r ON r.tenant_id=b.tenant_id
                     AND r.connector=b.connector AND r.artifact_id=b.artifact_id AND r.version=b.version
                    WHERE b.entry_id=ccr_entries.id)",
                params![
                    id,
                    scope.tenant_id,
                    scope.agent_id,
                    scope.session_id,
                    scope.source_acl,
                    unix_now()
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                    row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?)),
            )
            .optional()?;
        let authorized = original.as_ref().is_none_or(
            |(_, digest, _, _, required, tenant, connector, artifact_id, version, acl_revision)| {
                match validated_source {
                    None => {
                        *required == 0
                            && tenant.is_none()
                            && connector.is_none()
                            && artifact_id.is_none()
                            && version.is_none()
                            && acl_revision.is_none()
                    }
                    Some(bound) => {
                        (*required == 0 || *required == 1)
                            && tenant.as_deref() == Some(scope.tenant_id.as_str())
                            && connector.as_deref() == Some(bound.artifact.connector.as_str())
                            && artifact_id.as_deref() == Some(bound.artifact.artifact_id.as_str())
                            && version.as_deref() == Some(bound.artifact.version.as_str())
                            && acl_revision.as_deref() == Some(bound.artifact.acl_revision.as_str())
                            && digest == &bound.saved_sha256
                    }
                }
            },
        );
        let corrupt = original
            .as_ref()
            .is_some_and(|(text, digest, bytes, version, ..)| {
                *version != CCR_ENTRY_VERSION
                    || *bytes != text.len() as i64
                    || text.len() > MAX_ORIGINAL_BYTES
                    || *digest != format!("{:x}", Sha256::digest(text.as_bytes()))
            });
        let result = original
            .filter(|_| authorized && !corrupt)
            .and_then(|(original, ..)| {
                let mut start = offset.min(original.len());
                while !original.is_char_boundary(start) {
                    start -= 1;
                }
                if let Some(q) = query.filter(|q| !q.is_empty()) {
                    start += original[start..].find(q)?;
                }
                let cap = max_bytes.clamp(1, MAX_RETURN_BYTES);
                let mut end = start.saturating_add(cap).min(original.len());
                while !original.is_char_boundary(end) {
                    end -= 1;
                }
                Some(RetrievedChunk {
                    text: original[start..end].to_owned(),
                    byte_offset: start,
                    total_bytes: original.len(),
                    truncated: end < original.len(),
                    delivery_guard: None,
                })
            });
        let (status, returned_bytes) = match &result {
            Some(chunk) => ("granted", chunk.text.len() as i64),
            None => ("refused", 0),
        };
        tx.execute(
            "INSERT INTO ccr_retrieval_audit
             (requested_id_sha256, tenant_id, agent_id, session_id, source_acl,
              status, returned_bytes, attempted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                format!("{:x}", Sha256::digest(id.as_bytes())),
                scope.tenant_id,
                scope.agent_id,
                scope.session_id,
                scope.source_acl,
                status,
                returned_bytes,
                unix_now()
            ],
        )?;
        tx.execute(
            "DELETE FROM ccr_retrieval_audit WHERE audit_id IN
             (SELECT audit_id FROM ccr_retrieval_audit ORDER BY audit_id DESC LIMIT -1 OFFSET 10000)",
            [],
        )?;
        tx.commit()?;
        if !authorized {
            return Err(CcrError::Revoked);
        }
        if corrupt {
            return Err(CcrError::Corrupt);
        }
        result.ok_or(CcrError::NotFound)
    }
}
