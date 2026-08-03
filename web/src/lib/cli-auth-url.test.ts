import { describe, it, expect } from 'vitest';
import { extractAuthUrl } from './cli-auth-url';

/**
 * One realistic (ANSI-stripped) login transcript per runtime the dashboard can
 * drive, asserting the link the user is offered.
 *
 * Provenance of the samples:
 *  - claude / grok: the shapes documented in `cli_auth.rs` `spec_for()` (grok's
 *    device-code wording was captured live there against grok 0.2.111).
 *  - codex / gemini: localhost-callback flows — the point of the sample is the
 *    "listening on …" line that precedes the real URL.
 *  - antigravity: reconstructed from the URL constants inside the shipped `agy`
 *    1.1.10 binary (`accounts.google.com/o/oauth2/auth`,
 *    `antigravity.google/oauth-callback`, `antigravity.google/auth-success`,
 *    `antigravity.google/docs`). The interactive TUI could not be captured
 *    headlessly, so the ORDER of the lines is representative rather than
 *    verbatim — which is precisely why the extractor must not depend on order.
 */

describe('extractAuthUrl', () => {
  it('claude setup-token: takes the consent URL', () => {
    const out = [
      'Claude Code',
      '',
      'Open this URL in your browser to authenticate:',
      'https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=https%3A%2F%2Fconsole.anthropic.com%2Foauth%2Fcode%2Fcallback&scope=user%3Ainference',
      '',
      'Paste the code here:',
    ].join('\n');
    expect(extractAuthUrl(out)).toBe(
      'https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=https%3A%2F%2Fconsole.anthropic.com%2Foauth%2Fcode%2Fcallback&scope=user%3Ainference',
    );
  });

  it('grok device-code: takes the verification URL, not the docs link', () => {
    const out = [
      'To sign in, open this URL in your browser:',
      '',
      '  https://accounts.x.ai/device?user_code=BDWX-HQTM',
      '',
      'Confirm this code in your browser:',
      '',
      '  BDWX-HQTM',
      '',
      'Waiting for authorization...',
    ].join('\n');
    expect(extractAuthUrl(out)).toBe('https://accounts.x.ai/device?user_code=BDWX-HQTM');
  });

  it('codex: skips the bare "listening on" line for the parameterised URL', () => {
    const out = [
      'Starting local login server on http://localhost:1455.',
      'If your browser did not open, navigate to:',
      'http://localhost:1455/auth/callback?state=b1f0&code_challenge=Zx9&response_type=code',
    ].join('\n');
    expect(extractAuthUrl(out)).toBe(
      'http://localhost:1455/auth/callback?state=b1f0&code_challenge=Zx9&response_type=code',
    );
  });

  it('gemini: keeps the whole consent URL including an unencoded redirect_uri', () => {
    const url =
      'https://accounts.google.com/o/oauth2/v2/auth?response_type=code&client_id=681255809395.apps.googleusercontent.com&redirect_uri=http://localhost:37891/oauth2callback&scope=https://www.googleapis.com/auth/cloud-platform';
    const out = ['Waiting for authentication on http://localhost:37891', '', url, ''].join('\n');
    expect(extractAuthUrl(out)).toBe(url);
  });

  it('antigravity (agy): takes the Google consent URL, not the oauth-callback endpoint', () => {
    // The regression: `oauth-callback` is printed FIRST and contains "oauth",
    // so first-match-wins handed the user a dead link.
    const consent =
      'https://accounts.google.com/o/oauth2/auth?client_id=974169037036.apps.googleusercontent.com&redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback&response_type=code&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Faicode&state=8f21';
    const out = [
      'Sign in to Antigravity',
      '',
      'Listening for the callback at https://antigravity.google/oauth-callback',
      'Opening your browser…',
      'If it did not open, visit:',
      consent,
      '',
      'After approving you will land on https://antigravity.google/auth-success',
      'Docs: https://antigravity.google/docs',
    ].join('\n');
    expect(extractAuthUrl(out)).toBe(consent);
  });

  it('antigravity (agy): order-independent — consent URL first still wins', () => {
    const consent =
      'https://accounts.google.com/o/oauth2/auth?client_id=974169037036.apps.googleusercontent.com&response_type=code&state=8f21';
    const out = [
      consent,
      'Waiting for https://antigravity.google/oauth-callback …',
      'Terms: https://antigravity.google/terms',
    ].join('\n');
    expect(extractAuthUrl(out)).toBe(consent);
  });

  // ── Regressions from the 2026-08-04 review (FINDING-1) ────────────────────
  //
  // The first cut ranked by query string BEFORE excluding known
  // non-destinations, so any decoy carrying a `?` beat the real URL. All three
  // of these returned the wrong link.

  it('ignores a docs link even when it carries tracking parameters', () => {
    const consent =
      'https://accounts.google.com/o/oauth2/auth?client_id=974169037036.apps.googleusercontent.com&response_type=code&state=8f21';
    const out = [consent, 'Learn more: https://antigravity.google/docs/auth?utm_source=cli&scope=all'].join('\n');
    expect(extractAuthUrl(out)).toBe(consent);
  });

  it('ignores the callback echo the CLI prints after the browser redirects back', () => {
    const consent =
      'https://accounts.google.com/o/oauth2/auth?client_id=974169037036.apps.googleusercontent.com&response_type=code&state=8f21';
    const out = [
      consent,
      'Received https://antigravity.google/oauth-callback?code=4/0AbCd&scope=openid&state=8f21',
    ].join('\n');
    expect(extractAuthUrl(out)).toBe(consent);
  });

  it('prefers a bare device URL over a terms link that has a query string', () => {
    const out = [
      'Open https://accounts.x.ai/device and enter ABCD-1234',
      'Terms: https://x.ai/legal/terms?hl=zh-tw',
    ].join('\n');
    expect(extractAuthUrl(out)).toBe('https://accounts.x.ai/device');
  });

  it('prefers the newest URL when the flow is retried', () => {
    const first = 'https://accounts.x.ai/device?user_code=AAAA-1111';
    const second = 'https://accounts.x.ai/device?user_code=BBBB-2222';
    expect(extractAuthUrl(`${first}\ncode expired, try again\n${second}`)).toBe(second);
  });

  it('strips trailing punctuation the terminal UI appends', () => {
    expect(extractAuthUrl('open (https://claude.ai/oauth/authorize?client_id=x).')).toBe(
      'https://claude.ai/oauth/authorize?client_id=x',
    );
  });

  it('falls back to a shape match when no URL carries parameters', () => {
    const out = ['See https://antigravity.google/docs', 'Authorize at https://auth.example.com/authorize'].join('\n');
    expect(extractAuthUrl(out)).toBe('https://auth.example.com/authorize');
  });

  it('returns null when the transcript has no URL yet', () => {
    expect(extractAuthUrl('Starting login…\n')).toBeNull();
  });

  it('never returns a docs link when that is all there is to choose from', () => {
    // Only non-destinations present: still returns something rather than
    // pretending, but the caller only renders it while a login is running.
    expect(extractAuthUrl('Docs: https://antigravity.google/docs')).toBe(
      'https://antigravity.google/docs',
    );
  });
});
