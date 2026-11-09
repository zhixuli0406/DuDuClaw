/**
 * v1.81 F5/F6e — restore, adoption and purge of tombstoned (quarantined)
 * protected employees, plus the staff side of Personal employees.
 *
 * Dashboard-only RPCs. `me.isolation.*` acts as the signed-in staff member;
 * `isolation.*` needs an Admin, and every write takes the dashboard step-up
 * token as `csrf_token` (see `lib/step-up.ts`). Errors are `{ code, message }`
 * (or a plain zh-TW string from older handlers) — see `lib/restore-errors.ts`.
 *
 * Wire shapes below follow the F5/F6e owner-approved design. Fields marked
 * optional are tolerated as absent so an older gateway degrades to "unknown"
 * rather than crashing a screen.
 */
import { client } from './ws-client';
import type { StepUpToken } from './step-up';
import type { RequestOptionsJSON } from './passkey';

/** Epoch seconds or an RFC 3339 string. */
export type WireTime = number | string;

export function toMillis(value: WireTime | null | undefined): number | null {
  if (value == null) return null;
  if (typeof value === 'number') return value < 1e12 ? value * 1000 : value;
  const ms = Date.parse(value);
  return Number.isNaN(ms) ? null : ms;
}

const DAY_MS = 24 * 60 * 60 * 1000;

/** Whole days until `deadline` (never negative); null when unknown. */
export function daysUntil(deadline: WireTime | null | undefined, now = Date.now()): number | null {
  const ms = toMillis(deadline);
  if (ms == null) return null;
  return Math.max(0, Math.ceil((ms - now) / DAY_MS));
}

// ── Shared vocabulary ────────────────────────────────────────────────────────

export type EmployeeKind = 'personal' | 'shared';
/** 可還原 / 執行環境已不存在 / 無法確認 / 待釋放 */
export type RootState = 'restorable' | 'root_gone' | 'unknown' | 'pending_release';
/** Quarantine row seal (`isolation_lifecycle::TombstoneOutcome::seal`). */
export type SealState = 'sealed' | 'oversize' | 'invalid' | 'absent' | 'pending';
export type ProofFactor = 'passkey' | 'totp' | 'email';
export type AccessLevel = 'owner' | 'operator' | 'viewer';
export const ACCESS_LEVELS: readonly AccessLevel[] = ['owner', 'operator', 'viewer'];

export function normalizeRootState(value: unknown): RootState {
  const v = typeof value === 'string' ? value.toLowerCase() : '';
  return v === 'restorable' || v === 'root_gone' || v === 'pending_release' ? v : 'unknown';
}

export interface OpenRequest {
  request_id?: string;
  kind: 'restore' | 'adoption';
  filed_by_admin?: boolean;
  owner_unverified?: boolean;
  created_at?: WireTime;
}

// ── Staff (me.isolation.*) ───────────────────────────────────────────────────

export interface RestorableEmployee {
  employee_id: string;
  incarnation: number;
  label: string;
  display_name?: string;
  kind?: EmployeeKind;
  tombstoned_at?: WireTime;
  /** End of the restore window. */
  restore_until?: WireTime;
  days_left?: number;
  root_state?: RootState | string;
  open_request?: OpenRequest | null;
  /** A credential the owner did not change was changed by someone else. */
  credential_changed?: boolean;
}

export interface RestoreChallenge {
  /** The strongest factor the owner had before the tombstone. `none`: no
   *  second factor existed; `changed`: it was changed after the tombstone. */
  factor: ProofFactor | 'none' | 'changed';
  challenge_id?: string;
  /** WebAuthn request options (base64url fields) for `passkey`. */
  publicKey?: RequestOptionsJSON;
  /** `email`: the masked address the code went to. */
  email_hint?: string;
  expires_at?: WireTime;
}

export interface RestoreProof {
  factor: ProofFactor;
  code?: string;
  challenge_id?: string;
  credential?: unknown;
}

export interface PersonalEmployeeRequest {
  request_id: string;
  label: string;
  display_name: string;
  created_at: string;
  status: 'pending' | 'applying' | 'applied' | 'failed' | 'rejected' | 'expired';
  stage: 'requested' | 'proposed';
}

export interface PersonalDelegation {
  id: string;
  summary?: string;
  /** Full bound task text — shown exactly, never shortened. */
  task: string;
  source_agent: string;
  target_agent: string;
  delivery?: unknown;
  created_at?: WireTime;
  expires_at?: WireTime;
}

/** At most this many open Personal employee requests per person
 *  (`isolation_employee_rpc::MAX_OPEN_REQUESTS`). */
export const MAX_OPEN_PERSONAL_REQUESTS = 3;

