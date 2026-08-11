/**
 * Small pure helpers for "state as URL" sync (W3-3, Stripe pattern B4 —
 * filters/selection reflected into `?query=…` so a view is bookmarkable and
 * shareable). Kept dependency-free and side-effect-free so every list page's
 * mirror-to-URL effect can share one tested implementation instead of
 * hand-rolling the same `set`/`delete` dance.
 */

/**
 * Return a new `URLSearchParams` with `key` set to `value`, or removed when
 * `value` is empty/null/undefined. Never mutates the input — callers pass the
 * previous params (from `setSearchParams((prev) => …)`) and get back a fresh
 * object safe to hand to React Router.
 */
export function withParam(
  params: URLSearchParams,
  key: string,
  value: string | null | undefined,
): URLSearchParams {
  const next = new URLSearchParams(params);
  if (value) next.set(key, value);
  else next.delete(key);
  return next;
}

/**
 * Parse a URL search-param value against a fixed set of allowed string
 * literals. Returns `null` for a missing or unrecognized value so callers
 * fall back to whatever default they already use — this never silently
 * coerces an unknown value into a wrong member of `T`.
 */
export function parseEnumParam<T extends string>(
  raw: string | null,
  allowed: readonly T[],
): T | null {
  if (raw == null) return null;
  return (allowed as readonly string[]).includes(raw) ? (raw as T) : null;
}
