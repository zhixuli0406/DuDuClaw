import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { SecauditPage } from './SecauditPage';

const row = {
  file: '20260817T000000Z.json',
  mtime: '2026-08-17T00:00:00Z',
  repo: '/tmp/repo',
  started_at: '2026-08-17T00:00:00Z',
  profile_mode: 'quick',
  total_findings: 2,
  by_severity: { critical: 1, high: 1, medium: 0, low: 0, info: 0 },
  engines_run_count: 1,
  engines_missing_count: 1,
  parse_error: null,
};

const report = {
  repo: '/tmp/repo',
  started_at: '2026-08-17T00:00:00Z',
  profile: { mode: 'quick', intake: null },
  engines_run: [
    { engine: 'gitleaks', findings_count: 1, duration_ms: 10, parse_error: null, timed_out: false },
  ],
  engines_missing: [{ engine: 'osv-scanner', reason: 'requires network access' }],
  findings: [
    {
      id: 'gitleaks-aaa',
      source_engine: 'gitleaks',
      kind: 'secret',
      severity: 'critical',
      title: 'Hardcoded API key',
      file: 'config.py',
      line: 12,
      snippet: 'API_KEY = "***"',
      rule_id: 'generic-api-key',
      evidence: [
        { kind: 'static_hit', source: 'gitleaks', detail: 'matched pattern', recorded_at: '2026-08-17T00:00:00Z' },
      ],
      status: 'candidate',
    },
    {
      id: 'semgrep-bbb',
      source_engine: 'semgrep',
      kind: 'static_analysis',
      severity: 'high',
      title: 'SQL injection risk',
      file: 'src/db.py',
      line: 42,
      snippet: 'cursor.execute(query)',
      rule_id: 'sql-injection',
      evidence: [],
      status: 'candidate',
    },
  ],
  summary: {
    total_findings: 2,
    by_severity: { critical: 1, high: 1, medium: 0, low: 0, info: 0 },
    engines_run_count: 1,
    engines_missing_count: 1,
  },
};

function defaultImpl(method: string) {
  switch (method) {
    case 'secaudit.reports':
      return Promise.resolve({ reports: [row] });
    case 'secaudit.report':
      return Promise.resolve({ report });
    default:
      return Promise.resolve({});
  }
}

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  mockWsClient.call.mockImplementation(defaultImpl);
  try {
    localStorage.clear();
  } catch {
    /* jsdom */
  }
});

describe('SecauditPage', () => {
  it('shows the CLI-hint empty state when there are no reports yet', async () => {
    mockWsClient.call.mockImplementation((method: string) =>
      method === 'secaudit.reports' ? Promise.resolve({ reports: [] }) : Promise.resolve({}),
    );
    renderWithProviders(<SecauditPage />);
    expect(await screen.findByText('No security audit reports yet')).toBeInTheDocument();
    expect(
      screen.getByText(/duduclaw secaudit <repo> --save/),
    ).toBeInTheDocument();
  });

  it('lists reports and opens the selected report’s findings, grouped by severity', async () => {
    const user = userEvent.setup();
    renderWithProviders(<SecauditPage />);
    const repoRow = await screen.findByText('/tmp/repo');
    await user.click(repoRow);

    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('secaudit.report', { file: row.file });
    });

    expect(await screen.findByText('Hardcoded API key')).toBeInTheDocument();
    expect(screen.getByText('SQL injection risk')).toBeInTheDocument();
    // Severity section headers.
    expect(screen.getByText('Critical')).toBeInTheDocument();
    expect(screen.getByText('High')).toBeInTheDocument();
    // Engines run/skipped summary.
    expect(screen.getByText(/Ran: gitleaks/)).toBeInTheDocument();
    expect(screen.getByText(/Skipped: osv-scanner/)).toBeInTheDocument();
  });

  it('expands a finding to reveal its evidence chain', async () => {
    const user = userEvent.setup();
    renderWithProviders(<SecauditPage />);
    await user.click(await screen.findByText('/tmp/repo'));
    await user.click(await screen.findByText('Hardcoded API key'));

    expect(screen.getByText('Evidence chain')).toBeInTheDocument();
    expect(screen.getByText('Static scan hit')).toBeInTheDocument();
    expect(screen.getByText('matched pattern')).toBeInTheDocument();
    expect(screen.getByText('Rule: generic-api-key')).toBeInTheDocument();
  });

  it('optimistically confirms a finding, then rolls back when the write fails', async () => {
    let settle: ((value: unknown) => void) | null = null;
    let fail: ((err: unknown) => void) | null = null;
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'secaudit.finding_status') {
        return new Promise((resolve, reject) => {
          settle = resolve;
          fail = reject;
        });
      }
      return defaultImpl(method);
    });

    const user = userEvent.setup();
    renderWithProviders(<SecauditPage />);
    await user.click(await screen.findByText('/tmp/repo'));
    await user.click(await screen.findByText('Hardcoded API key'));

    const card = screen.getByText('Hardcoded API key').closest('[data-slot="card"]') as HTMLElement;
    expect(within(card).getByText('Unreviewed')).toBeInTheDocument();

    await user.click(within(card).getByRole('button', { name: /Confirm/ }));

    // Optimistic: flips to "Confirmed" before the write settles.
    await waitFor(() => expect(within(card).getByText('Confirmed')).toBeInTheDocument());
    expect(mockWsClient.call).toHaveBeenCalledWith('secaudit.finding_status', {
      file: row.file,
      finding_id: 'gitleaks-aaa',
      status: 'confirmed',
    });

    // The other finding is untouched by the optimistic update.
    expect(screen.getByText('SQL injection risk')).toBeInTheDocument();

    // Now the write fails — the finding must roll back to its prior status.
    fail!(new Error('boom'));
    await waitFor(() => expect(within(card).getByText('Unreviewed')).toBeInTheDocument());
    void settle; // referenced only to satisfy the linter's no-unused-vars on the success path
  });
});
