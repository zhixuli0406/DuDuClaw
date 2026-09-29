import { useAuthStore } from '@/stores/auth-store';

export interface CausalScope { tenant_id: string; acl: string }
export interface CausalSourceRemovalResult {
  demoted_claims: number; scrubbed_snapshots: number; scrubbed_ccr_originals: number;
}
export type ClaimModality = 'asserted' | 'speculated' | 'negated' | 'questioned';
export interface CausalClaim {
  id: string; cause_variable: string; effect_variable: string;
  lag_min_seconds: number; lag_max_seconds: number; modality: ClaimModality;
  context_json: string; review_state: string; reviewer: string | null; created_at: number;
}
export interface CausalEvidence {
  span: { id: string; artifact_id: string; span_start: number; span_end: number;
    excerpt: string; stance: 'supports' | 'opposes'; speaker_id: string | null; extractor_version: string };
  source_lineage_id: string; source_active: boolean;
}
export interface ClaimDetail {
  claim: CausalClaim; effective_state: string; evidence: CausalEvidence[];
  lineages: { independent_supporting_lineages: number; independent_opposing_lineages: number };
  revisions: Array<{ old_claim_id: string; new_claim_id: string; reviewer: string; note: string; revised_at: number }>;
}
export interface CausalAlias { alias: string; canonical_name: string; review_id: string; reviewer: string }
export interface ModelSummary { model: { id: string; name: string; version: string; review_state: string }; effective_state: string }
export interface ModelReview {
  model: ModelSummary['model'] & { treatment_variable_id: string; outcome_variable_id: string };
  effective_state: string;
  variables: Array<{ id: string; name: string; version: string; kind: 'binary' | 'count' | 'continuous' | 'categorical' }>;
  edges: CausalClaim[]; active_opposition_count: number; active_opposition_digest: string;
}
export type ParentAdjustment =
  | { status: 'unknown'; reasons: string[] }
  | { status: 'graphically_admissible'; adjustment_variable_ids: string[] };
export interface NegativeControlReview {
  id: string; model_id: string; variable_id: string; protocol_artifact_id: string;
  verdict: 'pass' | 'fail' | 'unknown'; rationale: string; reviewer: string; reviewed_at: number;
}
export interface NegativeControlView {
  review: NegativeControlReview | null;
  readiness: { state: 'reviewed' | 'unknown'; reasons?: string[] };
}
export type AssumptionKind = 'data_availability' | 'temporal_order' | 'positivity'
  | 'exchangeability' | 'consistency' | 'identifiability';
export interface AssumptionReview {
  kind: AssumptionKind; verdict: 'pass' | 'fail' | 'unknown'; rationale: string;
  reviewer: string; reviewed_at: number; review_id: string;
}
export interface AssumptionView {
  reviews: AssumptionReview[];
  readiness: { state: 'ready_for_estimator' | 'unknown'; reasons?: string[] };
}
export interface ObservedDataset {
  adjustment_variable_ids: string[];
  evaluation_cutoff?: number | null;
  negative_control?: { variable_id: string; review_id: string } | null;
  units: Array<{
    unit_id: string; adjustment_measured_at?: number | null;
    adjustment_values?: Record<string, string>; treatment_assigned_at: number;
    outcome_recorded_at: number; treated: boolean; outcome: number;
    pre_treatment_outcome?: number | null; pre_treatment_outcome_recorded_at?: number | null;
    negative_control_outcome?: number | null; negative_control_recorded_at?: number | null;
    stratum: string;
  }>;
}
export type EffectResult =
  | { state: 'unknown'; reasons: string[] }
  | { state: 'estimated'; estimate: {
      id: string; estimate: number; lower_bound: number; upper_bound: number;
      identification_state: string;
      negative_control: { contrast: number; lower_bound: number; upper_bound: number; imbalance_flag: boolean } | null;
    } };

