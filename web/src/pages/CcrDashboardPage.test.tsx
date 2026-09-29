import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter } from 'react-router';
import en from '@/i18n/en.json';
import { ccrApi } from '@/lib/ccr-api';
import { CcrDashboardPage } from './CcrDashboardPage';

vi.mock('@/lib/ccr-api', () => ({ ccrApi: { dashboard: vi.fn() } }));

const ready = {
  status: 'ready' as const,
  entries: { eligible_retained: 4, expired_retained: 1, revoked_retained: 0, declared_original_bytes_eligible: 8192 },
  revocations: { source_calls: 2, scopes: 1, artifact_versions: 3 },
  retrieval_audit: { grants: 8, refusals: 5, returned_bytes_granted: 1024, window_max_rows: 10000 },
  compression_metrics: {
    available: true, reason: null, window_max_rows: 10000,
    observed_loops: 3, provider_rounds: 7, usage_reported_rounds: 5,
    input_tokens: 100, output_tokens: 40, cache_read_tokens: 20,
    cache_write_tokens: 10, reasoning_tokens: 5,
    ccr_compressed_results: 2, ccr_original_bytes: 9000, ccr_delivered_bytes: 2000,
    ccr_find_attempts: 3, ccr_find_hits: 2, ccr_find_misses: 1,
    ccr_retrieve_attempts: 2, ccr_retrieve_successes: 1, ccr_retrieve_misses: 1,
    ccr_retrieved_bytes: 256, p95_elapsed_millis: 420,
  },
  causal_revocation_outbox: {
    available: true, reason: null, pending_notices: 2, oldest_pending_seconds: 360,
  },
  causal_delivery: {
    available: true, reason: null, active_leases: 1, oldest_lease_seconds: 45,
    revoking_sources: 2, oldest_revoking_seconds: 240,
  },
  connector_lifecycle: {
    available: true, reason: null, pending_events: 3, blocked_events: 1,
    completed_events: 5, oldest_pending_seconds: 600, latest_completed_seconds: 12,
    last_failure_code: 'delivery_lease_active' as const,
  },
};

function renderPage(initialEntry = '/app/system/ccr') {
  render(<IntlProvider locale="en" messages={en}><MemoryRouter initialEntries={[initialEntry]}><CcrDashboardPage /></MemoryRouter></IntlProvider>);
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(ccrApi.dashboard).mockResolvedValue(ready);
});

