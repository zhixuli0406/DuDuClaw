/**
 * User-facing error normalization (rubric P05 "error states are recoverable").
 *
 * Two deliberately separate jobs:
 *
 *  - `classifyError` maps any thrown value onto a small closed set of reasons
 *    so the UI can say *what went wrong* in the user's own words instead of
 *    echoing a stack trace. The reason maps to an `errorState.reason.*` key.
 *  - `sanitizeErrorDetail` produces the short technical string that belongs
 *    behind a "technical details" disclosure: one line, secrets redacted,
 *    HTML error pages stripped, length-capped.
 *
 * Nothing here ever returns a raw thrown value — a page must never render one
 * directly. `ErrorState` (components/mds/error-state.tsx) is the intended
 * consumer of both functions.
 */

export type ErrorKind =
  | 'network'
  | 'timeout'
  | 'auth'
  | 'notFound'
  | 'server'
  | 'unknown';

/** i18n key holding the plain-language explanation for a classified error. */
export function errorReasonKey(kind: ErrorKind): string {
  return `errorState.reason.${kind}`;
}

/** Best-effort extraction of a message string out of any thrown value. */
function rawText(err: unknown): string {
  if (err === null || err === undefined) return '';
  if (typeof err === 'string') return err;
  if (err instanceof Error) return err.message || err.name || '';
  if (typeof err === 'object') {
    const rec = err as Record<string, unknown>;
    for (const key of ['message', 'error', 'detail', 'reason', 'description']) {
      const v = rec[key];
      if (typeof v === 'string' && v.trim()) return v;
    }
    try {
      return JSON.stringify(err);
    } catch {
      return String(err);
    }
  }
  return String(err);
}

/** Numeric HTTP status carried on the thrown value, when there is one. */
function statusOf(err: unknown): number | undefined {
  if (typeof err === 'object' && err !== null) {
    const rec = err as Record<string, unknown>;
    for (const key of ['status', 'statusCode', 'code']) {
      const v = rec[key];
      if (typeof v === 'number' && Number.isFinite(v)) return v;
    }
  }
  return undefined;
}

/**
 * Status code stated inside the message text. Deliberately narrow: only
 * digit runs introduced by an explicit marker ("HTTP 500", "status: 404",
 * "503:") count, so an id or a byte count can't masquerade as a status.
 */
function statusFromText(text: string): number | undefined {
  const marked = /(?:https?\s|http|status(?:\s*code)?|error|code)\s*[:=]?\s*([45]\d{2})\b/.exec(
    text
  );
  if (marked) return Number(marked[1]);
  const leading = /^\s*([45]\d{2})\b/.exec(text);
  if (leading) return Number(leading[1]);
  return undefined;
}

const TIMEOUT_RE = /(timeout|timed\s*out|etimedout|deadline exceeded|逾時|超時)/;
const NETWORK_RE =
  /(failed to fetch|network\s*error|networkerror|load failed|network request failed|err_connection|econnrefused|econnreset|enotfound|ehostunreach|connection (?:refused|reset|closed|error)|websocket|not connected|disconnected|offline|無法連線|連線失敗|連線中斷)/;
const AUTH_RE =
  /(unauthori[sz]ed|forbidden|permission denied|access denied|invalid (?:api )?key|invalid token|authentication|not authenticated|未授權|權限不足|驗證失敗)/;
const NOT_FOUND_RE = /(not found|does not exist|no such |查無|不存在)/;
const SERVER_RE =
  /(internal server error|bad gateway|service unavailable|server error|upstream|panicked)/;

/**
 * Map a thrown value onto a plain-language reason. Unrecognised errors stay
 * `unknown` rather than being force-fit into a category — a wrong explanation
 * is worse than a generic one.
 */
