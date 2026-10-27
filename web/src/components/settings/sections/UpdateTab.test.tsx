import { describe, it, expect } from 'vitest';
import { restartPendingMessageId, showsNetworkHint } from './UpdateTab';

/**
 * WP18-A (2026-08-04 field report): the gateway retries download-class update
 * failures twice and never retries an integrity failure. The dashboard has to
 * mirror that split in what it TELLS the user — 「通常是網路或發布傳播延遲，稍後
 * 再試」 is the right advice after an exhausted download, and the wrong advice
 * after a checksum or signature mismatch, which no amount of waiting fixes and
 * which may mean the artifact was tampered with.
 *
 * The strings below are the ones `crates/duduclaw-gateway/src/updater.rs`
 * actually produces, carrying the localised 「更新失敗: 」 prefix the UI adds.
 */
const PREFIX = '更新失敗: ';

describe('UpdateTab failure classification', () => {
  it('offers the "try again later" hint for exhausted download failures', () => {
    expect(showsNetworkHint(`${PREFIX}Update archive download failed: connection reset`)).toBe(true);
    expect(showsNetworkHint(`${PREFIX}Update archive download returned HTTP 404`)).toBe(true);
    expect(showsNetworkHint(`${PREFIX}Checksum file download returned HTTP 503`)).toBe(true);
    expect(showsNetworkHint(`${PREFIX}Signature file download returned HTTP 429`)).toBe(true);
    expect(showsNetworkHint(`${PREFIX}Failed to read Update archive: operation timed out`)).toBe(true);
  });

  it('withholds it on integrity failures — waiting never fixes those', () => {
    expect(
      showsNetworkHint(`${PREFIX}SHA-256 checksum mismatch!\n  Expected: ab\n  Computed: cd`),
    ).toBe(false);
    expect(
      showsNetworkHint(`${PREFIX}Checksum file has invalid format (no SHA-256 digest found)`),
    ).toBe(false);
    expect(
      showsNetworkHint(`${PREFIX}Ed25519 signature verification FAILED — refusing update: bad`),
    ).toBe(false);
    expect(showsNetworkHint(`${PREFIX}Signature file has invalid format: truncated`)).toBe(false);
  });

  // The HTTP branch mirrors `updater::classify_http_status`, so the advice the
  // UI gives matches whether the gateway actually retried.
  it('withholds it on permanently-failing HTTP statuses', () => {
    expect(showsNetworkHint(`${PREFIX}Update archive download returned HTTP 410`)).toBe(false);
    expect(showsNetworkHint(`${PREFIX}Update archive download returned HTTP 400`)).toBe(false);
    expect(showsNetworkHint(`${PREFIX}Update archive download returned HTTP 451`)).toBe(false);
  });

  it('withholds it on non-network fatal failures and on an empty message', () => {
    expect(showsNetworkHint(`${PREFIX}Rejected unsafe download URL: http://evil/x`)).toBe(false);
    expect(showsNetworkHint(`${PREFIX}New binary verification failed: exec format error`)).toBe(false);
    expect(showsNetworkHint('')).toBe(false);
  });
});

// A desktop sidecar restarts with its app and a container restarts from a new
// image, so only a standalone gateway may say "restarting the desktop app
// does not restart the gateway" (2026-10-27 review of v1.76.2).
describe('UpdateTab restart-pending banner', () => {
  it('keeps the standalone wording for launchd/systemd/npm/pro gateways', () => {
    for (const install_method of ['npm', 'pro', 'binary', 'cargo']) {
      expect(restartPendingMessageId({ install_method })).toBe('settings.update.restartPending');
    }
  });

  it('tells a desktop sidecar to reopen the app', () => {
    expect(restartPendingMessageId({ install_method: 'desktop' })).toBe(
      'settings.update.restartPendingDesktop',
    );
  });

  it('tells a container to recreate from the image, whatever the install method', () => {
    expect(restartPendingMessageId({ install_method: 'pro', containerized: true })).toBe(
      'settings.update.restartPendingContainer',
    );
    expect(restartPendingMessageId({ install_method: 'desktop', containerized: true })).toBe(
      'settings.update.restartPendingContainer',
    );
  });
});
