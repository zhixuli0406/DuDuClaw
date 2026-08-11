/**
 * B5 — server-initiated dashboard navigation (Wave 3, HA `command_webview`
 * pattern; gateway twin: `crates/duduclaw-gateway/src/dashboard_navigate.rs`).
 *
 * The gateway can push a `dashboard.navigate` WS event when it wants an open
 * dashboard tab to jump to a specific page — first use case: an approval is
 * about to auto-deny (⅔ of its TTL elapsed), so an admin already staring at
 * the dashboard gets routed to the inbox row instead of only getting a
 * channel ping they might not notice. Subscribed once from `App.tsx` so
 * every page benefits without per-page wiring.
 *
 * Two guards keep a server-pushed navigation from being obnoxious:
 *
 * - **Cooldown** — a repeat event within {@link NAVIGATE_COOLDOWN_MS} of the
 *   last one this tab actually acted on (navigated OR toasted) is dropped
 *   silently. This is a leading-edge "ignore repeats" cooldown, not
 *   `debounceTrailing` (`lib/debounce.ts`) — that utility *delays* execution
 *   until a burst goes quiet, which is wrong here: the first event in a
 *   cluster should navigate immediately, and only the follow-on repeats
 *   should be dropped.
 * - **Mid-edit guard** — if the user is actively typing into a non-empty
 *   form field right now, a forced route change would discard their
 *   attention (and, for an uncontrolled input, risks losing keystrokes).
 *   Instead of navigating, a clickable toast offers to go there.
 */

export const NAVIGATE_COOLDOWN_MS = 5000;

// `-Infinity`, not `0`: tests exercise the cooldown with small, readable
// `now` values (e.g. `1000`), and `0` would make the very first call after a
// reset look like it's still inside a 5s cooldown relative to the epoch.
let lastHandledAt = -Infinity;

/** Reset the module-level cooldown clock. Test-only. */
export function __resetDashboardNavigateCooldownForTest(): void {
  lastHandledAt = -Infinity;
}

/**
 * Best-effort heuristic for "the user is mid-edit in a form right now": the
 * focused element is a text input / textarea / contenteditable AND it
 * already holds content. Mere focus is deliberately not enough — an empty
 * field the user just tabbed into (e.g. a search box) is not "unsaved work",
 * and treating it as such would block navigation far more often than the
 * guard is meant to.
 */
export function hasUnsavedFormInput(doc: Document = document): boolean {
  const el = doc.activeElement;
  if (!el) return false;
  const tag = el.tagName;
  if (tag === 'INPUT' || tag === 'TEXTAREA') {
    const value = (el as HTMLInputElement | HTMLTextAreaElement).value;
    return typeof value === 'string' && value.trim().length > 0;
  }
  // `isContentEditable` is the spec-correct check, but jsdom (our test
  // environment) never implements it — it leaves the property `undefined`
  // rather than computing it from the attribute. Falling back to the raw
  // attribute keeps this correct in real browsers (every contenteditable
  // element carries it too) while making the branch actually testable.
  const isEditable =
    (el as HTMLElement).isContentEditable || el.getAttribute('contenteditable') === 'true';
  if (isEditable) {
    return (el.textContent ?? '').trim().length > 0;
  }
  return false;
}

/**
 * Validate + narrow a `dashboard.navigate` WS payload to a same-origin
 * relative path. Mirrors the gateway's `is_safe_relative_path` — this is
 * defense in depth, not the only check (the gateway already refuses to emit
 * anything else), so a compromised or buggy future emitter still cannot
 * steer this tab off-origin.
 */
export function parseNavigatePath(payload: unknown): string | null {
  if (typeof payload !== 'object' || payload === null) return null;
  const raw = (payload as { path?: unknown }).path;
  if (typeof raw !== 'string') return null;
  const path = raw.trim();
  if (!path || path.length > 512) return null;
  if (!path.startsWith('/') || path.startsWith('//')) return null;
  // eslint-disable-next-line no-control-regex
  if (/[\x00-\x1f]/.test(path)) return null;
  return path;
}

export interface ToastActionFn {
  (message: string, action: { label: string; onClick: () => void }): void;
}

/**
 * Handle one `dashboard.navigate` event.
 *
 * @param payload  The raw WS event payload.
 * @param navigate `react-router`'s `useNavigate()` result (or any `(path) => void`).
 * @param toastInfo A function that shows an info toast with a clickable action
 *   (matches `toast.info(message, { action })`).
 * @param now Injectable clock for tests.
 */
export function handleDashboardNavigate(
  payload: unknown,
  navigate: (path: string) => void,
  toastInfo: ToastActionFn,
  now: number = Date.now(),
): void {
  const path = parseNavigatePath(payload);
  if (!path) return;
  if (now - lastHandledAt < NAVIGATE_COOLDOWN_MS) return;
  lastHandledAt = now;

  if (hasUnsavedFormInput()) {
    toastInfo('系統想帶你前往新的畫面，但目前表單還有未儲存的編輯內容，先幫你保留。', {
      label: '前往查看',
      onClick: () => navigate(path),
    });
    return;
  }
  navigate(path);
}
