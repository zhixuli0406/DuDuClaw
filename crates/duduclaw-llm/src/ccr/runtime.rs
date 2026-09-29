//! The model-facing runtime: the source-binding / delivery-lease traits, their
//! guards, and [`CcrRuntime`] itself. Moved verbatim out of `ccr.rs`.

use super::*;
use super::find::{find_terms, historical_task_score, lexical_find_rank};
use super::preview::{
    compact_json_lines, diagnostic_rank, is_bracket_delimited, markdown_section_highlights,
    strip_json_whitespace, tabular_outlier_rows,
};

/// Optional application-owned check for a saved, source-bound original.
/// A false result also covers an unavailable source; callers must fail closed.
pub trait CcrBoundSourceValidator: std::fmt::Debug + Send + Sync {
    fn valid(&self, scope: &CcrScope, artifact: &CcrSourceArtifact, saved_sha256: &str) -> bool;

    /// Acquire source-owned delivery protection after the original is read.
    /// The authority must atomically recheck the exact binding while taking
    /// its lease. Sources without a transactional lease keep read-time checks.
    fn acquire_delivery_guard(
        &self,
        _scope: &CcrScope,
        _artifact: &CcrSourceArtifact,
        _saved_sha256: &str,
    ) -> Result<Option<Arc<dyn CcrDeliveryLease>>, CcrError> {
        Ok(None)
    }
}

/// A source-owned guard retained while a retrieved original can be sent.
/// `still_valid` also covers retention expiry and authority loss.
pub trait CcrDeliveryLease: std::fmt::Debug + Send + Sync {
    fn still_valid(&self) -> bool;
}

#[derive(Debug)]
struct SourceCallDeliveryGuard {
    source: Option<Arc<dyn CcrDeliveryLease>>,
    validator: Arc<dyn CcrBoundSourceValidator>,
    artifact: CcrSourceArtifact,
    saved_sha256: String,
    store: CcrStore,
    scope: CcrScope,
    source_tool: String,
    source_call_id: String,
}

impl CcrDeliveryLease for SourceCallDeliveryGuard {
    fn still_valid(&self) -> bool {
        self.source
            .as_ref()
            .is_none_or(|source| source.still_valid())
            && self
                .validator
                .valid(&self.scope, &self.artifact, &self.saved_sha256)
            && self
                .store
                .ensure_source_call_active(&self.scope, &self.source_tool, &self.source_call_id)
                .is_ok()
    }
}

#[derive(Debug)]
struct SourceOnlyDeliveryGuard {
    source: Option<Arc<dyn CcrDeliveryLease>>,
    validator: Arc<dyn CcrBoundSourceValidator>,
    artifact: CcrSourceArtifact,
    saved_sha256: String,
    scope: CcrScope,
}

impl CcrDeliveryLease for SourceOnlyDeliveryGuard {
    fn still_valid(&self) -> bool {
        self.source
            .as_ref()
            .is_none_or(|source| source.still_valid())
            && self
                .validator
                .valid(&self.scope, &self.artifact, &self.saved_sha256)
    }
}

#[derive(Debug)]
struct EntryDeliveryGuard {
    source_call: Arc<dyn CcrDeliveryLease>,
    store: CcrStore,
    scope: CcrScope,
    id: String,
    source_tool: String,
    source_call_id: String,
    original_bytes: usize,
    bound: CcrBoundSource,
}

impl CcrDeliveryLease for EntryDeliveryGuard {
    fn still_valid(&self) -> bool {
        self.source_call.still_valid()
            && self
                .store
                .valid_saved_reference(
                    &self.scope,
                    &self.id,
                    &self.source_tool,
                    &self.source_call_id,
                    self.original_bytes,
                )
                .unwrap_or(false)
            && matches!(self.store.bound_source_for_id(&self.scope, &self.id),
                Ok(Some(current)) if current == self.bound)
    }
}

