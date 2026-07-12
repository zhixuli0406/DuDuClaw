/**
 * Approval UX logic layer (feature gap U2 — evidence-based approval redesign).
 *
 * Every function here is pure (or a thin localStorage wrapper over a pure core)
 * so the risk classification, plan-fact extraction, and fatigue accounting are
 * unit-testable in isolation from React. Design evidence:
 *
 *  - arXiv:2604.04918 — reviewing the *plan* beats catching execution errors:
 *    the panel leads with what the agent intends, not a bare approve/deny.
 *  - arXiv:2606.05391 — humans are "good-enough" reviewers who satisfice: the
 *    UI aids heuristic verification (risk badges, one-click spot-check) instead
 *    of assuming the operator reads every byte.
 *  - arXiv:2606.08919 — approvers fatigue: we surface the day's approval volume
 *    and group same-kind batches to lower cognitive load (never auto-approve).
 *  - arXiv:2605.28571 — over-granular uncertainty breeds over-trust: risk is
 *    shown at the whole-action level only, never token-by-token.
 */

/** Three-level risk band. Mapped to emerald / amber / rose tokens in the UI. */
export type RiskLevel = 'low' | 'medium' | 'high';

/** Badge tone per risk band (matches `@/components/ui` Badge tones). */
export type RiskTone = 'success' | 'warning' | 'danger';

const RISK_ORDER: Record<RiskLevel, number> = { low: 0, medium: 1, high: 2 };

/**
 * Baseline risk by action kind. Reversible / curated actions are low; actions
 * that reach the open web, install runnable code, or hire an autonomous worker
 * are higher. Unknown kinds default to `medium` — a fail-safe that flags "look
 * before you leap" without crying wolf on every unrecognised kind (which would
 * itself drive the alarm fatigue U2 is trying to prevent).
 */
const BASE_RISK: Record<string, RiskLevel> = {
  strategic_plan: 'low', // just a plan — reversible, nothing executes yet
  wiki_ingest: 'low', // writes curated knowledge, editable after the fact
  skill_activation: 'medium',
  tool_call: 'medium',
  browser_action: 'medium', // reaches the open web; can act on external state
  skill_create: 'high', // installs code that runs later
  agent_hire: 'high', // spins up an autonomous worker that spends budget
};

/** Default when the kind is unrecognised — see BASE_RISK note. */
export const UNKNOWN_KIND_RISK: RiskLevel = 'medium';

function higher(a: RiskLevel, b: RiskLevel): RiskLevel {
  return RISK_ORDER[a] >= RISK_ORDER[b] ? a : b;
}

function asRecord(v: unknown): Record<string, unknown> | null {
  return v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
}

/**
 * Escalate the base risk from payload signals that are deterministic and
 * shape-stable across kinds — chiefly a `safety_report` (skill_create) that
 * failed or is graded medium/high. Never de-escalates below the base.
 */
function payloadEscalation(payload: unknown): RiskLevel | null {
  const obj = asRecord(payload);
  if (!obj) return null;
  const sr = asRecord(obj.safety_report);
  if (sr) {
    if (sr.passed === false) return 'high';
    const lvl = typeof sr.risk_level === 'string' ? sr.risk_level.toLowerCase() : '';
    if (lvl === 'critical' || lvl === 'high') return 'high';
    if (lvl === 'medium') return 'medium';
  }
  return null;
}

/**
 * Classify an approval's risk band from its action kind and (optionally) its
 * payload. Pure and deterministic — the single source of truth for both the
 * row badge and the detail panel.
 */
export function approvalRisk(kind: string, payload?: unknown): RiskLevel {
  const base = BASE_RISK[kind] ?? UNKNOWN_KIND_RISK;
  const bump = payloadEscalation(payload);
  return bump ? higher(base, bump) : base;
}

/** Map a risk band to its Badge tone. */
export function riskTone(level: RiskLevel): RiskTone {
  return level === 'low' ? 'success' : level === 'medium' ? 'warning' : 'danger';
}

/** High-risk actions require a second confirmation (ConfirmDialog) before approve. */
export function riskNeedsConfirm(level: RiskLevel): boolean {
  return level === 'high';
}

// ── Plan facts (heuristic verification aids) ────────────────────────────────

/** Structured facts pulled from an opaque payload to summarise the plan. */
export interface PlanFacts {
  /** Tools / capabilities the action will invoke. */
  tools: string[];
  /** Concrete targets the action touches (urls, paths, spaces, channels). */
  targets: string[];
  /** A declared scope string, when the payload carries one. */
  scope?: string;
}

