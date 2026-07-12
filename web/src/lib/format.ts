/**
 * Machine-value formatters (dashboard-redesign §8, paperclip P8). One place
 * that turns raw ids / money / tokens / timestamps into the compact, aligned,
 * monospace-friendly strings the dashboard shows everywhere. Keeping them here
 * (instead of ad-hoc `.toFixed(2)` sprinkled across pages) makes machine values
 * read as one system.
 *
 * These are pure and locale-neutral on purpose: `timeAgo` returns compact
 * unit tokens (`now` / `5m` / `2h` / `3d`) that need no translation, so they
 * can be used inside `<Mono>` without threading `intl` through every call site.
 */

/** Format a cents integer as `$12.34`. Negative and huge values are safe. */
export function formatCents(cents: number | null | undefined): string {
  const n = typeof cents === 'number' && Number.isFinite(cents) ? cents : 0;
  return `$${(n / 100).toFixed(2)}`;
}

/**
 * Format a millicent integer as `$0.01234`. 1 cent = 1000 millicents, so
 * dollars = millicents / 100_000. Sub-cent amounts keep enough precision to
 * stay non-zero; larger amounts round to cents. Safe on null/NaN.
 */
export function formatMillicents(millicents: number | null | undefined): string {
  // NOTE: despite the `millicents` name, the backend cost telemetry stores and
  // sums these values in CENTS (see cost_telemetry.rs unit note — the field name
  // is a historical misnomer). Dollars = cents / 100, not / 100_000.
  const n = typeof millicents === 'number' && Number.isFinite(millicents) ? millicents : 0;
  const dollars = n / 100;
  const digits = Math.abs(dollars) >= 1 ? 2 : Math.abs(dollars) >= 0.01 ? 4 : 5;
  return `$${dollars.toFixed(digits)}`;
}

/** Format a token count with a `k`/`M` suffix once it gets large. */
export function formatTokens(tokens: number | null | undefined): string {
  const n = typeof tokens === 'number' && Number.isFinite(tokens) ? tokens : 0;
  if (Math.abs(n) >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (Math.abs(n) >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

/**
 * Shorten a long opaque id to `abcd…wxyz` for display. UTF-8 safe: slices by
 * codepoint, never by raw byte, so multi-byte ids never split a character
 * (mirrors the `duduclaw_core::truncate_chars` discipline on the Rust side).
 */
export function formatId(id: string | null | undefined, head = 6, tail = 4): string {
  if (!id) return '—';
  const chars = Array.from(id);
  if (chars.length <= head + tail + 1) return id;
  return `${chars.slice(0, head).join('')}…${chars.slice(-tail).join('')}`;
}

/**
 * Compact relative time from an ISO/parseable timestamp. Returns locale-neutral
 * unit tokens: `now`, `5m`, `2h`, `3d`, `6w`, or a `YYYY-MM-DD` date past ~1y.
 * Invalid input returns `—`.
 */
export function timeAgo(input: string | number | Date | null | undefined, nowMs?: number): string {
  if (input == null) return '—';
  const then = input instanceof Date ? input.getTime() : new Date(input).getTime();
  if (!Number.isFinite(then)) return '—';
  const now = typeof nowMs === 'number' ? nowMs : Date.now();
  const diffSec = Math.round((now - then) / 1000);
  if (diffSec < 45) return 'now';
  const mins = Math.round(diffSec / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.round(hours / 24);
  if (days < 7) return `${days}d`;
  const weeks = Math.round(days / 7);
  if (weeks < 52) return `${weeks}w`;
  // Older than ~1 year: show the calendar date instead of an unhelpful "60w".
  const d = new Date(then);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

/* ───────────────── Soft Play v2 gamification formatters (T0.4) ─────────────
   Same locale-neutral, Mono-friendly discipline as above: these return compact
   tokens; surrounding i18n copy supplies the label/unit sentence. */

/**
 * Format an XP score for the HUD capsule / `/growth` bar. Grouped thousands
 * below 1M (`1,234`), then a `M` suffix (`1.2M`). Negatives clamp to 0.
 */
export function formatXp(xp: number | null | undefined): string {
  const n = typeof xp === 'number' && Number.isFinite(xp) ? Math.max(0, Math.floor(xp)) : 0;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  return n.toLocaleString('en-US');
}

/**
 * Format a money amount stored as an integer number of cents (the existing
 * `formatCents` convention) into a coin/spend chip. USD keeps two decimals
 * (`$12.34`); TWD is a whole-dollar currency, so it rounds (`NT$1,234`).
 */
export function formatCoins(
  cents: number | null | undefined,
  currency: 'USD' | 'TWD' = 'USD',
): string {
  const n = typeof cents === 'number' && Number.isFinite(cents) ? cents : 0;
  if (currency === 'TWD') return `NT$${Math.round(n / 100).toLocaleString('en-US')}`;
  return `$${(n / 100).toFixed(2)}`;
}

/**
 * Humanize a saved-time magnitude given in minutes into a compact token:
 * `45m`, `2.5h`, `3d`. Whole values drop the decimal (`2h`, not `2.0h`).
 * Non-positive / invalid input returns `0m`. Locale-neutral by design — the
 * `/growth` copy wraps it with the translated "saved" phrasing.
 */
export function formatDurationSaved(minutes: number | null | undefined): string {
  const m = typeof minutes === 'number' && Number.isFinite(minutes) && minutes > 0 ? minutes : 0;
  if (m < 1) return '0m';
  if (m < 60) return `${Math.round(m)}m`;
  const hours = m / 60;
  if (hours < 24) return Number.isInteger(hours) ? `${hours}h` : `${hours.toFixed(1)}h`;
  const days = hours / 24;
  return Number.isInteger(days) ? `${days}d` : `${days.toFixed(1)}d`;
}