/// One invocation's retrieval authority. Cloneable so the tool loop can use
/// `spawn_blocking` without performing SQLite work on the async runtime.
#[derive(Debug, Clone)]
pub struct CcrRuntime {
    pub store: CcrStore,
    pub scope: CcrScope,
    pub min_compress_bytes: usize,
    allowed_source_keys: Option<HashSet<String>>,
    bound_source_validator: Option<Arc<dyn CcrBoundSourceValidator>>,
    /// Protected-section sentinel of the *embedding* process (W2-E, review
    /// finding 4 / P3). This crate cannot mint one of its own: a sentinel is
    /// per-process, and a `CcrRuntime` built inside a CLI subprocess must not
    /// be able to claim the gateway's exemption. `None` — the default — means
    /// no section is protected and `preview_with_query` compresses normally.
    protected_sentinel: Option<String>,
}

impl CcrRuntime {
    pub fn new(store: CcrStore, scope: CcrScope) -> Self {
        Self {
            store,
            scope,
            min_compress_bytes: 4_096,
            allowed_source_keys: Some(HashSet::new()),
            bound_source_validator: None,
            protected_sentinel: None,
        }
    }

    /// Teach this runtime which sentinel marks a never-trim section, so
    /// [`Self::preview_with_query`] leaves such a result uncompressed.
    ///
    /// The embedding process passes
    /// `duduclaw_core::protected_section::process_sentinel()`. An empty or
    /// blank value is treated as "no sentinel" — the exemption stays off
    /// rather than matching a degenerate marker.
    pub fn with_protected_sentinel(mut self, sentinel: &str) -> Self {
        self.protected_sentinel = Some(sentinel.to_owned()).filter(|s| !s.trim().is_empty());
        self
    }

    #[cfg(test)]
    pub(crate) fn new_unrestricted_for_test(store: CcrStore, scope: CcrScope) -> Self {
        let mut runtime = Self::new(store, scope);
        runtime.allowed_source_keys = None;
        runtime
    }

    /// Gate CCR storage and retrieval on registry-owned server/tool routes.
    /// An empty allowlist denies every source. Callers must opt in with routes
    /// resolved by their trusted executor before exposing retrieval tools.
    pub fn restrict_sources(mut self, sources: impl IntoIterator<Item = (String, String)>) -> Self {
        self.allowed_source_keys = Some(
            sources
                .into_iter()
                .filter(|(server, tool)| !server.trim().is_empty() && !tool.trim().is_empty())
                .map(|(server, tool)| Self::source_key(&server, &tool))
                .collect(),
        );
        self
    }

    pub fn with_bound_source_validator(
        mut self,
        validator: Arc<dyn CcrBoundSourceValidator>,
    ) -> Self {
        self.bound_source_validator = Some(validator);
        self
    }

    fn bound_source_valid(&self, bound: Option<&CcrBoundSource>) -> bool {
        match (&self.bound_source_validator, bound) {
            (Some(validator), Some(bound)) => {
                validator.valid(&self.scope, &bound.artifact, &bound.saved_sha256)
            }
            (None, Some(_)) => false,
            _ => true,
        }
    }

    /// Protect the first delivery of a connector-verified tool result. The
    /// source authority must recheck its exact current bytes while acquiring
    /// a lease; the caller retains the returned guard through model egress.
    pub fn acquire_source_delivery_guard(
        &self,
        artifact: &CcrSourceArtifact,
        saved_sha256: &str,
        source_tool: &str,
        source_call_id: &str,
    ) -> Result<Arc<dyn CcrDeliveryLease>, CcrError> {
        let validator = self
            .bound_source_validator
            .as_ref()
            .ok_or(CcrError::Revoked)?;
        if !validator.valid(&self.scope, artifact, saved_sha256) {
            return Err(CcrError::Revoked);
        }
        let guard = validator.acquire_delivery_guard(&self.scope, artifact, saved_sha256)?;
        if guard.as_ref().is_some_and(|guard| !guard.still_valid())
            || !validator.valid(&self.scope, artifact, saved_sha256)
        {
            return Err(CcrError::Revoked);
        }
        let scoped: Arc<dyn CcrDeliveryLease> = Arc::new(SourceCallDeliveryGuard {
            source: guard,
            validator: validator.clone(),
            artifact: artifact.clone(),
            saved_sha256: saved_sha256.to_owned(),
            store: self.store.clone(),
            scope: self.scope.clone(),
            source_tool: source_tool.to_owned(),
            source_call_id: source_call_id.to_owned(),
        });
        if !scoped.still_valid() {
            return Err(CcrError::Revoked);
        }
        Ok(scoped)
    }

