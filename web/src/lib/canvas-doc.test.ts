import { describe, expect, it } from 'vitest';
import { canvasSrcDoc, CANVAS_SANDBOX } from './canvas-doc';

describe('canvasSrcDoc', () => {
  it('embeds the sanitized html inside a self-contained document', () => {
    const doc = canvasSrcDoc('<h1>週報</h1><p>營收 100</p>');
    expect(doc.startsWith('<!doctype html>')).toBe(true);
    expect(doc).toContain('<meta charset="utf-8">');
    expect(doc).toContain('<h1>週報</h1><p>營收 100</p>');
    expect(doc).toContain('</body></html>');
  });

  it('adds no script tags of its own', () => {
    const doc = canvasSrcDoc('<p>hi</p>');
    expect(doc).not.toContain('<script');
  });

  it('keeps CJK content byte-identical', () => {
    const cjk = '<p>本週任務完成率 87%，繁體中文與 emoji 🐾 原樣保留。</p>';
    expect(canvasSrcDoc(cjk)).toContain(cjk);
  });

  // Security regression tripwire: the canvas iframe must stay FULLY sandboxed
  // (opaque origin, no script execution). If someone needs a capability, that
  // is a design change requiring review — not a quick attribute tweak.
  it('sandbox attribute stays empty (no allow-scripts / allow-same-origin)', () => {
    expect(CANVAS_SANDBOX).toBe('');
  });

  // Egress tripwire: a self-contained CSP must deny all network fetches so a
  // CSS `url()` in an allowed inline style can't beacon out (defense in depth
  // on top of the sanitizer + sandbox).
  it('embeds a self-contained CSP that blocks network egress', () => {
    const doc = canvasSrcDoc('<p>hi</p>');
    expect(doc).toContain('http-equiv="Content-Security-Policy"');
    expect(doc).toContain("default-src 'none'");
    expect(doc).toContain('img-src data:');
    // No remote image source that would let CSS url() phone home.
    expect(doc).not.toContain('img-src https:');
    expect(doc).toContain("form-action 'none'");
  });
});
