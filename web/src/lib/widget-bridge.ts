// Custom-widget sandbox runtime (design:
// commercial/docs/custom-widgets-design-2026-07-16.md).
//
// A custom widget's HTML runs inside `<iframe sandbox="allow-scripts">` —
// no `allow-same-origin`, so the document gets a UNIQUE origin: it cannot
// touch the dashboard DOM, cookies, localStorage or the JWT. Its only door
// to data is the postMessage bridge below, which proxies a fixed read-only
// allowlist through the CURRENT user's api client (role/data-scope applies
// automatically). An injected CSP blocks every external resource and
// network call, so bridged data cannot be exfiltrated anywhere.

import { api } from '@/lib/api';

/** Messages the iframe sends up. */
interface RpcRequest {
  type: 'duduclaw:rpc';
  seq: number;
  method: string;
  params?: unknown;
}
interface ResizeMsg {
  type: 'duduclaw:resize';
  height: number;
}
export type WidgetOutboundMsg = RpcRequest | ResizeMsg;

/** Per-widget bridge call budget: sliding 1s window. */
const RATE_LIMIT_PER_SECOND = 10;

/**
 * The read-only method allowlist. Fail-closed: anything not listed is
 * rejected. Each proxy SHAPES the payload down to what a dashboard card
 * legitimately needs — widgets never receive raw store dumps.
 */
const BRIDGE_METHODS: Record<string, () => Promise<unknown>> = {
  'agents.summary': async () => {
    const r = await api.agents.list();
    return {
      agents: r.agents.map((a) => ({
        name: a.name,
        display_name: a.display_name,
        role: a.role,
        department: a.department ?? '',
        archived: Boolean(a.archived),
      })),
    };
  },
  'tasks.summary': async () => {
    const r = await api.tasks.list();
    const byStatus: Record<string, number> = {};
    const today = new Date();
    const isToday = (iso?: string) => {
      if (!iso) return false;
      const d = new Date(iso);
      return (
        d.getFullYear() === today.getFullYear() &&
        d.getMonth() === today.getMonth() &&
        d.getDate() === today.getDate()
      );
    };
    for (const t of r.tasks) byStatus[t.status] = (byStatus[t.status] ?? 0) + 1;
    return {
      total: r.tasks.length,
      by_status: byStatus,
      // Computed parent-side so "today" widgets don't need date math (the
      // 2026-07-16 live generation test showed models want this directly).
      completed_today: r.tasks.filter((t) => isToday(t.completed_at)).length,
      recent: r.tasks.slice(0, 10).map((t) => ({
        id: t.id,
        title: t.title,
        status: t.status,
        assignee: t.assigned_to || '',
        completed_at: t.completed_at ?? null,
      })),
    };
  },
  'cost.summary': () => api.cost.summary(24),
  'channels.status': async () => {
    const r = await api.channels.status();
    return { channels: r.channels.map((c) => ({ channel: c.name, connected: c.connected })) };
  },
  'system.status': () => api.system.status(),
};

export const BRIDGE_METHOD_NAMES = Object.keys(BRIDGE_METHODS);

/**
 * How long a resolved (or in-flight) bridge result stays shared across
 * callers. The `/widgets` gallery mounts a live thumbnail — a real sandboxed
 * iframe, bridge and all — for every widget card that scrolls into view, so
 * a single page load can easily have a dozen+ iframes all requesting the
 * same 'agents.summary' / 'tasks.summary' within milliseconds of each other.
 * Without coalescing, that's a dozen+ duplicate API calls for identical data.
 */
const BRIDGE_CACHE_TTL_MS = 15_000;

interface CacheEntry {
  at: number;
  promise: Promise<unknown>;
}

/**
 * method → shared result. Stores the PROMISE, not the resolved value, so
 * concurrent callers that arrive before the first call settles share the
 * same in-flight request instead of each firing their own.
 */
const resultCache = new Map<string, CacheEntry>();

/** Test-only reset so each spec file starts from a clean cache. */
export function clearBridgeCache(): void {
  resultCache.clear();
}

/** Serve `method` from the shared cache, refreshing it (and the in-flight
 *  entry) once the TTL lapses. A rejected call is evicted immediately —
 *  failures are never cached, so the next caller gets a fresh attempt. */
function callCached(method: string, proxy: () => Promise<unknown>): Promise<unknown> {
  const now = Date.now();
  const cached = resultCache.get(method);
  if (cached && now - cached.at < BRIDGE_CACHE_TTL_MS) {
    return cached.promise;
  }
  const promise = proxy().catch((e: unknown) => {
    resultCache.delete(method);
    throw e;
  });
  resultCache.set(method, { at: now, promise });
  return promise;
}