/** `duduclaw_core::is_valid_new_agent_id`. Reserved names are checked by the server. */
export function isValidEmployeeLabel(label: string): boolean {
  return label.length > 0 && label.length <= 64 && /^[a-z0-9-]+$/.test(label) && !label.startsWith('-') && !label.endsWith('-');
}

const withProof = (base: Record<string, unknown>, proof: RestoreProof) => ({
  ...base,
  factor: proof.factor,
  ...(proof.code ? { code: proof.code } : {}),
  ...(proof.challenge_id ? { challenge_id: proof.challenge_id } : {}),
  ...(proof.credential !== undefined ? { credential: proof.credential } : {}),
});

export const myIsolationApi = {
  listRestorable: () =>
    client.call('me.isolation.list_restorable', {}) as Promise<{ employees: RestorableEmployee[] }>,
  restoreChallenge: (employee_id: string, incarnation: number, forAdoption = false) =>
    client.call('me.isolation.restore_challenge', {
      employee_id, incarnation, ...(forAdoption ? { for_adoption: true } : {}),
    }) as Promise<RestoreChallenge>,
  requestRestore: (employee_id: string, incarnation: number, proof: RestoreProof) =>
    client.call('me.isolation.request_restore', withProof({ employee_id, incarnation }, proof)) as Promise<{
      request_id?: string; state?: string;
    }>,
  cancelRestore: (employee_id: string, incarnation: number) =>
    client.call('me.isolation.cancel_restore', { employee_id, incarnation }) as Promise<{ cancelled?: boolean }>,
  requestAdoption: (employee_id: string, incarnation: number, proof: RestoreProof, targetLabel?: string) =>
    client.call('me.isolation.request_adoption', withProof({
      employee_id, incarnation, ...(targetLabel ? { target_label: targetLabel } : {}),
    }, proof)) as Promise<{ request_id?: string; state?: string; target_label?: string }>,
  requestPersonalEmployee: (label: string, display_name: string) =>
    client.call('me.isolation.request_personal_employee', { label, display_name }) as Promise<PersonalEmployeeRequest & {
      approval_required: boolean;
    }>,
  listPersonalEmployeeRequests: () =>
    client.call('me.isolation.list_personal_employee_requests', {}) as Promise<{ requests: PersonalEmployeeRequest[] }>,
  listDelegations: () =>
    client.call('me.isolation.list_personal_delegations', {}) as Promise<{ delegations: PersonalDelegation[] }>,
  decideDelegation: (id: string, approve: boolean) =>
    client.call('me.isolation.decide_personal_delegation', { id, approve }) as Promise<{
      id: string; decided: 'approved' | 'denied';
    }>,
};

// ── Admin (isolation.*) ──────────────────────────────────────────────────────

export interface PurgeBlock {
  /** The 90-day window is over: an ordinary purge is allowed. */
  purgeable: boolean;
  purge_after?: WireTime | null;
  /** Early purge allowed (with a second Admin). */
  early?: boolean;
  /** An early purge is waiting for a second Admin. */
  early_purge?: { requested_by?: string; requested_at?: WireTime; by_me?: boolean; lost?: string[] } | null;
  purged_at?: WireTime | null;
}

export interface QuarantineRow {
  employee_id: string;
  incarnation: number;
  label: string;
  display_name?: string;
  kind: EmployeeKind;
  owner_user_id?: string;
  owner_name?: string;
  seal: SealState | string;
  root_state: RootState | string;
  tombstoned_at?: WireTime;
  purge: PurgeBlock;
  /** The owner had no second factor: an Admin may file for them. */
  admin_may_file_for_owner?: boolean;
  open_request?: OpenRequest | null;
  /** Retire (release the runtime) already proposed. */
  retire_proposed?: boolean;
}

export interface RestoreMember {
  user_id: string;
  name?: string;
  access_level: AccessLevel;
}

export interface RestoreRoute {
  account: string;
  generation: number;
  provider?: string;
  kind?: string;
  label?: string;
}

export interface RestoreRequestRow {
  request_id: string;
  employee_id: string;
  incarnation: number;
  label: string;
  display_name?: string;
  employee_kind: EmployeeKind;
  kind: 'restore' | 'adoption';
  target_label?: string;
  filed_by?: string;
  filed_by_name?: string;
  /** Filed by the signed-in Admin. */
  filed_by_me?: boolean;
  filed_by_admin?: boolean;
  /** The owner did not confirm with a second factor. */
  owner_unverified?: boolean;
  /** A decision on this request would be the sole Admin approving their own filing. */
  self_approved?: boolean;
  /** This installation has a single Admin. */
  sole_admin?: boolean;
  /** Self-approval waits until this time (7-day cooling). */
  cooling_until?: WireTime | null;
  created_at?: WireTime;
  status?: string;
  members?: RestoreMember[];
  routes?: RestoreRoute[];
  /** Adoption: what goes over to the new employee / stays sealed. */
  carried?: string[];
  sealed?: string[];
}