    /// Guard a verified source whose bytes were changed by a trusted output
    /// interceptor. The call handle is retired because the transformed result
    /// cannot become a CCR original, but source validity must still protect
    /// its delivery through the model and final channel send.
    pub fn acquire_transformed_source_delivery_guard(
        &self,
        artifact: &CcrSourceArtifact,
        saved_sha256: &str,
    ) -> Result<Arc<dyn CcrDeliveryLease>, CcrError> {
        let validator = self
            .bound_source_validator
            .as_ref()
            .ok_or(CcrError::Revoked)?;
        if !validator.valid(&self.scope, artifact, saved_sha256) {
            return Err(CcrError::Revoked);
        }
        let source = validator.acquire_delivery_guard(&self.scope, artifact, saved_sha256)?;
        let guard: Arc<dyn CcrDeliveryLease> = Arc::new(SourceOnlyDeliveryGuard {
            source,
            validator: validator.clone(),
            artifact: artifact.clone(),
            saved_sha256: saved_sha256.to_owned(),
            scope: self.scope.clone(),
        });
        if !guard.still_valid() {
            return Err(CcrError::Revoked);
        }
        Ok(guard)
    }

    fn source_key(server: &str, tool: &str) -> String {
        serde_json::to_string(&(server, tool)).expect("source key contains only strings")
    }

    /// Returns the exact store/revocation key when this trusted route is
    /// allowed. `server` must come from the executor registry, not tool text.
    pub fn source_key_for_call(&self, server: Option<&str>, tool: &str) -> Option<String> {
        if tool.trim().is_empty() {
            return None;
        }
        match &self.allowed_source_keys {
            None => Some(tool.to_owned()),
            Some(allowed) => {
                let key = Self::source_key(server.filter(|s| !s.trim().is_empty())?, tool);
                allowed.contains(&key).then_some(key)
            }
        }
    }

    /// Read-time policy check prevents a removed allowlist route from serving
    /// old saved originals. The store still verifies scope, expiry and digest.
    pub fn retrieve(
        &self,
        id: &str,
        query: Option<&str>,
        offset: usize,
        max_bytes: usize,
    ) -> Result<RetrievedChunk, CcrError> {
        let initial_source = self.store.source_tool_for_id(&self.scope, id)?;
        if let Some(allowed) = &self.allowed_source_keys {
            if !initial_source
                .as_ref()
                .is_some_and(|source| allowed.contains(source))
            {
                self.store.record_retrieval_refusal(&self.scope, id)?;
                return Err(CcrError::NotFound);
            }
        }
        let bound = match self.store.bound_source_for_id(&self.scope, id) {
            Ok(bound) => bound,
            Err(CcrError::Revoked) => {
                self.store.record_retrieval_refusal(&self.scope, id)?;
                return Err(CcrError::Revoked);
            }
            Err(error) => return Err(error),
        };
        if !self.bound_source_valid(bound.as_ref()) {
            self.store.record_retrieval_refusal(&self.scope, id)?;
            return Err(CcrError::Revoked);
        }
        let mut chunk = self.store.retrieve_with_validated_source(
            &self.scope,
            id,
            query,
            offset,
            max_bytes,
            bound.as_ref(),
        )?;
        // Both the upstream source and the saved binding may change while
        // the CCR store reads its bytes. The final check precedes a source
        // guard whose acquisition rechecks the exact current binding.
        let source_valid = self.bound_source_valid(bound.as_ref());
        let source_unchanged = initial_source.is_some()
            && self.store.source_tool_for_id(&self.scope, id)? == initial_source;
        let binding_unchanged = match self.store.bound_source_for_id(&self.scope, id) {
            Ok(current) => current == bound,
            Err(CcrError::Revoked) => false,
            Err(error) => return Err(error),
        };
        if !source_valid || !source_unchanged || !binding_unchanged {
            self.store.record_retrieval_refusal(&self.scope, id)?;
            return Err(CcrError::Revoked);
        }
        if let Some(bound) = &bound {
            let source_tool = initial_source.as_deref().ok_or(CcrError::Revoked)?;
            let source_call_id = self
                .store
                .source_call_id_for_id(&self.scope, id)?
                .ok_or(CcrError::Revoked)?;
            let source_guard = match self.acquire_source_delivery_guard(
                &bound.artifact,
                &bound.saved_sha256,
                source_tool,
                &source_call_id,
            ) {
                Ok(guard) => guard,
                Err(error) => {
                    self.store.record_retrieval_refusal(&self.scope, id)?;
                    return Err(error);
                }
            };
            let guard: Arc<dyn CcrDeliveryLease> = Arc::new(EntryDeliveryGuard {
                source_call: source_guard,
                store: self.store.clone(),
                scope: self.scope.clone(),
                id: id.to_owned(),
                source_tool: source_tool.to_owned(),
                source_call_id,
                original_bytes: chunk.total_bytes,
                bound: bound.clone(),
            });
            if !guard.still_valid() {
                self.store.record_retrieval_refusal(&self.scope, id)?;
                return Err(CcrError::Revoked);
            }
            chunk.delivery_guard = Some(guard);
        }
        Ok(chunk)
    }

