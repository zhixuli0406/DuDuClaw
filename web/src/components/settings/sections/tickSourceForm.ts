import type { TickSourceConfig, TickSourceKind, TickSourceUpsert } from '@/lib/config-rpc-types';

/**
 * Editable draft of one `[[tick.sources]]` entry (strings for the inputs) and
 * its mapping onto `tick.sources.upsert` (v1.68 W2). Shapes follow the
 * gateway's `tick_sources_rpc.rs` / `tick_config.rs`:
 * - `command` is an argv array (one argument per line here),
 * - `json_fields` maps a field name to a JSON pointer (`price = /data/price`),
 * - an absent key keeps the stored value, `null` removes it,
 * - `headers` sent as an object replaces the stored map; values are never
 *   returned, only `headers_count`.
 */
export interface TickSourceDraft {
  id: string;
  kind: TickSourceKind;
  enabled: boolean;
  url: string;
  /** One argv element per line. */
  command: string;
  path: string;
  intervalSecs: string;
  /** `name = /json/pointer` per line. */
  jsonFields: string;
  maxEventsPerMinute: string;
  baselineMaxAgeSecs: string;
  subscribe: string;
  /** `Name: value` per line. Empty + !clearHeaders ⇒ keep the stored ones. */
  headersText: string;
  clearHeaders: boolean;
  /** How many headers are stored server-side (display only). */
  headersCount: number;
}

export const TICK_KINDS: readonly TickSourceKind[] = ['http_poll', 'command', 'file_tail', 'websocket'];

/** Which inputs each kind uses. */
export const KIND_FIELDS: Record<
  TickSourceKind,
  { url: boolean; command: boolean; path: boolean; interval: boolean; headers: boolean; subscribe: boolean }
> = {
  http_poll: { url: true, command: false, path: false, interval: true, headers: true, subscribe: false },
  command: { url: false, command: true, path: false, interval: true, headers: false, subscribe: false },
  file_tail: { url: false, command: false, path: true, interval: false, headers: false, subscribe: false },
  websocket: { url: true, command: false, path: false, interval: true, headers: true, subscribe: true },
};

/** Same rule as `tick_config::is_valid_source_id`. */
export const SOURCE_ID_RE = /^[a-z0-9][a-z0-9-]{0,63}$/;

export function emptyDraft(): TickSourceDraft {
  return {
    id: '',
    kind: 'http_poll',
    enabled: true,
    url: '',
    command: '',
    path: '',
    intervalSecs: '60',
    jsonFields: '',
    maxEventsPerMinute: '',
    baselineMaxAgeSecs: '',
    subscribe: '',
    headersText: '',
    clearHeaders: false,
    headersCount: 0,
  };
}

const numStr = (n: number | undefined) => (typeof n === 'number' ? String(n) : '');

export function draftFromConfig(src: TickSourceConfig): TickSourceDraft {
  const fields = src.json_fields && typeof src.json_fields === 'object' ? src.json_fields : {};
  return {
    id: src.id,
    kind: src.kind,
    enabled: src.enabled !== false,
    url: src.url ?? '',
    command: Array.isArray(src.command) ? src.command.join('\n') : '',
    path: src.path ?? '',
    intervalSecs: numStr(src.interval_secs),
    jsonFields: Object.entries(fields)
      .map(([k, v]) => `${k} = ${v}`)
      .join('\n'),
    maxEventsPerMinute: numStr(src.max_events_per_minute),
    baselineMaxAgeSecs: numStr(src.baseline_max_age_secs),
    subscribe: (src.subscribe ?? []).join('\n'),
    headersText: '',
    clearHeaders: false,
    headersCount: src.headers_count ?? 0,
  };
}

/** Parse `Name: value` lines. The error names a 1-based line number, never
 *  the line itself — a header value is usually a credential and must not end
 *  up in a toast. */
