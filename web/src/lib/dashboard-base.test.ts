import { describe, it, expect } from 'vitest';
import { dashboardBasePath, dashboardBaseUrl, isValidPublicUrl } from './dashboard-base';

describe('isValidPublicUrl', () => {
  it('accepts empty, HTTPS and loopback HTTP bases with a sub-path', () => {
    for (const ok of ['', '  ', 'https://example.com', 'https://example.com/agents/', 'https://192.168.1.20:18889/agents',
      'http://localhost:18889/agents', 'http://127.0.0.1/agents', 'http://127.42.3.4:18889/agents',
      'http://127.255.255.255/agents', 'http://[::1]:18889/agents']) {
      expect(isValidPublicUrl(ok), ok).toBe(true);
    }
  });

  it('refuses what the gateway refuses', () => {
    for (const bad of [
      'http://dashboard.example/agents', 'http://192.168.1.20/agents',
      'http://localhost.evil.example/agents', 'http://127.0.0.1.evil.example/agents',
      'http://[::ffff:127.0.0.1]/agents', 'http://[::2]/agents', 'http://128.0.0.1/agents',
      '/agents', 'example.com/agents', 'ftp://example.com', 'https://u:p@example.com/a', 'https://@example.com/a',
      'https://example.com/a?x=1', 'https://example.com/a#f', 'https://example.com/a/../b',
      'https://example.com/a/%2e%2e/b', 'https://example.com/a%2fb', 'https://example.com/a\\b',
    ]) {
      expect(isValidPublicUrl(bad), bad).toBe(false);
    }
  });
});

function fakeDoc(baseURI: string, hasBase: boolean) {
  return {
    baseURI,
    querySelector: (() => (hasBase ? ({} as Element) : null)) as Document['querySelector'],
  };
}

describe('dashboardBasePath', () => {
  it('is / without a <base> element, whatever the page URL', () => {
    expect(dashboardBasePath(fakeDoc('https://example.com/agents/manage/integrations?tab=mcp', false), 'https://example.com')).toBe('/');
  });

  it('uses a same-origin <base href> sub-path with one trailing slash', () => {
    for (const base of ['https://example.com/agents', 'https://example.com/agents/', 'https://example.com/agents//']) {
      expect(dashboardBasePath(fakeDoc(base, true), 'https://example.com')).toBe('/agents/');
    }
    expect(dashboardBasePath(fakeDoc('https://example.com/', true), 'https://example.com')).toBe('/');
  });

  it('ignores a cross-origin or unparsable <base>', () => {
    expect(dashboardBasePath(fakeDoc('https://evil.example/agents/', true), 'https://example.com')).toBe('/');
    expect(dashboardBasePath(fakeDoc('not a url', true), 'https://example.com')).toBe('/');
    expect(dashboardBasePath(undefined, 'https://example.com')).toBe('/');
  });
});

describe('dashboardBaseUrl', () => {
  it('keeps the bare origin at the root', () => {
    expect(dashboardBaseUrl(fakeDoc('https://example.com/', false), 'https://example.com')).toBe('https://example.com');
  });

  it('appends the sub-path behind a reverse proxy prefix', () => {
    expect(dashboardBaseUrl(fakeDoc('https://example.com/agents/', true), 'https://example.com')).toBe('https://example.com/agents/');
  });

  it('defaults to the live document (jsdom has no <base>)', () => {
    expect(dashboardBaseUrl()).toBe(window.location.origin);
  });

  it('is empty without an origin', () => {
    expect(dashboardBaseUrl(fakeDoc('https://example.com/agents/', true), '')).toBe('');
  });
});
