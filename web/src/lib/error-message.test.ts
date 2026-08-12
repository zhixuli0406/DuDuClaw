import { describe, it, expect } from 'vitest';
import {
  classifyError,
  errorReasonKey,
  sanitizeErrorDetail,
} from './error-message';

describe('classifyError', () => {
  it('classifies transport failures as network', () => {
    expect(classifyError(new Error('Failed to fetch'))).toBe('network');
    expect(classifyError('NetworkError when attempting to fetch')).toBe(
      'network'
    );
    expect(classifyError(new Error('ECONNREFUSED 127.0.0.1:8080'))).toBe(
      'network'
    );
    expect(classifyError('WebSocket is not connected')).toBe('network');
  });

  it('classifies timeouts', () => {
    expect(classifyError(new Error('request timed out'))).toBe('timeout');
    expect(classifyError({ status: 504 })).toBe('timeout');
  });

  it('classifies auth failures from status and from text', () => {
    expect(classifyError({ status: 401, message: 'nope' })).toBe('auth');
    expect(classifyError({ status: 403 })).toBe('auth');
    expect(classifyError(new Error('Unauthorized'))).toBe('auth');
    expect(classifyError(new Error('permission denied'))).toBe('auth');
  });

  it('classifies not-found and server errors', () => {
    expect(classifyError({ status: 404 })).toBe('notFound');
    expect(classifyError(new Error('agent not found'))).toBe('notFound');
    expect(classifyError({ status: 500 })).toBe('server');
    expect(classifyError(new Error('HTTP 502 Bad Gateway'))).toBe('server');
  });

  it('falls back to unknown rather than guessing', () => {
    expect(classifyError(new Error('rule 7 rejected the candidate'))).toBe(
      'unknown'
    );
    expect(classifyError(undefined)).toBe('unknown');
    expect(classifyError(null)).toBe('unknown');
    expect(classifyError({})).toBe('unknown');
  });

  it('does not mistake an id or a byte count for an HTTP status', () => {
    expect(classifyError(new Error('agent 404abc could not settle'))).toBe(
      'unknown'
    );
    expect(classifyError(new Error('wrote 500 rows'))).toBe('unknown');
  });

  it('maps every kind onto an errorState.reason key', () => {
    expect(errorReasonKey('network')).toBe('errorState.reason.network');
    expect(errorReasonKey('unknown')).toBe('errorState.reason.unknown');
  });
});

describe('sanitizeErrorDetail', () => {
  it('returns an empty string when there is nothing to show', () => {
    expect(sanitizeErrorDetail(undefined)).toBe('');
    expect(sanitizeErrorDetail(null)).toBe('');
    expect(sanitizeErrorDetail('   ')).toBe('');
  });

  it('keeps only the first non-empty line of a stack trace', () => {
    const err = new Error('boom');
    err.message = 'boom\n    at Foo (bar.ts:1:1)\n    at Baz (qux.ts:2:2)';
    expect(sanitizeErrorDetail(err)).toBe('boom');
  });

  it('strips tags out of an HTML error page', () => {
    const html = '<!doctype html><html><body><h1>502 Bad Gateway</h1></body></html>';
    expect(sanitizeErrorDetail(html)).toBe('502 Bad Gateway');
  });

  it('redacts credential-shaped key/value pairs', () => {
    expect(sanitizeErrorDetail('login failed: password=hunter2 for admin')).toBe(
      'login failed: password=*** for admin'
    );
    expect(
      sanitizeErrorDetail('{"api_key":"abcd1234efgh","db":"main"}')
    ).toContain('"api_key":"***"');
    expect(
      sanitizeErrorDetail('{"api_key":"abcd1234efgh","db":"main"}')
    ).not.toContain('abcd1234efgh');
  });

  it('redacts bearer tokens and vendor-prefixed keys', () => {
    expect(sanitizeErrorDetail('Authorization: Bearer eyJhbGciOi.J9zzz')).toContain(
      'Bearer ***'
    );
    const vendor = sanitizeErrorDetail('bad key sk-abcdefgh12345678 rejected');
    expect(vendor).not.toContain('sk-abcdefgh12345678');
    expect(vendor).toContain('***');
  });

  it('redacts long opaque token-shaped runs', () => {
    const token = 'a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6q7';
    const out = sanitizeErrorDetail(`session ${token} expired`);
    expect(out).toBe('session *** expired');
  });

  it('drops URL query strings', () => {
    expect(
      sanitizeErrorDetail('GET https://api.example.com/v1/x?token=abc failed')
    ).toBe('GET https://api.example.com/v1/x?*** failed');
  });

  it('redacts before truncating so a cut secret cannot leak', () => {
    const out = sanitizeErrorDetail(
      `token=abcdefghijklmnop ${'x'.repeat(400)}`,
      40
    );
    expect(out).not.toContain('abcdefghijklmnop');
    expect(out.startsWith('token=***')).toBe(true);
    expect(Array.from(out).length).toBeLessThanOrEqual(41);
  });

  it('truncates by codepoint so CJK and emoji stay intact', () => {
    const out = sanitizeErrorDetail('記憶體不足'.repeat(20), 5);
    expect(out).toBe('記憶體不足…');
    expect(sanitizeErrorDetail('🐾'.repeat(10), 3)).toBe('🐾🐾🐾…');
  });

  it('reads a message off a plain object error payload', () => {
    expect(sanitizeErrorDetail({ message: 'rpc failed', status: 500 })).toBe(
      'rpc failed'
    );
    expect(sanitizeErrorDetail({ error: 'no such agent' })).toBe(
      'no such agent'
    );
  });

  it('collapses whitespace', () => {
    expect(sanitizeErrorDetail('a\t\t  b   c')).toBe('a b c');
  });
});
