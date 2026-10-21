/**
 * About page — srcdoc builder for the distributor-information iframe.
 *
 * Security contract (same layering as the Live Canvas in canvas-doc.ts):
 * 1. The notice is a build input of the distributor build; the gateway
 *    sanitized it with an ammonia allowlist at build time and again before
 *    serving it from `about.get` (no scripts, event handlers, styles, forms,
 *    frames; links http(s)/mailto only; images PNG/JPEG/WebP data URIs only).
 * 2. The dashboard never injects it into its own DOM. It renders only inside
 *    `<iframe srcdoc sandbox="allow-popups allow-popups-to-escape-sandbox">`:
 *    no allow-scripts, no allow-same-origin (opaque origin), no forms, no top
 *    navigation. Popups are allowed so the distributor's links (forced to
 *    `target="_blank"`) open in a normal new tab.
 * 3. A self-contained CSP inside the document denies every network fetch.
 *
 * Height: a script-less, opaque-origin frame cannot report its content height
 * and the parent cannot measure it, so the frame uses a fixed height and the
 * document scrolls internally when the notice is longer.
 */

export type NoticeMode = 'light' | 'dark';

/** Exact sandbox value. Unit-tested so nobody adds allow-scripts quietly. */
export const DISTRIBUTOR_NOTICE_SANDBOX = 'allow-popups allow-popups-to-escape-sandbox';

/** Fixed frame height in CSS pixels (content scrolls inside beyond this). */
export const DISTRIBUTOR_NOTICE_HEIGHT_PX = 280;

const NOTICE_CSP =
  "default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'";

/**
 * Surface/ink tokens copied from the dashboard card tokens (`index.css`
 * `--surface`, `--foreground`, `--muted-foreground`, `--border`) so the frame
 * blends into the card; links use the brand ink (light) and the readable
 * amber text colour (dark) from web/DESIGN.md.
 */
const TOKENS: Record<NoticeMode, { bg: string; fg: string; muted: string; border: string; link: string; code: string }> = {
  light: {
    bg: 'oklch(1 0 0)',
    fg: 'oklch(0.141 0.005 285.823)',
    muted: 'oklch(0.552 0.016 285.938)',
    border: 'oklch(0.92 0.004 286.32)',
    link: '#102b48',
    code: 'oklch(0.967 0.001 286.375)',
  },
  dark: {
    bg: 'oklch(0.21 0.006 285.885)',
    fg: 'oklch(0.985 0 0)',
    muted: 'oklch(0.705 0.015 286.067)',
    border: 'oklch(1 0 0 / 12%)',
    link: '#e0a54f',
    code: 'oklch(1 0 0 / 6%)',
  },
};

function baseStyle(mode: NoticeMode): string {
  const t = TOKENS[mode];
  return `
  :root { color-scheme: ${mode}; }
  * { box-sizing: border-box; }
  html, body { margin: 0; background: ${t.bg}; }
  body {
    padding: 4px 2px 12px;
    font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", "PingFang TC", "Noto Sans TC", sans-serif;
    font-size: 14px;
    line-height: 1.65;
    color: ${t.fg};
    word-break: break-word;
    overflow-wrap: anywhere;
  }
  h1, h2, h3, h4 { margin: 0.6em 0 0.3em; line-height: 1.3; }
  h1 { font-size: 1.25rem; } h2 { font-size: 1.125rem; } h3, h4 { font-size: 1rem; }
  p, ul, ol, blockquote, pre, table { margin: 0 0 0.75em; }
  img { max-width: 100%; height: auto; }
  a { color: ${t.link}; }
  hr { border: 0; border-top: 1px solid ${t.border}; }
  blockquote { margin-left: 0; padding-left: 12px; border-left: 3px solid ${t.border}; color: ${t.muted}; }
  pre, code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.875em; }
  pre { overflow-x: auto; padding: 10px; border-radius: 8px; background: ${t.code}; }
  table { border-collapse: collapse; max-width: 100%; display: block; overflow-x: auto; }
  th, td { border: 1px solid ${t.border}; padding: 6px 10px; text-align: left; }
`;
}

/**
 * Wrap the sanitized notice in a minimal self-contained document for
 * `iframe.srcdoc`: charset, CSP, and our own theme-aware base styles.
 */
export function distributorNoticeSrcDoc(sanitizedHtml: string, mode: NoticeMode): string {
  return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${NOTICE_CSP}"><style>${baseStyle(mode)}</style></head><body>${sanitizedHtml}</body></html>`;
}
