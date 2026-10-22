import { client } from './ws-client';

/** Availability of a cited source for the current viewer. */
export type CitationState = 'available' | 'deleted' | 'restricted';

/** One knowledge-base source cited by an assistant reply (`[n]` marker). */
export interface ChatCitation {
  readonly n: number;
  /** Null when the passage cannot be opened (deleted / restricted). */
  readonly chunk_id: number | null;
  readonly source_id: string;
  readonly title: string;
  readonly locator: string;
  readonly excerpt: string;
  readonly state: CitationState;
}

/** Row of `knowledge.citations.for_session`. */
export interface SessionCitationRow extends ChatCitation {
  readonly turn_id: string | null;
  readonly assistant_message_id: number | null;
}

/** Result of `knowledge.passage.get` (server already cut before/match/after). */
export interface KnowledgePassage {
  readonly chunk_id: number;
  readonly source_id: string;
  readonly collection_id: string;
  readonly title: string;
  readonly locator_text: string;
  readonly heading_path: string[];
  readonly segment_label: string;
  readonly before: string;
  readonly match: string;
  readonly after: string;
  readonly prev_chunk_id: number | null;
  readonly next_chunk_id: number | null;
  readonly suspect: boolean;
}

export const MAX_CITATIONS = 30;

const str = (v: unknown): string => (typeof v === 'string' ? v : '');
const numOrNull = (v: unknown): number | null =>
  typeof v === 'number' && Number.isFinite(v) ? v : null;
const STATES: ReadonlySet<string> = new Set(['available', 'deleted', 'restricted']);

function parseOne(raw: unknown, requireState: boolean): ChatCitation | undefined {
  if (raw === null || typeof raw !== 'object') return undefined;
  const r = raw as Record<string, unknown>;
  if (typeof r.n !== 'number' || !Number.isInteger(r.n) || r.n < 1 || r.n > 99) return undefined;
  let state: CitationState = 'available';
  if (r.state !== undefined) {
    if (typeof r.state !== 'string' || !STATES.has(r.state)) return undefined;
    state = r.state as CitationState;
  } else if (requireState) {
    return undefined;
  }
  return {
    n: r.n,
    chunk_id: numOrNull(r.chunk_id),
    source_id: str(r.source_id),
    title: str(r.title),
    locator: str(r.locator),
    excerpt: str(r.excerpt),
    state,
  };
}

/**
 * Validate the `citations` array of an `assistant_done` frame. Keeps only
 * well-formed entries (unique `n`), caps at 30; returns undefined when nothing
 * usable remains so old frames behave exactly as before. Pure.
 */
export function parseChatCitations(raw: unknown): readonly ChatCitation[] | undefined {
  if (!Array.isArray(raw)) return undefined;
  const out: ChatCitation[] = [];
  const seen = new Set<number>();
  for (const item of raw) {
    const c = parseOne(item, false);
    if (!c || seen.has(c.n)) continue;
    seen.add(c.n);
    out.push(c);
    if (out.length >= MAX_CITATIONS) break;
  }
  return out.length > 0 ? out : undefined;
}

/** Validate the rows of `knowledge.citations.for_session`. Pure. */
export function parseSessionCitationRows(raw: unknown): SessionCitationRow[] {
  const list = (raw as { citations?: unknown } | null)?.citations;
  if (!Array.isArray(list)) return [];
  const rows: SessionCitationRow[] = [];
  for (const item of list) {
    const c = parseOne(item, true);
    if (!c) continue;
    const r = item as Record<string, unknown>;
    rows.push({
      ...c,
      turn_id: typeof r.turn_id === 'string' ? r.turn_id : null,
      assistant_message_id: numOrNull(r.assistant_message_id),
    });
  }
  return rows;
}

/**
 * Attach session citation rows to assistant messages whose stored row id
 * matches `assistant_message_id`. Returns a new array; messages without a
 * match are returned untouched. Pure.
 */
export function attachHistoryCitations<
  M extends { role: string; historyId?: number; citations?: readonly ChatCitation[] },
>(messages: readonly M[], rows: readonly SessionCitationRow[]): M[] {
  const byMsg = new Map<number, ChatCitation[]>();
  for (const row of rows) {
    if (row.assistant_message_id == null) continue;
    const { turn_id: _t, assistant_message_id: _a, ...cite } = row;
    void _t;
    void _a;
    const list = byMsg.get(row.assistant_message_id) ?? [];
    if (list.length < MAX_CITATIONS && !list.some((c) => c.n === cite.n)) list.push(cite);
    byMsg.set(row.assistant_message_id, list);
  }
  return messages.map((m) => {
    if (m.role !== 'assistant' || m.historyId == null) return m;
    const list = byMsg.get(m.historyId);
    return list && list.length > 0 ? { ...m, citations: list } : m;
  });
}

export const knowledgeCitationsApi = {
  passageGet: (chunkId: number) =>
    client.call('knowledge.passage.get', { chunk_id: chunkId }) as Promise<KnowledgePassage>,
  forSession: (sessionId: string) =>
    client.call('knowledge.citations.for_session', {
      session_id: sessionId,
    }) as Promise<{ citations: unknown[] }>,
};

/** Best effort: any failure yields no rows, never an exception. */
export async function fetchSessionCitations(sessionId: string): Promise<SessionCitationRow[]> {
  try {
    return parseSessionCitationRows(await knowledgeCitationsApi.forSession(sessionId));
  } catch {
    return [];
  }
}

export type MarkerSegment =
  | { readonly kind: 'text'; readonly text: string }
  | { readonly kind: 'cite'; readonly n: number; readonly raw: string };

const MARKER_RE = /```[\s\S]*?```|`[^`\n]*`|\[(\d{1,2})\]/g;

/**
 * Split reply text into plain-text and citation-marker segments. A `[n]`
 * becomes a `cite` segment only when `n` is in `validNs`; markers inside code
 * fences / inline code stay plain. Pure — never alters the text content.
 */
export function splitCitationMarkers(text: string, validNs: ReadonlySet<number>): MarkerSegment[] {
  const out: MarkerSegment[] = [];
  let last = 0;
  let buf = '';
  const flush = () => {
    if (buf) {
      out.push({ kind: 'text', text: buf });
      buf = '';
    }
  };
  for (const m of text.matchAll(MARKER_RE)) {
    const idx = m.index ?? 0;
    buf += text.slice(last, idx);
    last = idx + m[0].length;
    if (m[1] !== undefined && validNs.has(Number(m[1]))) {
      flush();
      out.push({ kind: 'cite', n: Number(m[1]), raw: m[0] });
    } else {
      buf += m[0];
    }
  }
  buf += text.slice(last);
  flush();
  return out;
}