export interface ExtractionEvalDataset {
  version: string;
  cutoff_unix: number;
  cases: Array<{
    id: string; occurred_at: number; source_lineage_id: string; source_family_id: string;
    question: string; source_text: string; gold: unknown[];
    reviewed_edges?: unknown[] | null; response_json: string;
  }>;
}
export interface ExtractionEvalCount {
  true_positive: number; predicted: number; gold: number;
  precision: number | null; recall: number | null; f1: number | null;
}
export interface ExtractionEvalReport {
  dataset_sha256: string; training_cases: number; held_out_cases: number; held_out_families: number;
  invalid_responses: number; ungrounded_predictions: number; reviewed_cases: number;
  reviewed_families: number; rejected_edges_proposed: number;
  full_claim: ExtractionEvalCount; exact_span: ExtractionEvalCount;
  direction_claim: ExtractionEvalCount; stance_claim: ExtractionEvalCount;
  modality_claim: ExtractionEvalCount; lag_claim: ExtractionEvalCount;
  negation_claim: ExtractionEvalCount; independent_lineage_claim: ExtractionEvalCount;
  reviewed_edge_case: ExtractionEvalCount; limitations: string[];
}
export interface ExtractionEvalResult {
  scope: CausalScope;
  mode: 'bundled_synthetic' | 'scoped_dataset';
  source_provenance: 'bundled_synthetic_unbound' | 'active_scoped_artifacts_at_evaluation';
  family_provenance: 'bundled_synthetic_annotations' | 'caller_supplied_unverified';
  report: ExtractionEvalReport;
}

/** A source artifact as the import endpoints return it.
 *
 * `retention_at` is `null` when the source has no retention deadline — a Wiki
 * page or an open-ended memory lives as long as its live source does. The
 * backend sentinel for that is `i64::MAX`, which does not survive a round trip
 * through a JavaScript number, so it is serialised as `null` and must never be
 * compared as if it were a timestamp. */
export interface ImportedSource {
  id: string;
  version: string;
  retention_at?: number | null;
}

async function request<T>(path: string, body?: object): Promise<T> {
  const jwt = useAuthStore.getState().jwt;
  const response = await fetch(path, {
    method: body ? 'POST' : 'GET',
    headers: { ...(jwt ? { Authorization: `Bearer ${jwt}` } : {}), ...(body ? { 'Content-Type': 'application/json' } : {}) },
    ...(body ? { body: JSON.stringify(body) } : {}),
  });
  if (!response.ok) {
    const payload: unknown = await response.json().catch(() => null);
    const detail = payload && typeof payload === 'object' && 'error' in payload && typeof payload.error === 'string'
      ? payload.error : `HTTP ${response.status}`;
    throw new Error(response.status === 409 ? `資料已變更，請重新載入：${detail}` : detail);
  }
  return response.json() as Promise<T>;
}

function query(scope: CausalScope, extra: Record<string, string> = {}): string {
  return new URLSearchParams({ tenant_id: scope.tenant_id, acl: scope.acl, ...extra }).toString();
}

