import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { SecurityPage } from './SecurityPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
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
});
