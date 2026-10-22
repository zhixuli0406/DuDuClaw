/**
 * Wire types for the `agent_files.*` RPCs (people editing an AI employee's
 * profile files: SOUL / IDENTITY / INSTRUCTIONS / CONTRACT, with agent.toml
 * shown read-only) and the shared save envelope that `contract.update` now
 * returns too. The gateway module is `crates/duduclaw-gateway/src/agent_files/`.
 */

/** Which file. `agent_toml` is read-only here (edit it on the edit page). */
export type AgentFileKey = 'soul' | 'identity' | 'instructions' | 'contract' | 'agent_toml';

/** How the file compares with the last approved version (SOUL / INSTRUCTIONS only). */
export type AgentFileDrift = 'intact' | 'drifted' | 'none';

export type AgentFileVerdict = 'pass' | 'warn' | 'block';

/** `rules+ai`: rules and the AI check both ran; `rules`: this file/source has
 *  no AI check; `rules+ai_unavailable`: the AI check did not complete. */
export type AgentFileReviewBasis = 'rules+ai' | 'rules' | 'rules+ai_unavailable';

export type AgentFileReasonCategory =
  | 'prompt_injection'
  | 'secret_request'
  | 'loosened_redline'
  | 'contract_conflict'
  | 'role_drift'
  | 'hidden_content'
  | 'secret_in_text'
  | 'size'
  | 'format'
  | 'analysis_unavailable'
  /** The file had been changed outside the dashboard; checked as a whole. */
  | 'outside_change';

export interface AgentFileReason {
  category: AgentFileReasonCategory;
  severity: 'low' | 'medium' | 'high';
  /** Server-side zh-TW explanation (UI shows the localized category text first). */
  message: string;
  /** Excerpt of the offending text, at most 200 characters (may be empty). */
  evidence: string;
}

export interface AgentFileReview {
  /** Pass back with `action: "confirm" | "request_approval"` (valid 15 minutes). */
  analysis_id: string | null;
  verdict: AgentFileVerdict;
  basis: AgentFileReviewBasis;
  /** Plain-language summary (zh-TW from the AI check, may be empty). */
  summary: string;
  reasons: AgentFileReason[];
  /** warn: anyone allowed to save; block: admins only (with acknowledge_risk). */
  can_confirm: boolean;
  /** Non-admins on warn / block. */
  can_request_approval: boolean;
}

export type AgentFileSaveStatus =
  | 'applied'
  | 'needs_confirmation'
  | 'blocked'
  | 'pending_approval'
  | 'unchanged'
  | 'conflict'
  | 'rejected';

export interface AgentFileContractCoverage {
  /** Common attacks the input filter already blocks. */
  covered: number;
  /** Attacks only a real-model test can confirm. */
  needs_live_validation: number;
  units: number;
}

/** Response of `agent_files.save`, `agent_files.rollback` and `contract.update`. */
export interface AgentFileSaveResponse {
  status: AgentFileSaveStatus;
  file: AgentFileKey;
  agent_id: string;
  /** New hash after `applied`/`unchanged` (use as the next `base_hash`). */
  hash?: string;
  version_id?: string | null;
  /** On `conflict`: what the file hashes to now. */
  current_hash?: string;
  review?: AgentFileReview;
  effective?: { registry_reloaded: boolean; applies_from: 'next_turn' };
  contract_coverage?: AgentFileContractCoverage | null;
  approval_id?: string;
  message?: string;
}

export interface AgentFileLastChange {
  at: string;
  user_id: string;
  source: string;
}

export interface AgentFileListEntry {
  file: AgentFileKey;
  /** i18n key for the plain-language name, e.g. `agentFiles.file.soul`. */
  label_key: string;
  exists: boolean;
  size: number;
  hash: string;
  editable_by_me: boolean;
  last_change: AgentFileLastChange | null;
  drift: AgentFileDrift;
  pending_approval_id: string | null;
}

export interface AgentFileListResponse {
  agent_id: string;
  files: AgentFileListEntry[];
  /** Byte limits per file key. */
  limits: Record<string, number>;
}

export interface AgentFileGetResponse {
  agent_id: string;
  file: AgentFileKey;
  content: string;
  hash: string;
  exists: boolean;
  /** agent.toml only: keys whose values were masked. */
  masked: string[];
  drift: { state: AgentFileDrift; accepted_version_id: string | null };
  version_id?: string | null;
}

export interface AgentFileDiffHunk {
  op: 'equal' | 'add' | 'remove';
  lines: string[];
}

export interface AgentFileDiffResponse {
  hunks: AgentFileDiffHunk[];
  added: number;
  removed: number;
}

export interface AgentFileVersion {
  version_id: string;
  file: AgentFileKey;
  at: string;
  user_id: string;
  /** dashboard | rollback | rename | agents.update | contract.update | config.raw | approval | initial | mcp | chat.model */
  source: string;
  sha256: string;
  size: number;
  verdict: AgentFileVerdict | null;
  reason_categories: AgentFileReasonCategory[];
  confirm_reason?: string | null;
  approval_id?: string | null;
  decided_by?: string | null;
}

export interface AgentFileHistoryResponse {
  agent_id: string;
  file: AgentFileKey;
  versions: AgentFileVersion[];
}

/** `agent_files.pending_get` (admins): what an `agent_file_change` approval asks for. */
export interface AgentFilePendingResponse {
  approval_id: string;
  agent_id: string;
  file: AgentFileKey;
  status: string;
  requested_by: string;
  reason: string;
  verdict: AgentFileVerdict;
  reason_categories: AgentFileReasonCategory[];
  /** True when the file changed after the request (approving it would fail). */
  stale: boolean;
  content_available: boolean;
  diff: AgentFileDiffResponse | null;
}

export type AgentFileSaveAction = 'check' | 'confirm' | 'request_approval';

export interface AgentFileSaveParams {
  agent_id: string;
  file: AgentFileKey;
  content: string;
  base_hash: string;
  action?: AgentFileSaveAction;
  analysis_id?: string;
  reason?: string;
  acknowledge_risk?: boolean;
}

/** Follow-up fields `contract.update` accepts after a `needs_confirmation` /
 *  `blocked` answer (send the same contract again). */
export interface ContractReviewFollowUp {
  action?: AgentFileSaveAction;
  analysis_id?: string;
  reason?: string;
  acknowledge_risk?: boolean;
  base_hash?: string;
}