export function classifyError(err: unknown): ErrorKind {
  const text = rawText(err).toLowerCase();
  const status = statusOf(err) ?? statusFromText(text);
  if (status !== undefined) {
    if (status === 401 || status === 403) return 'auth';
    if (status === 404) return 'notFound';
    if (status === 408 || status === 504) return 'timeout';
    if (status >= 500) return 'server';
  }
  if (!text) return 'unknown';
  if (TIMEOUT_RE.test(text)) return 'timeout';
  if (NETWORK_RE.test(text)) return 'network';
  if (AUTH_RE.test(text)) return 'auth';
  if (NOT_FOUND_RE.test(text)) return 'notFound';
  if (SERVER_RE.test(text)) return 'server';
  return 'unknown';
}

const HTML_TAG_RE = /<[a-z!/][^>]*>/gi;
const ENTITIES: ReadonlyArray<readonly [RegExp, string]> = [
  [/&nbsp;/gi, ' '],
  [/&amp;/gi, '&'],
  [/&lt;/gi, '<'],
  [/&gt;/gi, '>'],
  [/&quot;/gi, '"'],
  [/&#0?39;/gi, "'"],
];

/**
 * `key=value` / `"key": "value"` pairs whose value is a credential. The value
 * lookahead skips an auth scheme keyword and an already-redacted marker so
 * `Authorization: Bearer …` stays readable as `Authorization: Bearer ***`.
 */
const SECRET_PAIR_RE =
  /((?:api[_-]?key|access[_-]?key|secret[_-]?key|secret|token|password|passwd|pwd|authorization|auth[_-]?token|credential|session[_-]?id)["']?\s*[:=]\s*["']?)((?!\*\*\*|bearer\b|basic\b)[^\s"',;&})\]]+)/gi;
const BEARER_RE = /\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{4,}/gi;
/** Vendor-prefixed keys (sk-…, xoxb-…, ghp_…, AIza…) leak verbatim otherwise. */
const VENDOR_KEY_RE =
  /\b(?:sk|pk|rk|xox[baprs]|ghp|gho|ghs|ghu|github_pat|shpat|dop_v1|glpat)[-_][A-Za-z0-9_-]{8,}\b|\bAIza[A-Za-z0-9_-]{20,}\b/g;
/** Long opaque mixed-case+digit runs — the shape of a bare token. */
const OPAQUE_RUN_RE =
  /\b(?=[A-Za-z0-9+/_-]*[0-9])(?=[A-Za-z0-9+/_-]*[A-Za-z])[A-Za-z0-9+/_-]{32,}={0,2}\b/g;
/** Query strings routinely carry keys/ids; the path alone is enough context. */
const URL_QUERY_RE = /(https?:\/\/[^\s"'<>]*?)\?[^\s"'<>]*/gi;

const REDACTED = '***';

/**
 * Turn any thrown value into a short, safe, single-line technical detail.
 *
 * Redaction happens *before* truncation (same rule as the Rust audit log): a
 * secret cut in half is still a leak. Returns '' when there is nothing to show,
 * which callers treat as "no details disclosure".
 */
export function sanitizeErrorDetail(err: unknown, maxChars = 240): string {
  let text = rawText(err);
  if (!text) return '';

  if (/<[a-z!/][^>]*>/i.test(text)) {
    text = text.replace(HTML_TAG_RE, ' ');
    for (const [re, to] of ENTITIES) text = text.replace(re, to);
  }

  // Stack traces and multi-line backend dumps: the first line is the message.
  const firstLine = text.split(/\r?\n/).find((l) => l.trim().length > 0) ?? '';
  text = firstLine;

  text = text.replace(URL_QUERY_RE, `$1?${REDACTED}`);
  text = text.replace(BEARER_RE, `$1 ${REDACTED}`);
  text = text.replace(SECRET_PAIR_RE, `$1${REDACTED}`);
  text = text.replace(VENDOR_KEY_RE, REDACTED);
  text = text.replace(OPAQUE_RUN_RE, REDACTED);

  text = text.replace(/\s+/g, ' ').trim();
  if (!text) return '';

  // Codepoint-wise so a truncation can't split a surrogate pair (emoji/CJK ext).
  const chars = Array.from(text);
  if (chars.length <= maxChars) return text;
  return `${chars.slice(0, maxChars).join('').trimEnd()}…`;
}