    /// Recheck a saved session reference without reading original content or
    /// recording a retrieval grant. A displayed marker is only a hint; the
    /// actual retrieval repeats all authorization and integrity checks.
    pub fn valid_saved_reference(
        &self,
        scope: &CcrScope,
        id: &str,
        source_tool: &str,
        source_call_id: &str,
        original_bytes: usize,
    ) -> Result<bool, CcrError> {
        if scope != &self.scope
            || !self
                .allowed_source_keys
                .as_ref()
                .is_some_and(|allowed| allowed.contains(source_tool))
        {
            return Ok(false);
        }
        if !self.store.valid_saved_reference(
            scope,
            id,
            source_tool,
            source_call_id,
            original_bytes,
        )? {
            return Ok(false);
        }
        let bound = match self.store.bound_source_for_id(scope, id) {
            Ok(bound) => bound,
            Err(CcrError::Revoked) => return Ok(false),
            Err(error) => return Err(error),
        };
        if !self.bound_source_valid(bound.as_ref()) {
            return Ok(false);
        }
        Ok(self.store.source_tool_for_id(scope, id)?.as_deref() == Some(source_tool))
    }

    /// Return only a deterministic relevance score for a saved handle. The
    /// original stays inside the CCR store and never enters the prompt or the
    /// gateway ranking layer. This repeats the same exact-scope, route,
    /// expiry, integrity, tombstone, and upstream-source checks as a displayed
    /// historical handle, including a second check after reading the text.
    pub fn saved_reference_task_score(
        &self,
        scope: &CcrScope,
        id: &str,
        source_tool: &str,
        source_call_id: &str,
        original_bytes: usize,
        task: &str,
    ) -> Result<Option<u32>, CcrError> {
        if !self.valid_saved_reference(scope, id, source_tool, source_call_id, original_bytes)? {
            return Ok(None);
        }
        let Some(original) = self.store.validated_saved_original(
            scope,
            id,
            source_tool,
            source_call_id,
            original_bytes,
        )?
        else {
            return Ok(None);
        };
        let score = historical_task_score(&original, task);
        self.valid_saved_reference(scope, id, source_tool, source_call_id, original_bytes)
            .map(|valid| valid.then_some(score))
    }

    /// Discover saved handles on demand without adding them to the prompt.
    /// The current route allowlist is checked again on every search.
    pub fn find(&self, query: &str, limit: usize) -> Result<Vec<CcrFindHit>, CcrError> {
        Ok(self.find_with_status(query, limit)?.hits)
    }

