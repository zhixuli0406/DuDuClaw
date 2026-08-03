import { describe, expect, it } from 'vitest';
import { formatSource } from './KnowledgeCuration';

/**
 * WP5c — the curation station renders the source chain the backend stamps on
 * every auto-filed page (`conversation:<session-id>:<rfc3339>`). Session ids
 * carry colons of their own, so the formatter must not assume a fixed field
 * count, and it must degrade to something readable rather than throw.
 */
describe('formatSource', () => {
  it('renders channel and time from a telegram source id', () => {
    const out = formatSource('conversation:telegram:12345:0:2026-08-04T10:12:33Z');
    expect(out).toMatch(/^Telegram · 8\/4 \d{2}:\d{2}$/);
  });

  it('handles a webchat session id containing # and extra colons', () => {
    const out = formatSource('conversation:webchat:conn#agent:a#conv:x:2026-08-04T10:12:33Z');
    expect(out.startsWith('網頁對話 · ')).toBe(true);
  });

  it('shows an unknown channel token verbatim rather than hiding the row', () => {
    expect(formatSource('conversation:carrier-pigeon:1:2026-08-04T10:12:33Z')).toMatch(
      /^carrier-pigeon · /,
    );
  });

  it('falls back to the channel when there is no timestamp', () => {
    expect(formatSource('conversation:telegram:12345')).toBe('Telegram');
  });

  it('passes unfamiliar shapes through untouched', () => {
    expect(formatSource('imported-by-hand')).toBe('imported-by-hand');
    expect(formatSource('')).toBe('');
  });

  it('never throws on a malformed timestamp', () => {
    expect(() => formatSource('conversation:line:9:9999-99-99T99:99:99Z')).not.toThrow();
  });
});
