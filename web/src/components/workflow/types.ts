/** Wire projections; none of these values grant permissions in the browser. */
export type IntegrityStatus = 'current' | 'stale' | 'unverified';
export interface ReviewArtifact {
  artifact_id: string; name: string; agent_id: string;
  archived_name: string | null; source_path: string | null;
  source_hash: string | null; archived_hash: string | null;
  integrity: IntegrityStatus; reasons: string[];
  evidence_kind: 'self_report' | 'test' | 'operator'; run_id: string | null;
  audience: string[];
}
export interface ReviewSnapshot {
  schema_version: number; snapshot_id: string; snapshot_hash: string;
  task_id: string; authority_revision: number; authority_snapshot_hash: string;
  criteria_ledger: unknown | null; artifacts: ReviewArtifact[];
  captured_at: string; audience: string[]; gaps: string[];
  waiting_for: string | null; next_check_at: string | null;
}
export type FixtureKind = 'normal' | 'empty' | 'expired' | 'injection' | 'missing_permission';
export type RunStatus = 'pending' | 'running' | 'waiting_approval' | 'needs_input' | 'succeeded' | 'failed' | 'uncertain' | 'cancelled' | 'blocked';
export interface WorkflowDraft {
  schema_version: number; draft_id: string; revision: number; owner: string;
  source_task: string; skill_id: string; source_snapshot_id: string; source_evidence_hash: string;
  revision_hash: string; definition: { skill_revision_hash: string; required_capabilities: string[]; input_schema: unknown; output_schema: unknown };
  source_data: Array<{ classification: 'DATA'; value: unknown }>;
  fixtures: Array<{ fixture_id: string; kind: FixtureKind; input_hash: string; assertion_hash: string }>;
  audience: string[]; budget: { per_run_micros: number; monthly_micros: number; max_consecutive_failures: number };
  input_max_age_seconds: number; timezone: string; routine?: { expression: string; timezone: string } | null; stop_conditions?: string[];
  created_at: string; disabled: boolean; review_status: string; draft_hash: string;
}
export interface FixtureResult {
  evidence: { fixture_id: string; run_id: string; status: RunStatus; result_hash: string | null; expires_at: string; steps: Array<{ step_id: string; status: string; output_hash: string | null; operation_id: string | null; error_code: string | null }> };
  assertions: Array<{ outcome: 'matched' | 'mismatched' | 'not_evaluated'; run_id: string }>;
}
/** Gateway `workflow::cost_ledger::Pricing::view`. With every unit price at
 *  0 the money budgets cannot bind and only the count limits apply. */
export interface WorkflowPricing {
  money_limits_effective: boolean;
  limits: { max_runs_per_month: number; max_steps_per_run: number; max_reads_per_run: number; max_effects_per_run: number };
  limits_error?: string | null;
}
export interface DraftView {
  draft: WorkflowDraft;
  fixtures: FixtureResult[];
  activation: { activation_id: string; acceptance_id: string; state: string; material_hash: string; error_code: string | null; suspension?: { reason: string; changed_categories: string[]; suspended_at: string } | null; expires_at?: string } | null;
  pricing?: WorkflowPricing;
  /** Computed by the server after rechecking hashes, audience and grants. */
  activation_eligible: boolean;
  issues: string[];
}
export interface DraftService {
  list(taskId: string): Promise<DraftView[]>;
  create(taskId: string, snapshotId: string, snapshotHash: string, proposal: unknown): Promise<DraftView>;
  get(draftId: string, revision: number): Promise<DraftView>;
  runFixture(draftId: string, revision: number, fixtureId: string, draftHash: string): Promise<{ run_id: string }>;
  commitActivation?(draftId: string, revision: number, draftHash: string): Promise<unknown>;
  revokeActivation?(draftId: string, revision: number, draftHash: string): Promise<unknown>;
  requestActivation(draftId: string, revision: number, draftHash: string): Promise<{ approval_id: string }>;
}