export function parseHeaders(text: string): { headers: Record<string, string> } | { badLine: number } {
  const headers: Record<string, string> = {};
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].trim();
    if (line === '') continue;
    const idx = line.indexOf(':');
    if (idx <= 0 || idx === line.length - 1) return { badLine: i + 1 };
    const name = line.slice(0, idx).trim();
    const value = line.slice(idx + 1).trim();
    if (!/^[A-Za-z0-9!#$%&'*+.^_`|~-]+$/.test(name) || value === '') return { badLine: i + 1 };
    headers[name] = value;
  }
  return { headers };
}

/** Parse `name = /pointer` lines into the json_fields map. */
export function parseJsonFields(text: string): { fields: Record<string, string> } | { badLine: number } {
  const fields: Record<string, string> = {};
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].trim();
    if (line === '') continue;
    const idx = line.indexOf('=');
    if (idx <= 0) return { badLine: i + 1 };
    const name = line.slice(0, idx).trim();
    const pointer = line.slice(idx + 1).trim().replace(/^"|"$/g, '');
    if (!/^[A-Za-z0-9_]{1,64}$/.test(name) || !(pointer === '' || pointer.startsWith('/'))) {
      return { badLine: i + 1 };
    }
    fields[name] = pointer;
  }
  return { fields };
}

export type DraftError = 'id' | 'url' | 'command' | 'path' | 'interval' | 'number' | 'headers' | 'jsonFields';

const lines = (text: string) =>
  text
    .split('\n')
    .map((s) => s.trim())
    .filter((s) => s !== '');

/** Build the `tick.sources.upsert` payload, or name the first problem.
 *  Fields the chosen kind does not use are sent as `null` so a kind change
 *  does not leave stale keys behind. */
export function draftToUpsert(d: TickSourceDraft): { payload: TickSourceUpsert } | { error: DraftError; detail?: string } {
  const id = d.id.trim();
  if (!SOURCE_ID_RE.test(id)) return { error: 'id' };
  const f = KIND_FIELDS[d.kind];
  const payload: TickSourceUpsert = { id, kind: d.kind, enabled: d.enabled };

  if (f.url) {
    const url = d.url.trim();
    if (url === '') return { error: 'url' };
    payload.url = url;
  } else {
    payload.url = null;
  }
  if (f.command) {
    const argv = lines(d.command);
    if (argv.length === 0) return { error: 'command' };
    payload.command = argv;
  } else {
    payload.command = null;
  }
  if (f.path) {
    const path = d.path.trim();
    if (path === '') return { error: 'path' };
    payload.path = path;
  } else {
    payload.path = null;
  }

  // Blank optional number ⇒ null (back to the gateway default).
  const optNum = (s: string): number | null | 'bad' => {
    const v = s.trim();
    if (v === '') return null;
    const n = Number(v);
    return Number.isInteger(n) && n >= 0 ? n : 'bad';
  };
  if (f.interval) {
    const n = optNum(d.intervalSecs);
    if (n === 'bad' || n === 0) return { error: 'interval' };
    payload.interval_secs = n;
  } else {
    payload.interval_secs = null;
  }
  const mepm = optNum(d.maxEventsPerMinute);
  const bmas = optNum(d.baselineMaxAgeSecs);
  if (mepm === 'bad' || bmas === 'bad') return { error: 'number' };
  payload.max_events_per_minute = mepm;
  payload.baseline_max_age_secs = bmas;

  const jf = parseJsonFields(d.jsonFields);
  if ('badLine' in jf) return { error: 'jsonFields', detail: String(jf.badLine) };
  payload.json_fields = Object.keys(jf.fields).length > 0 ? jf.fields : null;

  if (f.subscribe) {
    const sub = lines(d.subscribe);
    payload.subscribe = sub.length > 0 ? sub : null;
  } else {
    payload.subscribe = null;
  }

  if (f.headers) {
    if (d.clearHeaders) {
      payload.headers = null;
    } else if (d.headersText.trim() !== '') {
      const parsed = parseHeaders(d.headersText);
      if ('badLine' in parsed) return { error: 'headers', detail: String(parsed.badLine) };
      payload.headers = parsed.headers;
    }
    // else: omitted ⇒ the stored headers stay.
  } else {
    payload.headers = null;
  }
  return { payload };
}
