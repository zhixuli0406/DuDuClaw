/**
 * Pull the sign-in URL out of a CLI's (ANSI-stripped) login transcript so the
 * dashboard can offer it as a one-click link. Every supported CLI prints the URL
 * somewhere in a full-screen terminal UI, where most people never find it.
 *
 * Why this is not a one-line regex: a login transcript usually contains several
 * URLs, and only one of them is the page the user must open.
 *
 *   • Antigravity (`agy`) announces its callback endpoint
 *     (`https://antigravity.google/oauth-callback`) and its docs/terms links
 *     alongside the real consent URL on `accounts.google.com`. Picking the first
 *     URL that merely contains the word "oauth" hands back the callback — which
 *     is what the 2026-08-04 report ("HY 抓錯授權網址") was about.
 *   • Codex and Gemini print a plain "listening on http://localhost:PORT" line
 *     before the URL that actually carries the consent parameters.
 *
 * The reliable signal is not the host or the word "oauth" — it is the query
 * string. A consent / device-authorization URL always carries OAuth parameters;
 * callback endpoints, docs links and status lines never do. So candidates are
 * ranked, strongest evidence first, instead of taken in document order.
 *
 * Ranking alone is not enough, though: a docs link can carry a `?utm_source=`
 * tracking query and a callback echo can carry `?code=…&state=…`, both of which
 * would otherwise out-rank the real URL. Known non-destinations are therefore
 * removed BEFORE ranking, not consulted as a late tie-break.
 *
 * Depends on the gateway giving the CLI a 600-column PTY
 * (`crates/duduclaw-gateway/src/cli_auth.rs:445`) so a ~400-character authorize
 * URL stays on one line. Narrow that and the URL wraps mid-string, no regex
 * here can reassemble it, and this function starts returning fragments.
 */

/** OAuth parameters that only ever appear on a page the user must open. */
const OAUTH_PARAM = /[?&](client_id|response_type|redirect_uri|code_challenge|user_code|device_code|scope|state)=/i;

/**
 * URLs a CLI mentions but nobody should be sent to: callback endpoints, the
 * post-login landing page, and reference links. Applied to every candidate
 * before ranking — these carry query strings often enough (`?utm_source=` on a
 * docs link, `?code=&state=` on a callback echo) that letting them into the
 * ranking hands back exactly the wrong URL.
 *
 * The trailing `(?![\w-])` matches a path segment that ends at `/`, `?`, `#` or
 * the end of the string, so `…/legal/terms?hl=zh-tw` is caught the same as
 * `…/terms`. Note `oauth-?callback` is deliberately narrower than plain
 * "callback": codex's real destination is `http://localhost:PORT/auth/callback`.
 */
const NOT_A_DESTINATION =
  /(oauth-?callback|auth-?success|\/(docs?|terms|privacy|support|changelog|help)(?![\w-]))/i;

/** Legacy shape match, kept as a fallback for transcripts with no query strings. */
const LOOKS_LIKE_AUTH = /oauth|authorize|auth\.|\/cai\//i;

/** Trailing characters a terminal UI may append to a URL (box borders, prose). */
const TRAILING_JUNK = /[.,;:)\]}>'"|]+$/;

function tidy(url: string): string {
  return url.replace(TRAILING_JUNK, '');
}

/**
 * Extract the URL the user should open, or `null` when the transcript has none
 * yet. Input must already have ANSI escapes stripped.
 */
export function extractAuthUrl(clean: string): string | null {
  const raw = clean.match(/https?:\/\/[^\s"'<>)\]]+/g);
  if (!raw) return null;
  const urls = raw.map(tidy).filter((u) => u.length > 0);
  if (urls.length === 0) return null;

  // Drop known non-destinations up front, so no later rule can promote one.
  // If that leaves nothing, fall back to the full set rather than returning
  // null — the caller only shows the link while a login is actually running.
  const plausible = urls.filter((u) => !NOT_A_DESTINATION.test(u));
  const pool = plausible.length > 0 ? plausible : urls;

  const withQuery = pool.filter((u) => u.includes('?'));

  // 1. A URL carrying OAuth parameters. Take the LAST one: a TUI redraws its
  //    screen, and after a retry the newest URL is the live one.
  const oauthParams = withQuery.filter((u) => OAUTH_PARAM.test(u));
  if (oauthParams.length > 0) return oauthParams[oauthParams.length - 1];

  // 2. Any other parameterised URL — still far more likely to be the
  //    destination than a bare link sitting in the surrounding prose.
  if (withQuery.length > 0) return withQuery[withQuery.length - 1];

  // 3. No query strings at all. Fall back to shape matching.
  const shaped = pool.find((u) => LOOKS_LIKE_AUTH.test(u));
  if (shaped) return shaped;

  // 4. Last resort: the longest remaining URL.
  return pool.reduce((a, b) => (b.length > a.length ? b : a));
}
