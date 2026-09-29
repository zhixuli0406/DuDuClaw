import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { SecurityPage } from './SecurityPage';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from '@/stores/system-store';

/** Shared response shape — satisfies security.status() / auditLog() /
 *  killswitch.get() in one mocked call (see the first test's own note). */
const SECURITY_RESPONSE = {
  events: [],
  triggers: {
    max_replies_per_minute: 30,
    max_consecutive_errors: 5,
    error_rate_threshold: 0.5,
    cost_limit_usd: 10,
  },
  circuit_breaker: {
    frequency_window_secs: 60,
    frequency_max_replies: 20,
    similarity_threshold: 0.8,
    token_explosion_multiplier: 5,
    cooldown_secs: 30,
    half_open_allow_count: 3,
  },
  safety_words: { stop: ['stop'], stop_all: [], resume: [], status: [] },
  defensive_prompt: { enabled: true, languages: ['zh-TW'] },
};

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // Default to the non-personal layout; the D9 split tests opt into Personal.
  useSystemStore.setState({ status: null } as never);
  // AdvancedSection remembers open/closed per storageKey in localStorage —
  // clear so the D9 fold test always starts from the documented collapsed default.
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('SecurityPage', () => {
  it('renders the security heading and audit empty state', async () => {
    // A single mocked WS response shape must satisfy security.status(),
    // security.auditLog() (reads `.events`), and killswitch.get() (reads
    // triggers/circuit_breaker/safety_words/defensive_prompt) — the page
    // renders defensively via optional chaining for the fields this shape
    // doesn't carry (credential_proxy / mount_guard / rbac / rate_limiter).
    mockWsClient.call.mockResolvedValue({
      events: [],
      triggers: {
        max_replies_per_minute: 0,
        max_consecutive_errors: 0,
        error_rate_threshold: 0,
        cost_limit_usd: 0,
      },
      circuit_breaker: {
        frequency_window_secs: 0,
        frequency_max_replies: 0,
        similarity_threshold: 0,
        token_explosion_multiplier: 1,
        cooldown_secs: 0,
        half_open_allow_count: 1,
      },
      safety_words: { stop: [], stop_all: [], resume: [], status: [] },
      defensive_prompt: { enabled: false, languages: [] },
    });

    renderWithProviders(<SecurityPage />);

    expect(screen.getByText('Security')).toBeInTheDocument();
    expect(await screen.findByText('No security events')).toBeInTheDocument();
  });

  it('renders the killswitch editor once config loads', async () => {
    mockWsClient.call.mockResolvedValue({
      events: [],
      triggers: {
        max_replies_per_minute: 30,
        max_consecutive_errors: 5,
        error_rate_threshold: 0.5,
        cost_limit_usd: 10,
      },
      circuit_breaker: {
        frequency_window_secs: 60,
        frequency_max_replies: 20,
        similarity_threshold: 0.8,
        token_explosion_multiplier: 5,
        cooldown_secs: 30,
        half_open_allow_count: 3,
      },
      safety_words: { stop: ['stop'], stop_all: [], resume: [], status: [] },
      defensive_prompt: { enabled: true, languages: ['zh-TW'] },
    });

    renderWithProviders(<SecurityPage />);

    expect(await screen.findByText('Kill Switch')).toBeInTheDocument();
  });

  // D9 (09-edition-split-features.md §4) — the organisation-scale cards have
  // no single-owner counterpart and stay off the page entirely.
  // G2 (2026-09 feature audit): the credential-proxy and mount-guard cards
  // were removed — their Rust modules were deleted and the numbers measured
  // something other than what the labels claimed. What stays organisation-
  // scale on this page is the RBAC matrix.
  it('enterprise: renders the RBAC card and no removed credential/mount cards', async () => {
    mockWsClient.call.mockResolvedValue(SECURITY_RESPONSE);
    renderWithProviders(<SecurityPage />);
    expect(await screen.findByText('Role permissions')).toBeInTheDocument();
    expect(screen.queryByText('Credential proxy')).not.toBeInTheDocument();
    expect(screen.queryByText('Mount guard')).not.toBeInTheDocument();
    // Kill switch is unfolded by default on this edition.
    expect(screen.getByText('Kill Switch')).toBeInTheDocument();
  });

  it('personal: hides RBAC, folds audit log + kill switch', async () => {
    const user = userEvent.setup();
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    mockWsClient.call.mockResolvedValue(SECURITY_RESPONSE);
    renderWithProviders(<SecurityPage />);

    // Organisation-scale cards are gone, not just collapsed.
    expect(screen.queryByText('Role permissions')).not.toBeInTheDocument();
    expect(screen.queryByText('Credential proxy')).not.toBeInTheDocument();
    expect(screen.queryByText('Mount guard')).not.toBeInTheDocument();

    // Audit log + kill switch are reachable but start folded (D9 "頁內摺疊區").
    expect(screen.queryByText('Kill Switch')).not.toBeInTheDocument();
    const toggle = screen.getByRole('button', { name: /Advanced: logs & emergency brake/i });
    await user.click(toggle);
    expect(await screen.findByText('Kill Switch')).toBeInTheDocument();
    expect(screen.getByText('Security Audit Log')).toBeInTheDocument();
  });
});