    /// Also report when the bounded upstream validation budget hid additional
    /// candidates. An empty hit list in that case does not prove no match exists.
    pub fn find_with_status(&self, query: &str, limit: usize) -> Result<CcrFindReport, CcrError> {
        let candidates = self
            .store
            .find(&self.scope, query, self.allowed_source_keys.as_ref())?;
        let mut cache = HashMap::new();
        let mut hits = Vec::new();
        let mut source_validation_limited = false;
        for (hit, bound) in candidates {
            let valid = match bound.as_ref() {
                Some(bound) if cache.contains_key(bound) => cache[bound],
                Some(bound) if cache.len() < MAX_BOUND_FIND_VALIDATIONS => {
                    let valid = self.bound_source_valid(Some(bound));
                    cache.insert(bound.clone(), valid);
                    valid
                }
                Some(_) => {
                    source_validation_limited = true;
                    false
                }
                None => true,
            };
            if valid {
                hits.push(hit);
                if hits.len() == limit.clamp(1, 5) {
                    break;
                }
            }
        }
        Ok(CcrFindReport {
            hits,
            source_validation_limited,
        })
    }

    /// Permanently scrub handles for routes removed from this caller's trusted
    /// allowlist. The gateway calls this before exposing retrieval on a turn.
    pub fn purge_disallowed_sources(&self) -> Result<usize, CcrError> {
        let allowed = self
            .allowed_source_keys
            .as_ref()
            .ok_or(CcrError::InvalidScope)?;
        self.store
            .revoke_unlisted_source_calls(&self.scope, allowed)
    }

    /// A bounded preview. JSON and JSON Lines are compacted without removing
    /// fields; long logs keep the most serious diagnostic lines. The original
    /// remains available by ID.
    /// Any failed store write must make the caller return the raw result.
    pub fn preview(&self, original: &str, id: &str) -> Option<String> {
        self.preview_with_query(original, id, None)
    }

    /// Compact validated JSON or JSON Lines by removing insignificant
    /// whitespace only. The returned text preserves duplicate keys, numeric
    /// spellings, key order, and string bytes. `None` means the input is not a
    /// supported structured form; a dense form may return unchanged text.
    pub fn lossless_compact_structured(original: &str) -> Option<String> {
        if matches!(original.trim_start().as_bytes().first(), Some(b'{' | b'['))
            && serde_json::from_str::<serde_json::Value>(original).is_ok()
        {
            return Some(strip_json_whitespace(original));
        }
        compact_json_lines(original)
    }