function pushStrings(out: string[], v: unknown): void {
  if (typeof v === 'string') {
    const s = v.trim();
    if (s) out.push(s);
  } else if (Array.isArray(v)) {
    for (const item of v) if (typeof item === 'string' && item.trim()) out.push(item.trim());
  }
}

const TOOL_KEYS = ['tool', 'tools', 'tool_name', 'tool_names', 'command', 'skill', 'action'];
const TARGET_KEYS = ['url', 'urls', 'target', 'targets', 'path', 'paths', 'file', 'files', 'space', 'channel', 'host'];

/**
 * Best-effort extraction of "what will this touch" from a payload whose shape
 * varies by kind. Deterministic; returns empty arrays for non-object payloads
 * so the UI can decide whether to render the facts strip at all.
 */
export function extractPlanFacts(payload: unknown): PlanFacts {
  const obj = asRecord(payload);
  const tools: string[] = [];
  const targets: string[] = [];
  let scope: string | undefined;
  if (obj) {
    for (const k of TOOL_KEYS) pushStrings(tools, obj[k]);
    for (const k of TARGET_KEYS) pushStrings(targets, obj[k]);
    if (typeof obj.scope === 'string' && obj.scope.trim()) scope = obj.scope.trim();
  }
  return {
    tools: dedupe(tools),
    targets: dedupe(targets),
    scope,
  };
}

function dedupe(xs: string[]): string[] {
  return [...new Set(xs)];
}

/** True when there is anything worth showing in the plan-facts strip. */
export function hasPlanFacts(f: PlanFacts): boolean {
  return f.tools.length > 0 || f.targets.length > 0 || Boolean(f.scope);
}

// ── Fatigue accounting (arXiv:2606.08919) ───────────────────────────────────

/** ≥ this many same-kind pending approvals → surface a "same batch" hint. */
export const SIMILAR_BATCH_THRESHOLD = 3;
/** ≥ this many approvals decided today → surface a gentle fatigue nudge. */
export const FATIGUE_NUDGE_THRESHOLD = 10;

/** Count pending approvals by kind. Pure. */
export function countByKind(kinds: readonly string[]): Record<string, number> {
  const out: Record<string, number> = {};
  for (const k of kinds) out[k] = (out[k] ?? 0) + 1;
  return out;
}

/** A same-kind cluster large enough to hint at batching (never auto-approved). */
export interface SimilarBatch {
  kind: string;
  count: number;
}

/** Kinds whose pending count reaches the batch threshold, largest first. */
export function similarBatches(
  kinds: readonly string[],
  threshold = SIMILAR_BATCH_THRESHOLD,
): SimilarBatch[] {
  const counts = countByKind(kinds);
  return Object.entries(counts)
    .filter(([, n]) => n >= threshold)
    .map(([kind, count]) => ({ kind, count }))
    .sort((a, b) => b.count - a.count);
}

// ── Daily approval counter (thin localStorage wrapper over a pure core) ──────

const DAILY_KEY = 'duduclaw:inbox:approvedToday';

export interface DailyApprovalCount {
  /** ISO day (YYYY-MM-DD) the count belongs to. */
  date: string;
  count: number;
}

/** ISO calendar day for a Date, in the local-independent UTC bucket. */
export function isoDay(now: Date): string {
  return now.toISOString().slice(0, 10);
}

/**
 * Pure rollover: the stored count survives only within its own day; a new day
 * resets to zero. Separated from storage so it is exhaustively testable.
 */
export function dailyRollover(stored: DailyApprovalCount | null, todayISO: string): number {
  return stored && stored.date === todayISO ? stored.count : 0;
}

function readRaw(): DailyApprovalCount | null {
  try {
    const raw = localStorage.getItem(DAILY_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as DailyApprovalCount;
    if (typeof parsed?.date === 'string' && typeof parsed?.count === 'number') return parsed;
  } catch {
    /* corrupt / unavailable storage → treat as empty */
  }
  return null;
}

/** How many approvals were decided (approved) today. */
export function readApprovedToday(now: Date = new Date()): number {
  return dailyRollover(readRaw(), isoDay(now));
}

/** Record one more approval today and return the new running count. */
export function bumpApprovedToday(now: Date = new Date()): number {
  const today = isoDay(now);
  const next = dailyRollover(readRaw(), today) + 1;
  try {
    localStorage.setItem(DAILY_KEY, JSON.stringify({ date: today, count: next } satisfies DailyApprovalCount));
  } catch {
    /* storage unavailable — the count is best-effort UX, not a source of truth */
  }
  return next;
}