export const causalApi = {
  importMemory: (agent_id: string, memory_id: string) => request<{ source: ImportedSource }>(
    '/api/causal/source/import-memory', { agent_id, memory_id }),
  importWiki: (agent_id: string, page_path: string) => request<{ source: ImportedSource }>(
    '/api/causal/source/import-wiki', { agent_id, page_path }),
  importSharedWiki: (page_path: string) => request<{ source: ImportedSource }>(
    '/api/causal/source/import-shared-wiki', { page_path }),
  clearRevocationFence: (scope: CausalScope, artifact_id: string) => request<{ cleared_version: string }>(
    '/api/causal/source/clear-revocation-fence', { ...scope, artifact_id }),
  claims: (scope: CausalScope) => request<{ claims: CausalClaim[] }>(`/api/causal/claims?${query(scope, { limit: '100' })}`),
  claim: (scope: CausalScope, id: string) => request<ClaimDetail>(`/api/causal/claim?${query(scope, { id })}`),
  source: (scope: CausalScope, id: string, offset: number) => request<{ text: string; byte_offset: number; next_offset: number; total_bytes: number; truncated: boolean }>(`/api/causal/source?${query(scope, { id, offset: String(offset), limit: '4096' })}`),
  invalidateSource: (scope: CausalScope, artifact_id: string) => request<{ result: CausalSourceRemovalResult }>(
    '/api/causal/source/invalidate', { ...scope, artifact_id }),
  eraseSource: (scope: CausalScope, artifact_id: string) => request<{ result: CausalSourceRemovalResult }>(
    '/api/causal/source/erase', { ...scope, artifact_id }),
  extract: (scope: CausalScope, artifact_id: string, question: string) => request<{ run: { candidates: Array<[CausalClaim, CausalEvidence['span']]>; provider_id: string; model_used: string } }>(
    '/api/causal/extract', { ...scope, artifact_id, question }),
  evaluateExtractionSynthetic: (scope: CausalScope) => request<ExtractionEvalResult>(
    '/api/causal/eval/extraction', { ...scope, mode: 'bundled_synthetic' }),
  evaluateExtractionScoped: (scope: CausalScope, dataset: ExtractionEvalDataset, case_artifacts: Record<string, string>) => request<ExtractionEvalResult>(
    '/api/causal/eval/extraction', { ...scope, mode: 'scoped_dataset', dataset, case_artifacts }),
  reviewClaim: (scope: CausalScope, id: string, expected_state: string, accept: boolean) => request<{ claim: CausalClaim }>('/api/causal/claim/review', { ...scope, id, expected_state, accept }),
  reviseClaim: (scope: CausalScope, id: string, revision: {
    expected_state: string; cause_variable: string; effect_variable: string; lag_min_seconds: number;
    lag_max_seconds: number; modality: ClaimModality; context: Record<string, unknown>;
    evidence_ids: string[]; note: string;
  }) => request<{ claim: CausalClaim }>('/api/causal/claim/revise', { ...scope, id, ...revision }),
  aliases: (scope: CausalScope) => request<{ aliases: CausalAlias[] }>(`/api/causal/aliases?${query(scope)}`),
  setAlias: (scope: CausalScope, alias: string, canonical_name: string, expected_review_id: string | null) => request<unknown>('/api/causal/alias', { ...scope, alias, canonical_name, expected_review_id }),
  revokeAlias: (scope: CausalScope, alias: string, expected_review_id: string) => request<unknown>('/api/causal/alias/revoke', { ...scope, alias, expected_review_id }),
  models: (scope: CausalScope) => request<{ models: ModelSummary[] }>(`/api/causal/models?${query(scope)}`),
  model: (scope: CausalScope, id: string) => request<{ review: ModelReview; parent_adjustment: ParentAdjustment }>(`/api/causal/model?${query(scope, { id })}`),
  reviewModel: (scope: CausalScope, review: ModelReview, approve: boolean, acknowledge_conflicts: boolean) => request<unknown>('/api/causal/model/review', {
    ...scope, id: review.model.id, expected_state: review.model.review_state,
    expected_opposition_digest: review.active_opposition_digest, approve, acknowledge_conflicts,
  }),
  assumptions: (scope: CausalScope, model_id: string) => request<AssumptionView>(
    `/api/causal/model/assumptions?${query(scope, { model_id })}`),
  reviewAssumption: (scope: CausalScope, review: {
    model_id: string; kind: AssumptionKind; verdict: 'pass' | 'fail' | 'unknown';
    rationale: string; expected_review_id: string | null;
  }) => request<{ review_id: string; readiness: AssumptionView['readiness'] }>(
    '/api/causal/model/assumptions', { ...scope, ...review }),
  addNegativeControlProtocol: (scope: CausalScope, source: {
    external_id: string; version: string; lineage_id: string; content: string;
    occurred_at: number; retention_at: number;
  }) => request<{ artifact: { id: string; version: string } }>(
    '/api/causal/negative-control/protocol', { ...scope, ...source }),
  negativeControlReview: (scope: CausalScope, model_id: string, variable_id: string) =>
    request<NegativeControlView>(`/api/causal/negative-control/review?${query(scope, { model_id, variable_id })}`),
  reviewNegativeControl: (scope: CausalScope, review: {
    model_id: string; variable_id: string; protocol_artifact_id: string;
    verdict: 'pass' | 'fail' | 'unknown'; rationale: string; expected_review_id: string | null;
  }) => request<{ review_id: string }>('/api/causal/negative-control/review', { ...scope, ...review }),
  estimateEffect: (scope: CausalScope, input: {
    model_id: string; external_id: string; version: string; lineage_id: string;
    occurred_at: number; retention_at: number; dataset: ObservedDataset;
  }) => request<{ dataset_artifact_id: string; result: EffectResult }>(
    '/api/causal/effect/estimate', { ...scope, ...input }),
  effect: (scope: CausalScope, id: string) => request<EffectResult>(`/api/causal/effect?${query(scope, { id })}`),
};
