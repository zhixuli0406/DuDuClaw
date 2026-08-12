/**
 * Lightweight toast event bus.
 *
 * The bus lets any module — including code paths that live outside the React
 * tree (API error handlers inside effects, utility modules, etc.) — dispatch
 * toast notifications without needing access to the `useToast()` hook.
 *
 * `ToastProvider` in `components/Toast.tsx` subscribes to this bus and
 * forwards `emit` payloads into its context-local `show()`. Immutable
 * listener array updates keep subscribe/unsubscribe safe under React 19
 * StrictMode double-invocation.
 */

export type ToastVariant = 'success' | 'error' | 'info';

/** An optional clickable action rendered alongside a toast's message (e.g.
 * "前往查看" for a navigation that was suppressed because the user has
 * unsaved edits in a form — see `lib/dashboard-navigate.ts`). */
export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface Toast {
  id: string;
  variant: ToastVariant;
  message: string;
  /** Auto-dismiss timeout in ms. Defaults: success/info=5000, error=7000. */
  durationMs?: number;
  /** Whether the user can close via the X button. Default true. */
  dismissible?: boolean;
  /** Optional clickable action button. */
  action?: ToastAction;
  createdAt: number;
}

export type ToastInput = Omit<Toast, 'id' | 'createdAt'>;
export type ToastOptions = Partial<Omit<Toast, 'id' | 'createdAt' | 'message' | 'variant'>>;

type Listener = (toast: ToastInput) => void;

let listeners: ReadonlyArray<Listener> = [];

export const toastBus = {
  subscribe(listener: Listener): () => void {
    listeners = [...listeners, listener];
    return () => {
      listeners = listeners.filter((l) => l !== listener);
    };
  },
  emit(payload: ToastInput): void {
    // Snapshot to guard against listeners mutating the array during dispatch.
    const snapshot = listeners;
    for (const listener of snapshot) {
      try {
        listener(payload);
      } catch (err) {
        // A misbehaving listener must not break the dispatch loop.
        console.warn('[toast]', err);
      }
    }
  },
};

export const toast = {
  success: (message: string, opts?: ToastOptions): void =>
    toastBus.emit({ variant: 'success', message, ...opts }),
  error: (message: string, opts?: ToastOptions): void =>
    toastBus.emit({ variant: 'error', message, ...opts }),
  info: (message: string, opts?: ToastOptions): void =>
    toastBus.emit({ variant: 'info', message, ...opts }),
};


// ── formatError: the P05 root fix ────────────────────────────────────────────
//
// `formatError` used to return `err.message` verbatim, which is how strings
// like "Request timeout: system.config" or a raw HTML error page ended up in
// front of end users — the shared root cause behind the 21 P05 blockers in the
// 2026-08 UX audit (§0.5 point 2).
//
// It now runs the same classification `ErrorState` uses (`lib/error-message`),
// so a failure reads identically wherever it surfaces, and returns a SHORT
// plain-language clause suited to being interpolated into a toast template
// ("儲存失敗：{message}"). The long "what you can do" sentence and the raw
// technical string stay where there is room for them: `ErrorState`'s body and
// its collapsed details disclosure (`formatErrorDetail`).
//
// The catalogue is a plain JSON map, so this non-React module can resolve a
// message id directly — callers outside the React tree keep working.

import { messages, useLocaleStore } from '@/i18n';
import { classifyError, sanitizeErrorDetail } from './error-message';

/** Short clause key for a classified error (`errorState.manage.short.*`). */
export function errorShortKey(err: unknown): string {
  return `errorState.manage.short.${classifyError(err)}`;
}

/** Resolve an i18n id against the active catalogue, outside a React context. */
export function lookupMessage(id: string): string {
  let locale = 'zh-TW';
  try {
    locale = useLocaleStore.getState().locale;
  } catch {
    // Store not constructible (no DOM) — fall through to the source catalogue.
  }
  return messages[locale]?.[id] ?? messages['zh-TW']?.[id] ?? id;
}

/**
 * A server message written for humans is worth more than a category label, so
 * short CJK sentences from the gateway pass through (sanitized). Anything
 * longer, or in the shape of a stack trace, is technical.
 */
const CJK_RE = /[぀-ヿ㐀-䶿一-鿿豈-﫿]/;
const PASSTHROUGH_MAX_CHARS = 120;

function humanReadablePassthrough(err: unknown): string | null {
  const detail = sanitizeErrorDetail(err, PASSTHROUGH_MAX_CHARS);
  if (!detail) return null;
  if (!CJK_RE.test(detail)) return null;
  if (detail.endsWith('…')) return null; // truncated ⇒ it was a dump, not a sentence
  if (/\bat \w+ \(|https?:\/\/|\{|\}/.test(detail)) return null;
  return detail;
}

/** How much sanitized technical text a toast-sized string may carry. */
const TOAST_DETAIL_MAX_CHARS = 120;

/**
 * Plain-language one-liner for a thrown value.
 *
 * Shape: `<classified clause>（<sanitized detail>）`. The clause comes first so
 * the sentence is readable by someone who does not know what a WebSocket is;
 * the detail is masked, single-line and length-capped, and is what makes a
 * support conversation possible. A raw, unbounded `err.message` — the audit
 * finding — is never returned. Surfaces with room for structure (`ErrorState`)
 * should use the classification directly and keep the detail collapsed.
 */
export function formatError(err: unknown): string {
  const passthrough = humanReadablePassthrough(err);
  if (passthrough) return passthrough;
  const clause = lookupMessage(errorShortKey(err));
  const detail = sanitizeErrorDetail(err, TOAST_DETAIL_MAX_CHARS);
  if (!detail) return clause;
  return lookupMessage('errorState.manage.withDetail')
    .replace('{reason}', clause)
    .replace('{detail}', detail);
}

/** Masked, single-line, length-capped technical text. Keep it collapsed. */
export function formatErrorDetail(err: unknown): string {
  return sanitizeErrorDetail(err);
}
