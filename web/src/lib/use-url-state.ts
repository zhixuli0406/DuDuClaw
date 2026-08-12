/**
 * `useUrlState` — the one "page state lives in the URL" hook (P11, W3-3).
 *
 * Audit finding P11 ("頁面狀態不可深連結") was the largest single root cause in
 * the 2026-08 UX audit: ~20 pages kept their tab / filter / selected-item state
 * in `useState` only, so a refresh or a shared link dropped the user back to
 * defaults. Every page hand-rolling its own `useSearchParams` dance is how the
 * half-done versions happened (read once at mount, never write back). This hook
 * is that dance, once:
 *
 *   - reads through `useSearchParams`, so the URL is the single source of truth
 *     (no shadow `useState` that can drift out of sync),
 *   - writes with `{ replace: true }` by default, so dragging a filter around
 *     doesn't bury the previous page under 30 history entries,
 *   - drops the param entirely when the value equals the default, keeping URLs
 *     short and making "no param" and "default" the same state,
 *   - falls back to the default for an unknown value when `allowed` is given,
 *     so a hand-edited `?tab=nonsense` can never push a page into a state it
 *     cannot render.
 *
 * Built on the pure helpers in `url-params.ts` (`withParam` / `parseEnumParam`)
 * so the merge/delete semantics stay tested in isolation.
 */
import { useCallback } from 'react';
import { useSearchParams } from 'react-router';
import { parseEnumParam, withParam } from './url-params';

export interface UrlStateOptions<T extends string> {
  /**
   * Closed set of legal values. A param outside the set (stale link, typo,
   * hand-edited URL) resolves to `defaultValue` instead of being handed to the
   * page as a bogus `T`.
   */
  allowed?: readonly T[];
  /**
   * Push a history entry instead of replacing. Default `false` (replace) —
   * filters and tabs are view state, not navigation steps, and pushing them
   * makes Back feel broken.
   */
  push?: boolean;
}

/**
 * Same-tick coalescing.
 *
 * React Router's `setSearchParams(fn)` calls `fn` with the params captured at
 * render time, not a live ref. Two different `useUrlState` instances updated in
 * the same event handler (e.g. "change the filter *and* reset the page") would
 * therefore both build on the same base and the second `navigate()` would win,
 * silently discarding the first update. The staging buffer below lets the
 * second call build on the first's result while they share the same base query
 * string, and is dropped on the next microtask so it can never leak into a
 * later interaction (or a later test).
 */
let staged: { base: string; next: URLSearchParams } | null = null;
let stagedScheduled = false;

function stageParams(
  current: URLSearchParams,
  mutate: (params: URLSearchParams) => URLSearchParams,
): URLSearchParams {
  const base = current.toString();
  const start = staged && staged.base === base ? staged.next : current;
  const next = mutate(start);
  staged = { base, next };
  if (!stagedScheduled) {
    stagedScheduled = true;
    queueMicrotask(() => {
      staged = null;
      stagedScheduled = false;
    });
  }
  return next;
}

/** Test-only escape hatch: forget any staged same-tick params. */
export function __resetUrlStateStaging(): void {
  staged = null;
  stagedScheduled = false;
}

/**
 * Read/write one string (or string-union) param, defaulting to `defaultValue`
 * when absent.
 *
 * ```ts
 * const [tab, setTab] = useUrlState('tab', 'overview', { allowed: TABS });
 * const [query, setQuery] = useUrlState('q', '');
 * ```
 */
export function useUrlState<T extends string = string>(
  key: string,
  // `NoInfer` keeps a plain `useUrlState('q', '')` typed as `string` instead of
  // collapsing `T` to the literal `''`; with `allowed` present, `T` is inferred
  // from the allowed set (the union the page actually renders).
  defaultValue: NoInfer<T>,
  options?: UrlStateOptions<T>,
): [T, (next: T) => void] {
  const [params, setParams] = useSearchParams();
  const allowed = options?.allowed;
  const replace = !options?.push;

  const raw = params.get(key);
  // An empty param (`?agent=`) reads as "absent": `withParam` never writes one,
  // so treating it as a legal value would only ever come from a mangled URL.
  const value: T =
    raw == null || raw === ''
      ? defaultValue
      : allowed
        ? (parseEnumParam(raw, allowed) ?? defaultValue)
        : (raw as T);

  const set = useCallback(
    (next: T) => {
      setParams(
        (prev) => stageParams(prev, (p) => withParam(p, key, next === defaultValue ? null : next)),
        { replace },
      );
    },
    [key, defaultValue, replace, setParams],
  );

  return [value, set];
}

/**
 * Read/write an optional param whose "unset" state is `null` — the shape most
 * selected-item state already uses (`const [selectedId, setSelectedId] =
 * useState<string | null>(null)`).
 *
 * Only ever store an **id** here, never a serialized object: URLs are shared,
 * logged, and length-limited.
 */
export function useUrlStateNullable(
  key: string,
  options?: { push?: boolean },
): [string | null, (next: string | null) => void] {
  const [params, setParams] = useSearchParams();
  const replace = !options?.push;

  const raw = params.get(key);
  const value = raw != null && raw !== '' ? raw : null;

  const set = useCallback(
    (next: string | null) => {
      setParams((prev) => stageParams(prev, (p) => withParam(p, key, next)), { replace });
    },
    [key, replace, setParams],
  );

  return [value, set];
}

export interface UrlNumberStateOptions {
  /** Values below this (or non-numeric) fall back to the default. */
  min?: number;
  /** Values above this fall back to the default. */
  max?: number;
  push?: boolean;
}

/**
 * Read/write a whole-number param (pagination, window sizes). Anything that
 * isn't a finite integer inside `[min, max]` resolves to `defaultValue`, so a
 * hand-edited `?page=abc` can't produce `NaN` slicing.
 */
export function useUrlNumberState(
  key: string,
  defaultValue: number,
  options?: UrlNumberStateOptions,
): [number, (next: number) => void] {
  const [params, setParams] = useSearchParams();
  const replace = !options?.push;
  const min = options?.min;
  const max = options?.max;

  const raw = params.get(key);
  const parsed = raw != null && /^-?\d+$/.test(raw.trim()) ? Number(raw.trim()) : NaN;
  const value =
    Number.isFinite(parsed) &&
    (min == null || parsed >= min) &&
    (max == null || parsed <= max)
      ? parsed
      : defaultValue;

  const set = useCallback(
    (next: number) => {
      setParams(
        (prev) =>
          stageParams(prev, (p) =>
            withParam(p, key, next === defaultValue ? null : String(next)),
          ),
        { replace },
      );
    },
    [key, defaultValue, replace, setParams],
  );

  return [value, set];
}
