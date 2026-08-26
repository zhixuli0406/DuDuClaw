import { describe, expect, it } from 'vitest';
import { formatError, formatErrorDetail, formatDeviceOpDetail, errorShortKey, lookupMessage } from './toast';

/**
 * `formatError` is the string that reaches every toast in the dashboard. The
 * 2026-08 UX audit traced 21 P05 blockers back to it returning `err.message`
 * verbatim, so these tests pin the two properties that fix depends on:
 * a plain-language clause always leads, and nothing credential-shaped or
 * unbounded ever gets through.
 */
describe('formatError', () => {
  // Resolved through the same catalogue lookup the code uses, so the test is
  // locale-agnostic (jsdom reports en-US; a developer's browser may not).
  const clause = (kind: string) => lookupMessage(`errorState.manage.short.${kind}`);

  it('leads with a classified plain-language clause, not the raw message', () => {
    const out = formatError(new Error('Request timeout: system.config'));
    expect(out.startsWith(clause('timeout'))).toBe(true);
  });

  it('classifies the failure kinds the gateway actually throws', () => {
    expect(errorShortKey(new Error('Not connected'))).toBe('errorState.manage.short.network');
    expect(errorShortKey(new Error('HTTP 403 forbidden'))).toBe('errorState.manage.short.auth');
    expect(errorShortKey(new Error('HTTP 500'))).toBe('errorState.manage.short.server');
    expect(errorShortKey(new Error('agent not found'))).toBe('errorState.manage.short.notFound');
  });

  it('keeps a short technical detail so support questions stay answerable', () => {
    const out = formatError(new Error('Agent not found: agnes'));
    expect(out).toContain('Agent not found: agnes');
    expect(out).toContain(clause('notFound'));
  });

  it('passes a human-written zh-TW server message through unchanged', () => {
    const out = formatError(new Error('目前沒有可用的帳號，請先新增一個'));
    expect(out).toBe('目前沒有可用的帳號，請先新增一個');
  });

  it('never leaks a credential from an error message', () => {
    const out = formatError(new Error('auth failed for api_key=sk-live-ABCDEF1234567890abcdef'));
    expect(out).not.toContain('sk-live-ABCDEF1234567890abcdef');
    expect(out).toContain('***');
  });

  it('bounds the length of a runaway error body', () => {
    const out = formatError(new Error('x'.repeat(5000)));
    expect(Array.from(out).length).toBeLessThan(200);
  });

  it('handles non-Error throwables without stringifying [object Object]', () => {
    expect(formatError({ error: 'boom' })).toContain('boom');
    expect(formatError('plain string')).toContain('plain string');
    expect(formatError(null)).toBe(clause('unknown'));
  });
});

describe('formatErrorDetail', () => {
  it('is single-line, masked and capped — safe for a details disclosure', () => {
    const detail = formatErrorDetail(
      new Error(`connect failed\n  at Foo (bar.ts:1)\ntoken=${'a1b2c3d4'.repeat(6)}`),
    );
    expect(detail).not.toContain('\n');
    expect(detail).not.toContain('a1b2c3d4a1b2c3d4');
    expect(Array.from(detail).length).toBeLessThanOrEqual(241);
  });

  it('returns an empty string when there is nothing to show', () => {
    expect(formatErrorDetail(undefined)).toBe('');
  });
});

/**
 * `DESIGN-maintenance-mode-2026-08.md` §2.6: `formatDeviceOpDetail`'s
 * `maintenanceModeActive` parameter is the ENTIRE client-side wiring of the
 * "顯示詳情" full-disclosure bypass — every existing call site keeps its
 * prior behavior when it's omitted, and every field this test pins is what
 * the design's §6 threat-4 fail-closed requirement depends on: redaction
 * never turns off, only the classify/truncate/first-line-only layers do.
 */
describe('formatDeviceOpDetail — maintenance mode bypass', () => {
  it('defaults to the classified single-line summary (unchanged prior behavior)', () => {
    const multiline = { success: true, stdout: 'line one\nline two\nline three', stderr: '' };
    const out = formatDeviceOpDetail(multiline);
    expect(out).not.toContain('\n');
    expect(out).toBe('line one');
  });

  it('with maintenanceModeActive=true, returns the full multi-line text instead of just the first line', () => {
    const multiline = { success: true, stdout: 'line one\nline two\nline three', stderr: '' };
    const out = formatDeviceOpDetail(multiline, true);
    expect(out).toContain('line one');
    expect(out).toContain('line two');
    expect(out).toContain('line three');
  });

  it('the bypass never turns off secret redaction', () => {
    const withToken = { success: true, stdout: `token=${'a1b2c3d4'.repeat(6)}`, stderr: '' };
    const out = formatDeviceOpDetail(withToken, true);
    expect(out).not.toContain('a1b2c3d4a1b2c3d4');
    expect(out).toContain('***');
  });

  it('the bypass ignores success/failure — no classification clause is prepended', () => {
    const failed = { success: false, stdout: '', stderr: 'permission denied\nsecond line' };
    const out = formatDeviceOpDetail(failed, true);
    expect(out).toBe('permission denied\nsecond line');
    // Contrast: without the bypass, a failure gets formatError's classified
    // clause prepended, so the raw text alone is no longer the whole output.
    const classified = formatDeviceOpDetail(failed, false);
    expect(classified).not.toBe('permission denied\nsecond line');
  });

  it('returns empty string when there is nothing to show, bypass or not', () => {
    expect(formatDeviceOpDetail({ success: true, stdout: '', stderr: '' }, true)).toBe('');
    expect(formatDeviceOpDetail({ success: true, stdout: '', stderr: '' }, false)).toBe('');
  });
});
