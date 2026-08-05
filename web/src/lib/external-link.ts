/**
 * Desktop-shell-aware "open in the user's real browser".
 *
 * Inside the Tauri desktop shell, `window.open` / `<a target="_blank">` are
 * silent no-ops — the wry webview has no new-window handler — which dead-ended
 * every OAuth consent flow on desktop. There the URL goes through the shell's
 * `open_external_url` command (http/https only, validated Rust-side) to the
 * system default browser; in a plain browser tab it stays `window.open`.
 * The app runs with `withGlobalTauri: true`, so the runtime is reached through
 * `window.__TAURI__` rather than an npm package (same pattern as lib/pet.ts).
 */

type TauriInvoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

function tauriInvoke(): TauriInvoke | undefined {
  const w = window as unknown as { __TAURI__?: { core?: { invoke?: TauriInvoke } } };
  const fn = w.__TAURI__?.core?.invoke;
  return typeof fn === 'function' ? fn : undefined;
}

/** Open `url` in the system browser (desktop shell) or a new tab (browser). */
export function openExternal(url: string): void {
  const invoke = tauriInvoke();
  if (invoke) {
    // Older shells without the command reject; window.open is a no-op there,
    // but callers surface the URL as select-all text, so the flow stays alive.
    void invoke('open_external_url', { url }).catch(() => {
      window.open(url, '_blank', 'noopener');
    });
    return;
  }
  window.open(url, '_blank', 'noopener');
}

/**
 * Route every `<a target="_blank" href="http(s)…">` click through
 * {@link openExternal}. Installed once at boot; does nothing outside the
 * desktop shell, where anchors already work natively. This covers the long
 * tail of outbound links (pricing, docs, file downloads) without touching
 * each component.
 */
export function installExternalLinkInterceptor(): void {
  if (!tauriInvoke()) return;
  document.addEventListener('click', (e) => {
    if (e.defaultPrevented) return;
    const anchor = (e.target as Element | null)?.closest?.('a[target="_blank"]');
    const href = anchor instanceof HTMLAnchorElement ? anchor.href : '';
    if (!/^https?:/i.test(href)) return;
    e.preventDefault();
    openExternal(href);
  });
}
