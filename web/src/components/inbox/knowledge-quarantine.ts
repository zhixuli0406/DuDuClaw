/**
 * Plain-language reading of `knowledge_quarantine` approvals and their
 * decision results.
 *
 * Server contract (crates/duduclaw-gateway):
 *  - `wiki_ingest::quarantine_approval` (called by
 *    `dispatch_quarantine_side_effects`) builds the payload
 *    `{ memory_db, agent_id, origin, subject, quarantined_ids }`. For a claim
 *    held by the memory trust guard it adds `promote_on_approve: true`,
 *    `disposition: "trust_held"`, `predicate`, `snippet` (the new statement),
 *    `existing_id`, `existing_content` (the content currently held, ≤600
 *    chars, `existing_content_truncated: true` when cut), `existing_value`,
 *    `new_value`, `subject_label` (plain subject, incl. whose profile), and
 *    `snippet` is the full statement that will be written. Never shown:
 *    `predicate`, `subject` (unless nothing better exists), `existing_id`,
 *    `claim_digest`, `origin_channel`, `origin_chat_id`, `reason`. Rows written
 *    before those fields existed carry only `promote_on_approve`.
 *  - The summary (`trust_held_summary`) names the topic as 「關於「X」的新說法」
 *    and always ends with 「內容摘要：<statement>」; the marker is stripped
 *    from every untrusted part, so the last occurrence is the real one.
 *  - `handlers/approvals_decide.rs` returns `side_effect` with
 *    `quarantine_promoted` / `quarantine_released` / `quarantine_rejected`,
 *    and (server work in progress) `quarantine_stale` (nothing written,
 *    the situation changed) and `quarantine_held` (items from a burst batch
 *    turned into separate conflict cards; may come with `quarantine_released`).
 * Anything else falls back to the generic approval rendering.
 */

export const KNOWLEDGE_QUARANTINE_KIND = 'knowledge_quarantine';

/** Marker the server summary puts in front of the held content snippet. */
const SNIPPET_MARKER = '內容摘要：';

/** The plain topic label `trust_held_summary` writes (「關於「X」的新說法」). */
const SUBJECT_LABEL_RE = /關於「([^「」]+)」的新說法/;

function str(v: unknown): string | undefined {
  return typeof v === 'string' && v.trim() ? v.trim() : undefined;
}

/** Text that will be written: kept verbatim (no trimming, no cutting), only
 *  an all-blank string counts as absent. */
function verbatim(v: unknown): string | undefined {
  return typeof v === 'string' && v.trim() ? v : undefined;
}

/** A stored value may be a scalar of any JSON type; render it as text. */
function scalarText(v: unknown): string | undefined {
  if (typeof v === 'number' || typeof v === 'boolean') return String(v);
  return verbatim(v);
}

export type KnowledgeQuarantineVariant = 'conflict' | 'burst';

export interface KnowledgeQuarantineView {
  variant: KnowledgeQuarantineVariant;
  /** What the memory is about: payload `subject_label`, else the plain label
   *  cut from the summary (older rows), else the payload's `subject`. */
  subject?: string;
  /** The full new statement that will be written: payload `snippet`, else
   *  cut from the summary (older rows). */
  statement?: string;
  /** The value that will be written (payload `new_value`). */
  newValue?: string;
  /** The content the system currently holds (payload `existing_content`). */
  current?: string;
  /** True when the server cut `existing_content` (it caps it at 600 chars). */
  currentTruncated?: boolean;
  /** The value the system currently holds (payload `existing_value`). */
  currentValue?: string;
  /** Number of held items. */
  count: number;
}

function asRecord(v: unknown): Record<string, unknown> | null {
  if (typeof v === 'string') {
    try {
      return asRecord(JSON.parse(v));
    } catch {
      return null;
    }
  }
  return v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
}

/** Read a `knowledge_quarantine` approval; `null` for any other kind. */
export function parseKnowledgeQuarantine(
  kind: string,
  payload: unknown,
  summary: string,
): KnowledgeQuarantineView | null {
  if (kind !== KNOWLEDGE_QUARANTINE_KIND) return null;
  const p = asRecord(payload) ?? {};
  const conflict = p.promote_on_approve === true || p.disposition === 'trust_held';
  const ids = Array.isArray(p.quarantined_ids) ? p.quarantined_ids.length : 0;
  const at = summary.lastIndexOf(SNIPPET_MARKER);
  const cutStatement =
    at >= 0 ? summary.slice(at + SNIPPET_MARKER.length).trim() || undefined : undefined;
  const label = conflict ? SUBJECT_LABEL_RE.exec(summary)?.[1]?.trim() : undefined;
  if (!conflict) {
    return { variant: 'burst', subject: str(p.subject), statement: cutStatement, count: ids };
  }
  return {
    variant: 'conflict',
    subject: str(p.subject_label) ?? (label || str(p.subject)),
    statement: verbatim(p.snippet) ?? cutStatement,
    newValue: scalarText(p.new_value),
    current: verbatim(p.existing_content),
    currentTruncated: p.existing_content_truncated === true ? true : undefined,
    currentValue: scalarText(p.existing_value),
    count: ids,
  };
}

export interface ApprovalDecideResult {
  side_effect?: Record<string, unknown> | null;
}

export interface QuarantineResultMessage {
  id: string;
  values?: Record<string, number>;
}

function count(se: Record<string, unknown>, key: string): number | undefined {
  const v = se[key];
  return typeof v === 'number' && Number.isFinite(v) ? v : undefined;
}

/**
 * The message to show after deciding a knowledge-quarantine approval, or
 * `null` when the result carries no quarantine side effect (caller keeps its
 * generic text).
 */
export function quarantineResultMessage(
  result: ApprovalDecideResult | null | undefined,
  variant?: KnowledgeQuarantineVariant,
): QuarantineResultMessage | null {
  const se = result?.side_effect;
  if (!se || typeof se !== 'object') return null;
  if ('quarantine_stale' in se) return { id: 'approval.knowledge.result.stale' };
  if ('quarantine_promoted' in se) return { id: 'approval.knowledge.result.promoted' };
  const held = count(se, 'quarantine_held');
  const released = count(se, 'quarantine_released');
  if (held !== undefined && held > 0) {
    return released !== undefined && released > 0
      ? { id: 'approval.knowledge.result.releasedAndHeld', values: { released, held } }
      : { id: 'approval.knowledge.result.allHeld', values: { held } };
  }
  if ('quarantine_released' in se) return { id: 'approval.knowledge.result.released' };
  if ('quarantine_rejected' in se) {
    return {
      id:
        variant === 'conflict'
          ? 'approval.knowledge.result.conflictRejected'
          : 'approval.knowledge.result.rejected',
    };
  }
  return null;
}

type FormatMessage = (descriptor: { id: string }, values?: Record<string, string | number>) => string;

/**
 * List-row title for an approval. A held conflict gets a plain sentence (the
 * server summary carries internal detail); every other approval keeps its
 * summary unchanged.
 */
export function approvalListTitle(
  approval: { kind: string; payload: unknown; summary: string },
  formatMessage: FormatMessage,
): string {
  const view = parseKnowledgeQuarantine(approval.kind, approval.payload, approval.summary);
  if (view?.variant !== 'conflict') return approval.summary;
  return view.subject
    ? formatMessage({ id: 'approval.knowledge.rowTitle' }, { subject: view.subject })
    : formatMessage({ id: 'approval.knowledge.rowTitleNoSubject' });
}