describe('CcrDashboardPage', () => {
  it('prefills the exact tenant passed from Decision Lab', async () => {
    const user = userEvent.setup();
    renderPage('/app/system/ccr?tenant_id=tenant-a');
    expect(screen.getByRole('textbox', { name: en['ccrDashboard.tenant'] })).toHaveValue('tenant-a');
    await user.click(screen.getByRole('button', { name: en['ccrDashboard.refresh'] }));
    await waitFor(() => expect(ccrApi.dashboard).toHaveBeenCalledWith('tenant-a'));
  });

  it('shows scoped loop aggregates, usage coverage, and p95 with honest limits', async () => {
    const user = userEvent.setup();
    renderPage();
    await user.click(screen.getByRole('button', { name: en['ccrDashboard.refresh'] }));
    await waitFor(() => expect(ccrApi.dashboard).toHaveBeenCalledWith('local'));
    expect(screen.getByText(en['ccrDashboard.eligibleRetained'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.eligibilityNote'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.refusals'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.loopMetrics'])).toBeInTheDocument();
    expect(screen.getByText('5 / 7')).toBeInTheDocument();
    expect(screen.getByText('420')).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.metricsNote'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.pendingNotices'])).toBeInTheDocument();
    expect(screen.getByText('360')).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.causalOutboxNote'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.activeLeases'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.revokingSources'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.causalDeliveryNote'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.connectorLifecycle'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.connectorCompleted'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.connectorLatestCompleted'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.connectorFailure.delivery_lease_active'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.connectorLifecycleNote'])).toBeInTheDocument();
    expect(screen.queryByText(/source_acl|principal-sha256|original content/i)).not.toBeInTheDocument();
  });

  it('distinguishes legacy stores from an empty retained telemetry window', async () => {
    const user = userEvent.setup();
    vi.mocked(ccrApi.dashboard)
      .mockResolvedValueOnce({ ...ready, compression_metrics: { ...ready.compression_metrics,
        available: false, reason: 'not_persisted' } })
      .mockResolvedValueOnce({ ...ready, compression_metrics: { ...ready.compression_metrics,
        observed_loops: 0, provider_rounds: 0, usage_reported_rounds: 0,
        p95_elapsed_millis: null } });
    renderPage();
    await user.click(screen.getByRole('button', { name: en['ccrDashboard.refresh'] }));
    expect(await screen.findByText(en['ccrDashboard.metricsUnavailable'])).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: en['ccrDashboard.refresh'] }));
    expect(await screen.findByText(en['ccrDashboard.noObservations'])).toBeInTheDocument();
    expect(screen.queryByText(en['ccrDashboard.metricsUnavailable'])).not.toBeInTheDocument();
  });

  it('clears one tenant’s counters before loading another tenant', async () => {
    const user = userEvent.setup();
    vi.mocked(ccrApi.dashboard)
      .mockResolvedValueOnce(ready)
      .mockResolvedValueOnce({ ...ready, status: 'absent' });
    renderPage();
    await user.click(screen.getByRole('button', { name: en['ccrDashboard.refresh'] }));
    expect(await screen.findByText(en['ccrDashboard.eligibleRetained'])).toBeInTheDocument();
    await user.clear(screen.getByRole('textbox', { name: en['ccrDashboard.tenant'] }));
    await user.type(screen.getByRole('textbox', { name: en['ccrDashboard.tenant'] }), 'another');
    expect(screen.queryByText(en['ccrDashboard.eligibleRetained'])).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: en['ccrDashboard.refresh'] }));
    await waitFor(() => expect(ccrApi.dashboard).toHaveBeenLastCalledWith('another'));
    expect(await screen.findByText(en['ccrDashboard.absent'])).toBeInTheDocument();
    expect(screen.queryByText(en['ccrDashboard.eligibleRetained'])).not.toBeInTheDocument();
  });

  it('shows pending source notices even when the CCR database is absent', async () => {
    const user = userEvent.setup();
    vi.mocked(ccrApi.dashboard).mockResolvedValue({ ...ready, status: 'absent' });
    renderPage();
    await user.click(screen.getByRole('button', { name: en['ccrDashboard.refresh'] }));
    expect(await screen.findByText(en['ccrDashboard.absent'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.pendingNotices'])).toBeInTheDocument();
    expect(screen.getByText('360')).toBeInTheDocument();
    expect(screen.queryByText(en['ccrDashboard.eligibleRetained'])).not.toBeInTheDocument();
  });

  it('distinguishes an unavailable or legacy source queue from an empty queue', async () => {
    const user = userEvent.setup();
    vi.mocked(ccrApi.dashboard)
      .mockResolvedValueOnce({ ...ready, causal_revocation_outbox: {
        available: false, reason: 'legacy_schema', pending_notices: 0, oldest_pending_seconds: null,
      } })
      .mockResolvedValueOnce({ ...ready, causal_revocation_outbox: {
        available: false, reason: 'source_db_absent', pending_notices: 0, oldest_pending_seconds: null,
      } })
      .mockResolvedValueOnce({ ...ready, causal_revocation_outbox: {
        available: true, reason: null, pending_notices: 0, oldest_pending_seconds: null,
      } });
    renderPage();
    const refresh = screen.getByRole('button', { name: en['ccrDashboard.refresh'] });
    await user.click(refresh);
    expect(await screen.findByText(en['ccrDashboard.causalOutboxLegacy'])).toBeInTheDocument();
    expect(screen.queryByText(en['ccrDashboard.pendingNotices'])).not.toBeInTheDocument();
    await user.click(refresh);
    expect(await screen.findByText(en['ccrDashboard.causalOutboxAbsent'])).toBeInTheDocument();
    await user.click(refresh);
    expect(await screen.findByText(en['ccrDashboard.pendingNotices'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.oldestPendingSeconds']).nextSibling).toHaveTextContent('—');
  });

  it('distinguishes unknown source lease state from an observed zero', async () => {
    const user = userEvent.setup();
    vi.mocked(ccrApi.dashboard)
      .mockResolvedValueOnce({ ...ready, causal_delivery: {
        available: false, reason: 'legacy_schema', active_leases: 0,
        oldest_lease_seconds: null, revoking_sources: 0, oldest_revoking_seconds: null,
      } })
      .mockResolvedValueOnce({ ...ready, causal_delivery: {
        available: true, reason: null, active_leases: 0,
        oldest_lease_seconds: null, revoking_sources: 0, oldest_revoking_seconds: null,
      } });
    renderPage();
    const refresh = screen.getByRole('button', { name: en['ccrDashboard.refresh'] });
    await user.click(refresh);
    expect(await screen.findByText(en['ccrDashboard.causalDeliveryLegacy'])).toBeInTheDocument();
    expect(screen.queryByText(en['ccrDashboard.activeLeases'])).not.toBeInTheDocument();
    await user.click(refresh);
    expect(await screen.findByText(en['ccrDashboard.activeLeases'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.oldestLeaseSeconds']).nextSibling).toHaveTextContent('—');
  });

  it('distinguishes missing connector journal from zero pending events', async () => {
    const user = userEvent.setup();
    vi.mocked(ccrApi.dashboard)
      .mockResolvedValueOnce({ ...ready, connector_lifecycle: {
        available: false, reason: 'legacy_schema', pending_events: 0, blocked_events: 0,
        completed_events: 0, oldest_pending_seconds: null, latest_completed_seconds: null,
        last_failure_code: null,
      } })
      .mockResolvedValueOnce({ ...ready, connector_lifecycle: {
        available: true, reason: null, pending_events: 0, blocked_events: 0,
        completed_events: 0, oldest_pending_seconds: null, latest_completed_seconds: null,
        last_failure_code: null,
      } });
    renderPage();
    const refresh = screen.getByRole('button', { name: en['ccrDashboard.refresh'] });
    await user.click(refresh);
    expect(await screen.findByText(en['ccrDashboard.connectorLifecycleLegacy'])).toBeInTheDocument();
    expect(screen.queryByText(en['ccrDashboard.connectorPending'])).not.toBeInTheDocument();
    await user.click(refresh);
    expect(await screen.findByText(en['ccrDashboard.connectorPending'])).toBeInTheDocument();
    expect(screen.getByText(en['ccrDashboard.connectorOldest']).nextSibling).toHaveTextContent('—');
  });
});
