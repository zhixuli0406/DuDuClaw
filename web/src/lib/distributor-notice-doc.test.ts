import { describe, expect, it } from 'vitest';
import {
  DISTRIBUTOR_NOTICE_MAX_HEIGHT_PX,
  DISTRIBUTOR_NOTICE_MIN_HEIGHT_PX,
  DISTRIBUTOR_NOTICE_SANDBOX,
  clampNoticeHeight,
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

  // Security tripwire: exactly these three tokens; never scripts.
  it('sandbox tokens are exactly same-origin + popups, never scripts', () => {
    const tokens = DISTRIBUTOR_NOTICE_SANDBOX.split(/\s+/).sort();
    expect(tokens).toEqual(['allow-popups', 'allow-popups-to-escape-sandbox', 'allow-same-origin']);
    for (const forbidden of ['allow-scripts', 'allow-top-navigation', 'allow-forms', 'allow-modals']) {
      expect(DISTRIBUTOR_NOTICE_SANDBOX).not.toContain(forbidden);
    }
  });

  it('clamps the measured height to 64–600 px', () => {
    expect(DISTRIBUTOR_NOTICE_MIN_HEIGHT_PX).toBe(64);
    expect(DISTRIBUTOR_NOTICE_MAX_HEIGHT_PX).toBe(600);
    expect(clampNoticeHeight(10)).toBe(64);
    expect(clampNoticeHeight(0)).toBe(64);
    expect(clampNoticeHeight(Number.NaN)).toBe(64);
    expect(clampNoticeHeight(180.2)).toBe(181);
    expect(clampNoticeHeight(600)).toBe(600);
    expect(clampNoticeHeight(5000)).toBe(600);
  });
});