/**
 * Handle one message coming out of a widget iframe. Returns the reply to
 * post back (or null for resize/no-reply messages). The CALLER verifies
 * `event.source` is its own iframe before invoking this.
 */
export async function handleWidgetMessage(
  msg: WidgetOutboundMsg,
  rateWindow: number[],
): Promise<{ seq: number; ok: boolean; result?: unknown; error?: string } | null> {
  if (msg.type !== 'duduclaw:rpc') return null;
  const now = Date.now();
  while (rateWindow.length > 0 && now - rateWindow[0] > 1000) rateWindow.shift();
  if (rateWindow.length >= RATE_LIMIT_PER_SECOND) {
    return { seq: msg.seq, ok: false, error: 'rate limit exceeded (10 req/s)' };
  }
  // Rate limit is charged per-widget even on a cache hit — keeps the budget
  // simple and predictable rather than special-casing "free" cached reads.
  rateWindow.push(now);

  const proxy = BRIDGE_METHODS[msg.method];
  if (!proxy) {
    return { seq: msg.seq, ok: false, error: `method '${msg.method}' is not allowed` };
  }
  try {
    const result = await callCached(msg.method, proxy);
    return { seq: msg.seq, ok: true, result };
  } catch (e) {
    return { seq: msg.seq, ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

// ── srcDoc composition ──────────────────────────────────────

/**
 * CSP injected into every widget document. `default-src 'none'` +
 * no `connect-src` kills fetch/XHR/WebSocket/external loads; inline
 * script/style and data: images are all a self-contained card needs.
 */
const WIDGET_CSP =
  "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:";

/** The in-iframe SDK: `duduclaw.call(method, params)` + theme + auto-resize. */
const WIDGET_SHIM = `
(function () {
  var seq = 0, pending = {}, themeCbs = [], theme = null;
  window.duduclaw = {
    call: function (method, params) {
      return new Promise(function (resolve, reject) {
        var id = ++seq;
        pending[id] = { resolve: resolve, reject: reject };
        parent.postMessage({ type: 'duduclaw:rpc', seq: id, method: method, params: params || {} }, '*');
        setTimeout(function () {
          if (pending[id]) { delete pending[id]; reject(new Error('duduclaw.call timeout')); }
        }, 15000);
      });
    },
    onTheme: function (cb) { themeCbs.push(cb); if (theme) cb(theme); },
  };
  window.addEventListener('message', function (e) {
    var d = e.data || {};
    if (d.type === 'duduclaw:rpc:result' && pending[d.seq]) {
      var p = pending[d.seq]; delete pending[d.seq];
      d.ok ? p.resolve(d.result) : p.reject(new Error(d.error || 'bridge error'));
    } else if (d.type === 'duduclaw:theme') {
      theme = d.mode;
      document.documentElement.setAttribute('data-theme', d.mode);
      themeCbs.forEach(function (cb) { cb(d.mode); });
    }
  });
  var report = function () {
    parent.postMessage({ type: 'duduclaw:resize', height: document.documentElement.scrollHeight }, '*');
  };
  new ResizeObserver(report).observe(document.documentElement);
  window.addEventListener('load', report);
})();
`;

/** Calm Glass base tokens so widgets inherit the dashboard look by default. */
const WIDGET_BASE_CSS = `
:root { color-scheme: light; --bg: transparent; --fg: #1c1917; --muted: #78716c; --accent: #f59e0b; --card: #fafaf9; --border: #e7e5e4; }
:root[data-theme="dark"] { color-scheme: dark; --fg: #fafaf9; --muted: #a8a29e; --card: #292524; --border: #44403c; }
html, body { margin: 0; padding: 0; background: var(--bg); color: var(--fg);
  font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif; font-size: 14px; line-height: 1.6; }
`;

/**
 * Wrap stored widget HTML (a body fragment by contract) into the sandbox
 * document: CSP + SDK shim + theme tokens are injected at RENDER time, so
 * runtime upgrades apply to every existing widget without migration.
 */
export function composeWidgetSrcDoc(userHtml: string, themeMode: 'light' | 'dark'): string {
  return [
    '<!doctype html><html data-theme="' + themeMode + '"><head><meta charset="utf-8">',
    `<meta http-equiv="Content-Security-Policy" content="${WIDGET_CSP}">`,
    `<style>${WIDGET_BASE_CSS}</style>`,
    `<script>${WIDGET_SHIM}<\/script>`,
    '</head><body>',
    userHtml,
    '</body></html>',
  ].join('\n');
}
