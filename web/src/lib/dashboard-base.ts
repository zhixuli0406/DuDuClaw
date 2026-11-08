/**
 * RM-129 — the address the dashboard is reachable at, including the sub-path
 * when it is served behind a reverse proxy prefix (e.g.
 * `https://example.com/agents/`).
 *
 * The SPA itself has no router basename, so the only reliable signal of a
 * prefix is a same-origin `<base href>` (injected by the proxy or the page).
 * Without one, `document.baseURI` is just the current page URL and says
 * nothing about the prefix, so the path is `/`.
 *
 * The gateway validates whatever is sent here again: for OAuth only the
 * origin is checked against the redirect allowlist and the configured
 * `[dashboard] public_url` decides the path.
 */

/** `/` or `/prefix/` (always a leading and a trailing slash). */
export function dashboardBasePath(
  doc: Pick<Document, 'baseURI' | 'querySelector'> | undefined = typeof document === 'undefined' ? undefined : document,
  origin: string = typeof window === 'undefined' ? '' : window.location.origin,
): string {
  if (!doc || !doc.querySelector('base[href]')) return '/';
  let url: URL;
  try {
    url = new URL(doc.baseURI);
  } catch {
    return '/';
  }
  // A cross-origin <base> is not where this dashboard lives.
  if (url.origin !== origin) return '/';
  const path = url.pathname.replace(/\/+$/, '');
  return path === '' ? '/' : `${path}/`;
}

/**
 * Client-side pre-check for `[dashboard] public_url`, mirroring the gateway
 * rule (`deep_link::valid_oauth_public_base`): empty (unset) or an http(s)
 * URL (HTTP only for localhost, 127.0.0.0/8 or ::1) with a host and no
 * user name, query, fragment, backslash, `..` or
 * encoded separators. The gateway re-validates on save.
 */
export function isValidPublicUrl(raw: string): boolean {
  const value = raw.trim();
  if (value === '') return true;
  const control = [...value].some((c) => {
    const code = c.charCodeAt(0);
    return code < 0x20 || (code >= 0x7f && code <= 0x9f);
  });
  if (control || value.includes('\\') || !value.includes('://')) return false;
  const rest = value.slice(value.indexOf('://') + 3);
  const authorityEnd = rest.search(/[/?#]/);
  const authority = authorityEnd === -1 ? rest : rest.slice(0, authorityEnd);
  const path = authorityEnd === -1 ? '' : rest.slice(authorityEnd).toLowerCase().replace(/%2e/g, '.');
  if (authority.includes('@') || path.includes('..') || path.includes('%2f') || path.includes('%5c')) return false;
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return false;
  }
  const loopback = url.hostname === 'localhost' || url.hostname === '[::1]'
    || /^127(?:\.\d{1,3}){3}$/.test(url.hostname);
  return (url.protocol === 'https:' || (url.protocol === 'http:' && loopback)) && url.hostname !== ''
    && url.username === '' && url.password === '' && url.search === '' && url.hash === ''
    && !value.includes('?') && !value.includes('#');
}

/**
 * `window.location.origin` plus the dashboard base path. Stays the bare
 * origin when the dashboard is served at the root, so nothing changes for
 * the common deployment.
 */
export function dashboardBaseUrl(
  doc: Pick<Document, 'baseURI' | 'querySelector'> | undefined = typeof document === 'undefined' ? undefined : document,
  origin: string = typeof window === 'undefined' ? '' : window.location.origin,
): string {
  if (!origin) return '';
  const path = dashboardBasePath(doc, origin);
  return path === '/' ? origin : `${origin}${path}`;
}
