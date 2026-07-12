/**
 * G15 Live Canvas — srcdoc builder for the sandboxed canvas iframe.
 *
 * Security contract (defense-in-depth, layer 2 of 2):
 * the gateway already sanitized the HTML at write time (ammonia allowlist —
 * no scripts, no event handlers, no iframes/objects/embeds/forms). The
 * dashboard STILL renders it only through `<iframe srcdoc sandbox="">` with
 * an empty sandbox attribute: no `allow-scripts`, no `allow-same-origin`
 * (opaque origin), no forms, no popups, no top navigation. This module only
 * builds the srcdoc string; the empty `sandbox` attribute lives on the
 * iframe element in CanvasPage.
 */

/**
 * Base styles injected by US (not the agent) so a bare-markup canvas looks
 * native to the dashboard: system font stack, Calm Glass-ish neutral colors,
 * readable tables, and `overflow-x: auto` wrappers so wide content scrolls
 * inside the frame instead of blowing out the layout.
 */
const BASE_STYLE = `
  :root { color-scheme: light dark; }
  * { box-sizing: border-box; }
  body {
    margin: 16px;
    font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
    font-size: 15px;
    line-height: 1.65;
    color: #292524;
    background: #fafaf9;
    word-break: break-word;
    overflow-wrap: anywhere;
  }
  @media (prefers-color-scheme: dark) {
    body { color: #e7e5e4; background: #1c1917; }
    table, th, td { border-color: #44403c; }
  }
  img, svg { max-width: 100%; height: auto; }
  pre { overflow-x: auto; padding: 12px; border-radius: 8px; background: rgba(120,113,108,.12); }
  table { border-collapse: collapse; max-width: 100%; display: block; overflow-x: auto; }
  th, td { border: 1px solid #d6d3d1; padding: 6px 10px; text-align: left; }
  a { color: #d97706; }
`;

/**
 * Self-contained Content-Security-Policy for the canvas document. The empty
 * `sandbox` already blocks scripts; this closes the remaining *network egress*
 * channel that sanitization can't (CSS `background: url(http://attacker/beacon)`
 * inside an allowed inline `style` attribute would otherwise phone home —
 * leaking the viewer's IP / a view-confirmation, even with no script running).
 * `default-src 'none'` denies every fetch; only inline styles and inlined
 * `data:` images/fonts are permitted — the canvas is a fully offline visual
 * surface. Because remote (`https:`) images are also blocked here, agents must
 * embed images as `data:` URIs; the server sanitizer still accepts `https:`
 * <img> for forward-compat, but this CSP is the authoritative render-time gate.
 */
const CANVAS_CSP =
  "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; form-action 'none'";

/**
 * Wrap sanitized canvas HTML in a minimal self-contained document for
 * `iframe.srcdoc`. The wrapper adds charset, a self-contained CSP (see
 * {@link CANVAS_CSP}), and our own base styles — agent content is embedded
 * as-is (it was sanitized server-side).
 */
export function canvasSrcDoc(sanitizedHtml: string): string {
  return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${CANVAS_CSP}"><style>${BASE_STYLE}</style></head><body>${sanitizedHtml}</body></html>`;
}

/**
 * The exact sandbox attribute value the canvas iframe must use. Exported as
 * a constant (and unit-tested) so nobody "temporarily" adds allow-scripts
 * without tripping a test.
 */
export const CANVAS_SANDBOX = '';
