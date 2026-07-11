/**
 * Typed RPC wrappers for the self-built skill surface (V13 / §5.6).
 *
 * Kept in its own file (not folded into `lib/api.ts`) so the V13 wave owns a
 * clean, independently-reviewable boundary. Every shape here mirrors the
 * gateway `custom_skills.rs` + `handlers.rs` responses verbatim — adjust in
 * lockstep with the Rust side. The six `skills.custom_*` RPCs plus a
 * reason-carrying `approvals.decide` wrapper (the base `api.approvals.decide`
 * omits `reason` + `side_effect`, both of which the skill_create approval card
 * needs).
 */
import { client } from './ws-client';

/** Lifecycle status — mirrors `CustomSkillStatus` (snake_case). */
export type CustomSkillStatus =
  | 'draft'
  | 'generating'
  | 'pending_approval'
  | 'approved'
  | 'rejected'
  | 'retired';

/** The six canonical statuses in lifecycle order (chips / filters iterate this). */
export const CUSTOM_SKILL_STATUSES: readonly CustomSkillStatus[] = [
  'draft',
  'generating',
  'pending_approval',
  'approved',
  'rejected',
  'retired',
];

/**
 * Self-reported time-saving unit. The backend stores a free string (default
 * `minutes_per_use`); §5.6 enumerates the two the wizard offers. Widened with
 * `(string & {})` so an unexpected server value still types.
 */
export type TimeSavedUnit = 'minutes_per_use' | 'hours_per_month' | (string & {});

/**
 * One `custom_skill_registry` row, as returned by `custom_skill_to_json`.
 * `tags` is a comma-separated string on the wire (NOT an array).
 */
export interface CustomSkillRecord {
  id: string;
  /** Machine-stable skill name (loader identity once installed). */
  slug: string;
  /** Human-facing name, separate from `slug`. */
  display_name: string;
  description_human: string;
  time_saved_value: number;
  time_saved_unit: TimeSavedUnit;
  /** Comma-separated tags. */
  tags: string;
  created_by_user: string;
  built_by_agent: string;
  status: CustomSkillStatus;
  approval_id: string | null;
  rejection_reason: string | null;
  created_at: string;
  updated_at: string;
  approved_at: string | null;
  /**
   * Real invocation counter (L5 §14) — how many times the Claude CLI `Skill`
   * tool named this approved skill's slug. 0 for unapproved/never-run skills.
   */
  usage_count: number;
  /**
   * Cumulative saved HOURS derived from the estimate (L5 §14): per-use units use
   * `usage_count × value`, per-month units accrue over months since approval.
   * A real figure — the detail page shows it, still labeled as your estimate.
   */
  saved_hours_estimate: number;
}

/** One safety-scan finding inside `safety_report.findings`. */
export interface SafetyReportFinding {
  category: string;
  severity: string;
  description: string;
  line_number: number | null;
}

/** The mandatory pre-submit safety report attached to the approval payload. */
export interface SafetyReport {
  passed: boolean;
  /** Debug-formatted risk level (e.g. "Low" / "Medium" / "High" / "Critical"). */
  risk_level: string;
  findings: SafetyReportFinding[];
  sandbox_trial: {
    /** Always false pre-submit — sandbox trial is a post-install mechanism. */
    ran: boolean;
    skip_reason?: string;
  };
}

export interface CustomCreateParams {
  display_name: string;
  slug?: string;
  description_human?: string;
  time_saved_value?: number;
  time_saved_unit?: TimeSavedUnit;
  /** Comma-separated tags. */
  tags?: string;
  built_by_agent?: string;
}

export interface CustomGenerateParams {
  id: string;
  /** Override target agent; defaults to the record's `built_by_agent`. */
  agent?: string;
  /** Free-text revision note appended to the authoring prompt (re-generate). */
  instruction?: string;
}

export interface CustomGenerateResult {
  success: boolean;
  id: string;
  message_id: string;
  target_agent: string;
  draft_path: string;
  status: 'generating';
}

/** Human-field partial update. Any omitted field is left untouched. */
export interface CustomUpdateParams {
  id: string;
  display_name?: string;
  description_human?: string;
  time_saved_value?: number;
  time_saved_unit?: TimeSavedUnit;
  tags?: string;
}

export interface CustomSubmitResult {
  success: boolean;
  id: string;
  approval_id: string;
  status: 'pending_approval';
  safety_report: SafetyReport;
}

export interface CustomListResult {
  custom_skills: CustomSkillRecord[];
  count: number;
}

/** `side_effect` block on an `approvals.decide` of an `skill_create` request. */
export interface ApprovalSideEffect {
  /** Present on approve: the installed skill's name. */
  installed_skill?: string;
  /** Present on approve: whether the creator approved their own request. */
  self_approved?: boolean;
  /** Present on deny: the rejected custom-skill id. */
  custom_skill_rejected?: string;
}

export interface ApprovalDecideResult {
  id: string;
  decided: 'approved' | 'denied';
  /** `null` for non-skill_create kinds. */
  side_effect: ApprovalSideEffect | null;
}

/** `skills.custom_create` — record a new draft (human fields only). */
export function createCustomSkill(params: CustomCreateParams): Promise<CustomSkillRecord> {
  return client.call('skills.custom_create', { ...params }) as Promise<CustomSkillRecord>;
}

/**
 * `skills.custom_generate` — delegate SKILL.md authoring to an agent (bus
 * queue). Re-callable to regenerate with a revised `instruction`.
 */
export function generateCustomSkill(params: CustomGenerateParams): Promise<CustomGenerateResult> {
  return client.call('skills.custom_generate', { ...params }) as Promise<CustomGenerateResult>;
}

/** `skills.custom_update` — edit human-facing fields only. Returns the record. */
export function updateCustomSkill(params: CustomUpdateParams): Promise<CustomSkillRecord> {
  return client.call('skills.custom_update', { ...params }) as Promise<CustomSkillRecord>;
}

/**
 * `skills.custom_submit` — run the mandatory safety scan and, on pass, route to
 * an approver. REJECTS (throws) when risk is high/critical (fail-closed): the
 * gateway returns an error frame, which `client.call` surfaces as a rejection.
 */
export function submitCustomSkill(id: string): Promise<CustomSubmitResult> {
  return client.call('skills.custom_submit', { id }) as Promise<CustomSubmitResult>;
}

/** `skills.custom_list` — admins see all; others see only their own. */
export function listCustomSkills(): Promise<CustomListResult> {
  return client.call('skills.custom_list') as Promise<CustomListResult>;
}

/** `skills.custom_retire` — creator or admin retires a custom skill. */
export function retireCustomSkill(id: string): Promise<{ success: boolean; id: string; status: 'retired' }> {
  return client.call('skills.custom_retire', { id }) as Promise<{
    success: boolean;
    id: string;
    status: 'retired';
  }>;
}

/**
 * `approvals.decide` with a mandatory-on-deny `reason` and typed `side_effect`.
 * The base `api.approvals.decide` omits both; the skill_create approval card
 * needs the reason (rejection) and the side_effect (installed / rejected id).
 */
export function decideApproval(
  id: string,
  approve: boolean,
  reason?: string,
): Promise<ApprovalDecideResult> {
  return client.call('approvals.decide', {
    id,
    approve,
    ...(reason ? { reason } : {}),
  }) as Promise<ApprovalDecideResult>;
}
