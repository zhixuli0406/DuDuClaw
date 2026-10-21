import { describe, expect, it } from 'vitest';
import {
  DISTRIBUTOR_NOTICE_HEIGHT_PX,
  DISTRIBUTOR_NOTICE_SANDBOX,
  distributorNoticeSrcDoc,
} from './distributor-notice-doc';

describe('distributorNoticeSrcDoc', () => {
  it('embeds the sanitized notice in a self-contained document', () => {
    const doc = distributorNoticeSrcDoc('<h2>總經銷</h2><p>服務專線</p>', 'light');
    expect(doc.startsWith('<!doctype html>')).toBe(true);
    expect(doc).toContain('<meta charset="utf-8">');
    expect(doc).toContain('<body><h2>總經銷</h2><p>服務專線</p></body>');
  });

  it('adds no script of its own', () => {
    expect(distributorNoticeSrcDoc('<p>x</p>', 'dark')).not.toContain('<script');
  });

  it('denies all network fetches; images only as data URIs', () => {
    const doc = distributorNoticeSrcDoc('<p>x</p>', 'light');
    expect(doc).toContain("default-src 'none'");
    expect(doc).toContain('img-src data:;');
    expect(doc).not.toContain('img-src https:');
    expect(doc).toContain("form-action 'none'");
  });

  it('follows the dashboard theme', () => {
    expect(distributorNoticeSrcDoc('<p>x</p>', 'light')).toContain('color-scheme: light');
    expect(distributorNoticeSrcDoc('<p>x</p>', 'dark')).toContain('color-scheme: dark');
    expect(distributorNoticeSrcDoc('<p>x</p>', 'dark')).not.toEqual(
      distributorNoticeSrcDoc('<p>x</p>', 'light'),
    );
  });

  // Security tripwire: popups only, never scripts or same-origin.
  it('sandbox value is exactly popups-only', () => {
    expect(DISTRIBUTOR_NOTICE_SANDBOX).toBe('allow-popups allow-popups-to-escape-sandbox');
    expect(DISTRIBUTOR_NOTICE_SANDBOX).not.toContain('allow-scripts');
    expect(DISTRIBUTOR_NOTICE_SANDBOX).not.toContain('allow-same-origin');
    expect(DISTRIBUTOR_NOTICE_SANDBOX).not.toContain('allow-top-navigation');
    expect(DISTRIBUTOR_NOTICE_SANDBOX).not.toContain('allow-forms');
  });

  it('uses a fixed, bounded frame height', () => {
    expect(DISTRIBUTOR_NOTICE_HEIGHT_PX).toBeGreaterThanOrEqual(160);
    expect(DISTRIBUTOR_NOTICE_HEIGHT_PX).toBeLessThanOrEqual(480);
  });
});