    /// For search-like tools, retain a few matching lines in the lossy
    /// preview. The query is used only in memory and never stored in CCR.
    pub fn preview_with_query(
        &self,
        original: &str,
        id: &str,
        query: Option<&str>,
    ) -> Option<String> {
        if original.len() < self.min_compress_bytes || original.len() > MAX_ORIGINAL_BYTES {
            return None;
        }
        // W2-E (review finding 4 / P3): this used to be a second, hand-copied
        // list of the four never-trim headers, so any tool result containing a
        // bare `## Constraints` line silently opted out of CCR compression.
        // The spelling now comes from `duduclaw_core` (one source of truth
        // with the gateway pipeline) and the exemption requires the embedding
        // process's marker on the very next line. No sentinel configured ⇒ no
        // exemption.
        if let Some(sentinel) = self.protected_sentinel.as_deref() {
            let lines: Vec<&str> = original.lines().collect();
            let protected = lines.iter().enumerate().any(|(idx, line)| {
                duduclaw_core::protected_section::opens_protected_section(
                    line,
                    lines.get(idx + 1).copied(),
                    sentinel,
                )
            });
            if protected {
                return None;
            }
        }
        let marker = format!(
            "[CCR: {} bytes; retrieve with {CCR_RETRIEVE_TOOL} id={id}]",
            original.len()
        );
        if let Some(compact) = Self::lossless_compact_structured(original) {
            let preview = format!("{compact}\n{marker}");
            if preview.len() * 2 < original.len() {
                return Some(preview);
            }
            // A valid, already-dense JSON/JSONL result keeps every field.
            // Falling through to a lossy preview would discard exact values.
            return None;
        }
        // serde_json::Value rejects some valid JSON numeric lexemes (for
        // example 1e400 without arbitrary_precision). An object/array-shaped
        // result that it cannot validate must retain its original bytes rather
        // than silently fall through to a lossy preview.
        //
        // The opening bracket alone is far too wide a net: a log tail that
        // starts `[2026-09-28T10:00:00Z] …`, or prose opening with a Markdown
        // link, would be permanently excluded from CCR. A JSON document is
        // bracket-delimited at BOTH ends, so require the matching closer too —
        // every real JSON document satisfies that, bracket-opening prose does
        // not.
        if is_bracket_delimited(original) {
            return None;
        }
        let head = duduclaw_core::truncate_bytes(original, 1_200);
        let tail_start = original.len().saturating_sub(400);
        let mut tail_start = tail_start;
        while !original.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        let tail = &original[tail_start..];
        let mut diagnostics: Vec<(usize, u8, String)> = Vec::new();
        for (index, line) in original.lines().enumerate() {
            let rank = diagnostic_rank(line);
            if rank == 0 {
                continue;
            }
            let snippet = duduclaw_core::truncate_bytes(line, 300);
            if diagnostics.iter().any(|(_, _, item)| item == snippet) {
                continue;
            }
            if diagnostics.len() < 8 {
                diagnostics.push((index, rank, snippet.to_owned()));
            } else if let Some((lowest_index, _)) = diagnostics
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, rank, _))| *rank)
            {
                if rank > diagnostics[lowest_index].1 {
                    diagnostics[lowest_index] = (index, rank, snippet.to_owned());
                }
            }
        }
        diagnostics.sort_by_key(|(index, _, _)| *index);
        let mut highlights: Vec<String> = diagnostics
            .into_iter()
            .map(|(_, _, snippet)| snippet)
            .collect();
        if let Some(query) = query
            .map(str::trim)
            .filter(|query| (3..=128).contains(&query.len()))
        {
            let terms = find_terms(query);
            let query_lower = query.to_lowercase();
            let mut query_lines = Vec::new();
            for (line_index, line) in original.lines().enumerate() {
                let exact = line.to_lowercase().contains(&query_lower);
                let rank = if exact {
                    Some((true, terms.len(), 0))
                } else {
                    lexical_find_rank(line, query, &terms)
                        .map(|(_, matched_terms, gap, _)| (false, matched_terms, gap))
                };
                if let Some((exact, matched_terms, gap)) = rank {
                    let snippet = duduclaw_core::truncate_bytes(line, 300).to_owned();
                    if highlights.iter().any(|item| item == &snippet)
                        || query_lines
                            .iter()
                            .any(|(_, _, _, _, item)| item == &snippet)
                    {
                        continue;
                    }
                    query_lines.push((exact, matched_terms, gap, line_index, snippet));
                    query_lines.sort_by(|a, b| {
                        b.0.cmp(&a.0)
                            .then_with(|| b.1.cmp(&a.1))
                            .then_with(|| a.2.cmp(&b.2))
                            .then_with(|| a.3.cmp(&b.3))
                    });
                    query_lines.truncate(24);
                }
            }
            highlights.extend(
                query_lines
                    .into_iter()
                    .take(12 - highlights.len())
                    .map(|(_, _, _, _, snippet)| snippet),
            );
        }
        let sections = markdown_section_highlights(original, query);
        let section_block = if sections.is_empty() {
            String::new()
        } else {
            format!("\n[CCR sections]\n{}", sections.join("\n"))
        };
        let outliers = tabular_outlier_rows(original);
        let outlier_block = if outliers.is_empty() {
            String::new()
        } else {
            format!("\n[CCR numeric outlier rows]\n{}", outliers.join("\n"))
        };
        let preview = if highlights.is_empty() {
            format!("{head}{section_block}{outlier_block}\n{marker}\n{tail}")
        } else {
            format!(
                "{head}{section_block}{outlier_block}\n[CCR diagnostic lines]\n{}\n{marker}\n{tail}",
                highlights.join("\n")
            )
        };
        (preview.len() * 2 < original.len()).then_some(preview)
    }
}