export type RegrantEntry = string | { item: string; kind?: string; label?: string; done?: boolean };

export interface RegrantGroup {
  employee_id: string;
  incarnation: number;
  label: string;
  items: RegrantEntry[];
}

export interface IsolationEmployeeList {
  step_up_valid?: boolean;
  csrf_token?: StepUpToken | null;
  mfa_enrolled?: boolean;
  restores?: {
    quarantine?: QuarantineRow[];
    requests?: RestoreRequestRow[];
    regrants?: RegrantGroup[];
  };
}

export interface RestoreApproveReply {
  state?: string;
  incarnation?: number;
  self_approved?: boolean;
  label_state?: { regrant?: RegrantEntry[]; skipped?: RegrantEntry[]; listed?: RegrantEntry[] };
  work_paused?: boolean | number;
  routes_need_provisioning?: boolean | number | RegrantEntry[];
  members?: RestoreMember[];
  members_dropped?: Array<string | { user_id: string; name?: string; reason?: string }>;
  routes_reapproved?: number | RestoreRoute[];
}

export interface SharedRestoreOptions {
  members: Array<{
    user_id: string;
    name?: string;
    access_level: AccessLevel;
    is_primary?: boolean;
    active: boolean;
    dropped_reason?: string | null;
  }>;
  routes: Array<RestoreRoute & { provider: string; kind: string }>;
}

export interface PurgeReply {
  state: 'purged' | 'requested';
  lost?: string[];
}

export function regrantId(entry: RegrantEntry): string {
  return typeof entry === 'string' ? entry : entry.item;
}

export const adminRestoreApi = {
  list: () => client.call('isolation.employee_list', {}) as Promise<IsolationEmployeeList>,
  decide: (request_id: string, decision: 'approve' | 'deny', csrf_token: StepUpToken) =>
    client.call('isolation.employee_restore_decide', { request_id, decision, csrf_token }) as Promise<RestoreApproveReply>,
  cancel: (request_id: string, csrf_token: StepUpToken) =>
    client.call('isolation.employee_restore_cancel', { request_id, csrf_token }) as Promise<{ cancelled?: boolean }>,
  regrant: (employee_id: string, incarnation: number, item: string, csrf_token: StepUpToken) =>
    client.call('isolation.employee_restore_regrant', { employee_id, incarnation, item, csrf_token }) as Promise<{
      granted?: boolean;
    }>,
  retirePropose: (employee_id: string, csrf_token: StepUpToken) =>
    client.call('isolation.employee_retire_propose', { employee_id, csrf_token }) as Promise<{ request_id?: string }>,
  sharedOptions: (employee_id: string, incarnation: number) =>
    client.call('isolation.shared_restore_options', { employee_id, incarnation }) as Promise<SharedRestoreOptions>,
  sharedRestoreRequest: (
    employee_id: string, incarnation: number,
    members: Array<{ user_id: string; access_level: AccessLevel }>,
    routes: Array<{ account: string; generation: number }>,
    csrf_token: StepUpToken,
  ) =>
    client.call('isolation.shared_restore_request', { employee_id, incarnation, members, routes, csrf_token }) as Promise<{
      request_id?: string;
    }>,
  sharedAdoptionRequest: (
    employee_id: string, incarnation: number,
    members: Array<{ user_id: string; access_level: AccessLevel }>,
    routes: Array<{ account: string; generation: number }>,
    csrf_token: StepUpToken,
    targetLabel?: string,
  ) =>
    client.call('isolation.shared_adoption_request', {
      employee_id, incarnation, members, routes, csrf_token, ...(targetLabel ? { target_label: targetLabel } : {}),
    }) as Promise<{ request_id?: string }>,
  restoreForOwner: (employee_id: string, incarnation: number, csrf_token: StepUpToken) =>
    client.call('isolation.employee_restore_request_for_owner', { employee_id, incarnation, csrf_token }) as Promise<{
      request_id?: string;
    }>,
  adoptionForOwner: (employee_id: string, incarnation: number, csrf_token: StepUpToken, targetLabel?: string) =>
    client.call('isolation.adoption_request_for_owner', {
      employee_id, incarnation, csrf_token, ...(targetLabel ? { target_label: targetLabel } : {}),
    }) as Promise<{ request_id?: string }>,
  purge: (employee_id: string, incarnation: number, csrf_token: StepUpToken, opts: { confirmEarly?: boolean; retire?: boolean } = {}) =>
    client.call('isolation.quarantine_purge', {
      employee_id, incarnation, csrf_token,
      ...(opts.confirmEarly ? { confirm_early: true } : {}),
      ...(opts.retire ? { retire: true } : {}),
    }) as Promise<PurgeReply>,
};
