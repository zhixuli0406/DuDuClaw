import type { RedactionModelState, RedactionModelStatus } from '@/lib/api';

// ── "AI 智慧偵測" row — pure helpers (WP-N2) ─────────────────────────────
//
// DESIGN-redaction-ner-and-custom-rules-2026-09 §12.1/§13.4. Mirrors the
// house convention set by `redactionMyRules.ts`: this file holds ONLY pure,
// side-effect-free functions and their types, so `redactionAiDetection.
// test.ts` can exercise them without a WebSocket, a timer, or an
// IntlProvider. The `redaction.model.*` RPC calls and the 2s polling
// `setInterval` itself live in `RedactionAiDetectionRow.tsx`.

/** §5's fixed OPF-label → DuDuClaw-category mapping, in the order the canvas
 *  chips are shown (人名／地址／Email／電話／網址／日期／帳號／密鑰). This is
 *  a closed, documented list — NOT derived from
 *  `RedactionProfileInfo.categories` — because the expanded row must always
 *  show exactly these 8 chips regardless of how the backend happens to order
 *  (or in the future extend) that field. */
export const AI_DETECT_CATEGORIES = [
  'PERSON',
  'ADDRESS',
  'EMAIL',
  'PHONE',
  'URL',
  'DATE',
  'ACCOUNT_NUMBER',
  'SECRET',
] as const;

// ── byte / progress formatting ──────────────────────────────────────────

const GB = 1024 * 1024 * 1024;
const MB = 1024 * 1024;

/** Mirrors `LocalModelsPage.tsx`'s `fmtBytes` — GB with one decimal above
 *  1024MB, whole MB (floor of 1) below. Kept as its own copy rather than a
 *  shared import: that page's helper is a page-local, unexported function. */
export function formatModelBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 MB';
  if (bytes >= GB) return `${(bytes / GB).toFixed(1)} GB`;
  return `${Math.max(1, Math.round(bytes / MB))} MB`;
}

/** `done_bytes / total_bytes` as a clamped 0-100 integer percent, or `null`
 *  when `total_bytes` isn't known yet (progress bar then renders an
 *  indeterminate sliver instead of claiming a number it doesn't have). */
export function downloadPercent(doneBytes: number, totalBytes: number): number | null {
  if (!Number.isFinite(totalBytes) || totalBytes <= 0) return null;
  return Math.max(0, Math.min(100, Math.round((doneBytes / totalBytes) * 100)));
}

/** The "下載中 · X / Y MB" caption's `X / Y` half. When both sides happen to
 *  land in the same unit, the first number's own unit is dropped ("348 / 917
 *  MB") — matching the canvas copy exactly; a rare cross-unit case ("870 MB /
 *  1.2 GB") keeps both units so neither number reads as wrong. */
export function formatProgressCaption(doneBytes: number, totalBytes: number): string {
  const done = formatModelBytes(doneBytes);
  if (!Number.isFinite(totalBytes) || totalBytes <= 0) return done;
  const total = formatModelBytes(totalBytes);
  const [doneNum, doneUnit] = done.split(' ');
  const [, totalUnit] = total.split(' ');
  return doneUnit === totalUnit ? `${doneNum} / ${total}` : `${done} / ${total}`;
}

/** Truncates a (typically long HF commit hash) revision string for the
 *  "已安裝 · <revision>" chip. `null`/empty in ⇒ `null` out, so the caller
 *  can fall back to a revision-less "已安裝" message instead of rendering
 *  "已安裝 ·" with nothing after it. */
export function shortRevision(revision: string | null | undefined, len = 8): string | null {
  if (!revision) return null;
  return revision.length > len ? revision.slice(0, len) : revision;
}

// ── status → view ────────────────────────────────────────────────────────

/** Everything the row needs to render, derived once from the raw
 *  `RedactionModelStatus` (or its absence, on a load failure) so the
 *  component's JSX is a plain switch over `kind` with no state-shape
 *  guessing scattered through it. */
export type AiDetectView =
  /** `redaction.model.status` hasn't resolved yet, or the call itself
   *  failed — deliberately distinct from the backend's own `"error"` state
   *  (a failed download): this means "we don't know", not "it broke". */
  | { kind: 'unknown' }
  | { kind: 'absent'; sizeBytes: number }
  | {
      kind: 'downloading';
      doneBytes: number;
      totalBytes: number;
      pct: number | null;
      file: string | null;
    }
  | { kind: 'ready'; revision: string | null; hasUsage: boolean; avgLatencyMs: number | null }
  | { kind: 'error'; message: string | null };

/** `ready` and `loaded` (§13.4: "已安裝" / actively-loaded-in-memory) render
 *  identically in this UI — the row doesn't distinguish "installed" from
 *  "installed and currently warm in RAM". */
const READY_STATES: ReadonlySet<RedactionModelState> = new Set(['ready', 'loaded']);

export function deriveAiDetectView(status: RedactionModelStatus | null): AiDetectView {
  if (!status) return { kind: 'unknown' };
  if (status.state === 'absent') return { kind: 'absent', sizeBytes: status.size_bytes };
  if (status.state === 'downloading') {
    const doneBytes = status.progress?.done_bytes ?? 0;
    const totalBytes = status.progress?.total_bytes ?? status.size_bytes;
    return {
      kind: 'downloading',
      doneBytes,
      totalBytes,
      pct: downloadPercent(doneBytes, totalBytes),
      file: status.progress?.file ?? null,
    };
  }
  if (READY_STATES.has(status.state)) {
    return {
      kind: 'ready',
      revision: shortRevision(status.model_revision),
      hasUsage: status.calls > 0,
      avgLatencyMs: status.avg_latency_ms,
    };
  }
  if (status.state === 'error') return { kind: 'error', message: status.error };
  // Defensive: an unrecognised future state degrades to "we don't know"
  // rather than crashing the row or silently rendering a stale chip.
  return { kind: 'unknown' };
}

/** §13.4 "下載中每 2 秒輪詢" — poll ONLY while a download is actually moving.
 *  Every other view (including the brief gap right after clicking "下載模
 *  型", before the first `downloading` status lands) is a no-op tick saved. */
export function shouldPollModelStatus(view: AiDetectView): boolean {
  return view.kind === 'downloading';
}

/** §12.1: "未安裝就勾選 → 立即提示…不擋勾選" — the warn line under the row.
 *  Deliberately excludes `unknown` (a load failure says nothing about
 *  whether the model is actually missing, so it must not claim it is) and
 *  `ready` (the whole point of the warning). */
export function needsNotReadyWarning(view: AiDetectView, checked: boolean): boolean {
  return checked && (view.kind === 'absent' || view.kind === 'downloading' || view.kind === 'error');
}
