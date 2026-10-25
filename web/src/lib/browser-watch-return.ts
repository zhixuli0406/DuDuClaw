/** Login navigation only; the watch token still needs server verification
 * after normal authentication. No other return path is accepted here. */
export function browserWatchReturn(raw: unknown): string | undefined {
  if (typeof raw !== 'string' || raw.length > 1400 || /[\x00-\x20\\#]/.test(raw)
    || !/^\/computer\/[A-Za-z0-9_-]{1,128}(?:\?.*)?$/.test(raw)) return;
  const url = new URL(raw, 'https://dashboard.invalid');
  if (url.origin !== 'https://dashboard.invalid') return;
  const keys = [...url.searchParams.keys()];
  if (keys.length === 0) return url.pathname;
  const token = url.searchParams.get('l');
  if (keys.length !== 1 || keys[0] !== 'l' || !token || token.length > 1024
    || !/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(token)) return;
  return `${url.pathname}?${new URLSearchParams({ l: token })}`;
}
