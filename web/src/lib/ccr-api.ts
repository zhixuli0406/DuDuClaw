import { useAuthStore } from '@/stores/auth-store';

/** Content-free, exact-tenant CCR store health. This is not task-quality or cost evidence. */
export interface CcrDashboardSnapshot {
  status: 'absent' | 'ready';
  entries: {
    eligible_retained: number;
    expired_retained: number;
    revoked_retained: number;
    declared_original_bytes_eligible: number;
  };
  revocations: {
    source_calls: number;
    scopes: number;
    artifact_versions: number;
  };
  retrieval_audit: {
    grants: number;
    refusals: number;
    returned_bytes_granted: number;
    window_max_rows: number;
  };
  compression_metrics: {
    available: boolean;
    reason: string | null;
    window_max_rows: number;
    observed_loops: number;
    provider_rounds: number;
    usage_reported_rounds: number;
    input_tokens: number;
    output_tokens: number;
    cache_read_tokens: number;
    cache_write_tokens: number;
    reasoning_tokens: number;
    ccr_compressed_results: number;
    ccr_original_bytes: number;
    ccr_delivered_bytes: number;
    ccr_find_attempts: number;
    ccr_find_hits: number;
    ccr_find_misses: number;
    ccr_retrieve_attempts: number;
    ccr_retrieve_successes: number;
    ccr_retrieve_misses: number;
    ccr_retrieved_bytes: number;
    p95_elapsed_millis: number | null;
  };
  causal_revocation_outbox: {
    available: boolean;
    reason: 'not_configured' | 'source_db_absent' | 'legacy_schema' | 'source_db_unavailable' | null;
    pending_notices: number;
    oldest_pending_seconds: number | null;
  };
  causal_delivery: {
    available: boolean;
    reason: 'not_configured' | 'source_db_absent' | 'legacy_schema' | 'source_db_unavailable' | null;
    active_leases: number;
    oldest_lease_seconds: number | null;
    revoking_sources: number;
    oldest_revoking_seconds: number | null;
  };
  connector_lifecycle: {
    available: boolean;
    reason: 'not_configured' | 'source_db_absent' | 'legacy_schema' | 'source_db_unavailable' | null;
    pending_events: number;
    blocked_events: number;
    completed_events: number;
    oldest_pending_seconds: number | null;
    latest_completed_seconds: number | null;
    last_failure_code: 'delivery_lease_active' | 'dependent_store_unavailable' | 'identity_mismatch' | null;
  };
}

/** Recorded evidence is user supplied. These values are replayed claims, not live attestation. */
export interface CcrReplayArmReport {
  arm: 'raw' | 'lossless' | 'lossy' | 'ccr';
  tasks: number;
  successes: number;
  success_rate: number;
  total_cost_millicents: number | null;
  cost_per_success_millicents: number | null;
  p95_latency_millis: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_input_ratio: number | null;
  usage_complete_tasks: number;
  retrieval_attempts: number;
  retrieval_misses: number;
  retrieval_miss_rate: number | null;
}

export interface CcrReplayResult {
  evidence_origin: 'synthetic' | 'recorded_claim';
  quality_method: 'recorded_exact_value_check';
  billing_status: 'unavailable_without_verified_receipts';
  /** Present in the API response; the dashboard must never render task-level rows. */
  observations: unknown[];
  /** Null for older recordings without a delivery digest for every arm. */
  source_delivery?: { arm: CcrReplayArmReport['arm']; changed_tasks: number }[] | null;
  report: {
    version: string;
    cost_source: 'unbilled_replay';
    paired_tasks: number;
    arms: CcrReplayArmReport[];
    ccr_success_delta_vs_raw: number;
    ccr_cost_per_success_delta_vs_raw_millicents: number | null;
    limitations: string[];
  };
  limitations: string[];
}

export const ccrApi = {
  async replay(tenantId: string, evidenceJson: string): Promise<CcrReplayResult> {
    const jwt = useAuthStore.getState().jwt;
    const query = new URLSearchParams({ tenant_id: tenantId });
    const response = await fetch(`/api/ccr/replay?${query}`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(jwt ? { Authorization: `Bearer ${jwt}` } : {}),
      },
      body: evidenceJson,
    });
    if (!response.ok) {
      // Uploaded evidence may contain private text. Never surface server error bodies in this UI.
      throw new Error(`HTTP ${response.status}`);
    }
    return response.json() as Promise<CcrReplayResult>;
  },
  async dashboard(tenantId: string): Promise<CcrDashboardSnapshot> {
    const jwt = useAuthStore.getState().jwt;
    const query = new URLSearchParams({ tenant_id: tenantId });
    const response = await fetch(`/api/ccr/dashboard?${query}`, {
      method: 'GET',
      headers: jwt ? { Authorization: `Bearer ${jwt}` } : {},
    });
    if (!response.ok) {
      const payload: unknown = await response.json().catch(() => null);
      const detail = payload && typeof payload === 'object' && 'error' in payload
        && typeof payload.error === 'string' ? payload.error : `HTTP ${response.status}`;
      throw new Error(detail);
    }
    return response.json() as Promise<CcrDashboardSnapshot>;
  },
};
