/**
 * v1.81 F5/F6e — restore, adoption and purge of tombstoned (quarantined)
 * protected employees, plus the staff side of Personal employees.
 *
 * Dashboard-only RPCs. `me.isolation.*` acts as the signed-in staff member;
 * `isolation.*` needs an Admin, and every write takes the dashboard step-up
 * token as `csrf_token` (see `lib/step-up.ts`). Errors are `{ code, message }`
 * (or a plain zh-TW string from older handlers) — see `lib/restore-errors.ts`.
 *
 * CONTRACT SOURCE. The restore handlers (`isolation_restore_rpc.rs`,
 * `handlers/isolation_employee_restore.rs`) are NOT in this tree; the shapes
 * below follow the backend implementation reports for v1.81 F6:
 *   - `me.isolation.list_restorable` → `{ restorable: RestorableEmployee[] }`
 *   - `me.isolation.request_restore` → `RestoreFiled`
 *   - `isolation.employee_restore_decide` → `RestoreApproveReply`
 *   - `isolation.employee_list` → `.restores` (`IsolationEmployeeList`)
 *   - `isolation.quarantine_purge` → `PurgeReply` / refusal
 *     `EARLY_PURGE_CONFIRM_REQUIRED`
 * The runtime state comes from `root_flag` (`rootStateOf`), never from
 * invented `root_state` strings. Every field the reports do not name is
 * marked `NOT IN CONTRACT` below: it is read only when present and the
 * screens work without it. `RestoreChallenge` follows the handler
 * (`handlers/isolation_employee_restore.rs`).
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
/** Quarantine row seal (`isolation_lifecycle::TombstoneOutcome::seal`). */
export type SealState = 'sealed' | 'oversize' | 'invalid' | 'absent' | 'pending';
export type ProofFactor = 'passkey' | 'totp' | 'email';
export type AccessLevel = 'owner' | 'operator' | 'viewer';
export const ACCESS_LEVELS: readonly AccessLevel[] = ['owner', 'operator', 'viewer'];

/** Backend runtime flag: `null` means the employee's runtime is intact. */
export type RootFlag = 'ROOT_GONE' | 'ROOT_RETIRED' | 'ROOT_INCONSISTENT';

/**
 * What the screens say about the runtime (執行環境), derived from real fields:
 * - `restorable` 可還原 — no `root_flag`, data kept, still `restorable`
 * - `root_gone` 執行環境已不存在 — `ROOT_GONE` (only a take-over is possible)
 * - `unknown` 執行環境無法確認 — `ROOT_INCONSISTENT`, or a flag we do not know
 * - `pending_release` 待釋放 — data purged, runtime not released yet
 * - `retired` 已釋放 — `ROOT_RETIRED`
 * - `closed` 已無法還原 — no flag, but the server says `restorable: false`
 */
export type RootState = 'restorable' | 'root_gone' | 'unknown' | 'pending_release' | 'retired' | 'closed';

export function rootStateOf(row: {
  root_flag?: RootFlag | string | null;
  restorable?: boolean;
  purge?: { purged_at?: WireTime | null } | null;
}): RootState {
  const flag = row.root_flag ?? null;
  if (flag === 'ROOT_RETIRED') return 'retired';
  if (flag === 'ROOT_GONE') return 'root_gone';
  if (flag !== null) return 'unknown';
  if (row.purge?.purged_at != null) return 'pending_release';
  return row.restorable === false ? 'closed' : 'restorable';
}

export interface OpenRequest {
  kind: 'restore' | 'adoption';
  filed_by_admin?: boolean;
  owner_unverified?: boolean;
  /** NOT IN CONTRACT. */
  request_id?: string;
  /** NOT IN CONTRACT. */
  created_at?: WireTime;
}

// ── Staff (me.isolation.*) ───────────────────────────────────────────────────

/** One row of `me.isolation.list_restorable` → `{ restorable: [...] }`. */
export interface RestorableEmployee {
  employee_id: string;
  incarnation: number;
  label: string;
  kind?: EmployeeKind;
  owner_user_id?: string;
  seal?: SealState | string;
  tombstoned_at?: WireTime;
  /** End of the restore window. */
  restore_deadline?: WireTime | null;
  /** The server would accept a restore (window open, data sealed, no block). */
  restorable?: boolean;
  /** Opaque backend state; the screens read `root_flag`. */
  root_state?: string;
  root_flag?: RootFlag | string | null;
  open_request?: OpenRequest | null;
  admin_may_file_for_owner?: boolean;
  /** NOT IN CONTRACT: shown instead of `label` when a gateway sends it. */
  display_name?: string;
}

/** `me.isolation.request_restore` / `request_adoption` reply. */
export interface RestoreFiled {
  request_id: string;
  state: string;
  label?: string;
  /** The request lapses if no Admin decides by then. */
  expires_at?: WireTime | null;
  approval_required?: boolean;
  /** Admins told about the request (count, or `true`). */
  notified?: number | boolean;
}

/**
 * `me.isolation.restore_challenge` reply (`handlers/isolation_employee_restore.rs`):
 * the challenge for the owner's strongest factor at tombstone time.
 *   - `totp`: `{ factor, challenge_id: null }`
 *   - `passkey`: the WebAuthn options plus `factor` and `expires_in`
 *   - `email`: `{ factor, challenge_id, sent: true, expires_in }`
 * The gateway never answers a "no factor" / "factor changed" factor: those
 * are errors (`FACTOR_NOT_ENROLLED`, `FACTOR_NOT_PREEXISTING`,
 * `FACTOR_BASELINE_MISSING`, …) — see `needsAdminHelp` in `restore-errors.ts`.
 */
export interface RestoreChallenge {
  factor: ProofFactor;
  challenge_id?: string | null;
  /** WebAuthn request options (base64url fields) for `passkey`. */
  publicKey?: RequestOptionsJSON;
  /** `email`: the code was sent (the address is never returned). */
  sent?: boolean;
  /** Seconds the challenge stays valid. */
  expires_in?: number;
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
    client.call('me.isolation.list_restorable', {}) as Promise<{ restorable: RestorableEmployee[] }>,
  restoreChallenge: (employee_id: string, incarnation: number, forAdoption = false) =>
    client.call('me.isolation.restore_challenge', {
      employee_id, incarnation, ...(forAdoption ? { for_adoption: true } : {}),
    }) as Promise<RestoreChallenge>,
  requestRestore: (employee_id: string, incarnation: number, proof: RestoreProof) =>
    client.call('me.isolation.request_restore', withProof({ employee_id, incarnation }, proof)) as Promise<RestoreFiled>,
  cancelRestore: (employee_id: string, incarnation: number) =>
    client.call('me.isolation.cancel_restore', { employee_id, incarnation }) as Promise<{ cancelled?: boolean }>,
  requestAdoption: (employee_id: string, incarnation: number, proof: RestoreProof, targetLabel?: string) =>
    client.call('me.isolation.request_adoption', withProof({
      employee_id, incarnation, ...(targetLabel ? { target_label: targetLabel } : {}),
    }, proof)) as Promise<RestoreFiled & { target_label?: string }>,
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
  /** An early purge is waiting for a second Admin. `by_me` / `lost` are NOT IN CONTRACT. */
  early_purge?: { requested_by?: string; requested_at?: WireTime; by_me?: boolean; lost?: string[] } | null;
  purged_at?: WireTime | null;
}

/** One row of `isolation.employee_list` → `restores.quarantine`. */
export interface QuarantineRow {
  employee_id: string;
  incarnation: number;
  label: string;
  kind: EmployeeKind;
  owner_user_id?: string;
  seal: SealState | string;
  tombstoned_at?: WireTime;
  restore_deadline?: WireTime | null;
  restorable?: boolean;
  /** Opaque backend state; the screens read `root_flag` (`rootStateOf`). */
  root_state?: string;
  root_flag?: RootFlag | string | null;
  purge: PurgeBlock;
  /** The owner had no second factor: an Admin may file for them. */
  admin_may_file_for_owner?: boolean;
  open_request?: OpenRequest | null;
  /** NOT IN CONTRACT: shown instead of `label` when present. */
  display_name?: string;
  /** NOT IN CONTRACT: the screens look `owner_user_id` up in the member list. */
  owner_name?: string;
  /** NOT IN CONTRACT: retire (release the runtime) already proposed. */
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

/** One row of `isolation.employee_list` → `restores.requests`. */
export interface RestoreRequestRow {
  request_id: string;
  employee_id: string;
  incarnation: number;
  label: string;
  employee_kind: EmployeeKind;
  kind: 'restore' | 'adoption';
  owner_user_id?: string;
  /** users.db id of whoever filed it (owner, or the Admin for the owner). */
  filed_by?: string;
  filed_by_admin?: boolean;
  /** The owner did not confirm with a second factor. */
  owner_unverified?: boolean;
  /** Approving it would be the sole Admin approving their own filing. */
  self_approved?: boolean;
  /** Why an earlier approval could not finish (an error code), if it failed. */
  failure?: string | null;
  created_at?: WireTime;
  expires_at?: WireTime | null;
  state?: string;
  target_label?: string;
  /** NOT IN CONTRACT: shown instead of `label` when present. */
  display_name?: string;
  /** NOT IN CONTRACT: the screens look `filed_by` up in the member list. */
  filed_by_name?: string;
  /** NOT IN CONTRACT: the screens compare `filed_by` with the signed-in user. */
  filed_by_me?: boolean;
  /** NOT IN CONTRACT: `self_approved` already says the filer is the sole Admin. */
  sole_admin?: boolean;
  /** NOT IN CONTRACT: the server refuses with `SELF_APPROVAL_COOLING` instead. */
  cooling_until?: WireTime | null;
  /** NOT IN CONTRACT (Shared): snapshot members / routes chosen when filing. */
  members?: RestoreMember[];
  routes?: RestoreRoute[];
  /** NOT IN CONTRACT (adoption): what goes over / stays sealed. */
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

/** `isolation.employee_restore_decide` (approve) reply: `state: "done"`. */
export interface RestoreApproveReply {
  state?: string;
  incarnation?: number;
  self_approved?: boolean;
  label_state?: { regrant?: RegrantEntry[]; skipped?: RegrantEntry[]; listed?: RegrantEntry[] };
  work_paused?: boolean | number;
  routes_need_provisioning?: boolean | number | RegrantEntry[];
  // Shared employees only:
  members?: RestoreMember[];
  members_dropped?: Array<string | { user_id: string; name?: string; reason?: string }>;
  routes_reapproved?: number | RestoreRoute[];
  members_notified?: number | unknown[];
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

/** `isolation.quarantine_purge`: done, or an early purge waiting for a second
 *  Admin (with what will be `lost`). Without `confirm_early` an early purge is
 *  refused with `EARLY_PURGE_CONFIRM_REQUIRED`. */
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
