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

const v2Report = {
  ...report,
  schema_version: 2,
  run_status: 'incomplete',
  incomplete_reason: 'validation_budget_exhausted',
  coverage: [],
  prior_run: {
    report_file: 'old.json',
    carried_suppressed: 3,
    carried_refuted: 2,
    revalidated_prior_confirmed: 1,
    changed_source: 4,
  },
  verifier: { independence: 'different_agent', audit_agent: 'auditor', verifier_agent: 'checker' },
  findings: [
    {
      ...report.findings[0],
      id: 'ai-ccc',
      source_engine: 'ai_audit',
      title: 'Unchecked redirect target',
      severity_basis: 'model_self_reported',
      status: 'needs_human',
      threat_model: {
        principal: 'Anonymous visitor',
        input: 'next query parameter',
        control: 'No allowlist',
        boundary: 'Site to external domain',
        affected: 'Logged-in users',
        result: 'Phishing redirect',
      },
      trace: [
        { kind: 'entrypoint', file: 'src/web.py', line: 10, scope: 'login', description: 'reads next param' },
        { kind: 'sink', file: 'src/web.py', line: 30, scope: 'redirect', description: 'issues redirect' },
      ],
      conditions: [{ kind: 'user_interaction', description: 'victim clicks crafted link' }],
      blockers: ['Is there a reverse-proxy rewrite?'],
      validation_plan: { local: 'Run the dev server and request /login?next=//evil', deployment: null },
      precheck: { passed: false, violations: ['line out of range'] },
    },
  ],
  summary: {
    ...report.summary,
    needs_human_by_severity: { critical: 0, high: 1, medium: 0, low: 0, info: 0 },
    gate_includes_needs_human: false,
    coverage: { modules_total: 9, covered: 4, candidate: 2, deferred: 2, failed: 1, partial: true },
    precheck_refuted: 1,
    carried_from_prior: 5,
  },
};

let currentReport: unknown = report;

function defaultImpl(method: string) {
  switch (method) {
    case 'secaudit.reports':
      return Promise.resolve({ reports: [row] });
    case 'secaudit.report':
      return Promise.resolve({ report: currentReport });
    default:
      return Promise.resolve({});
  }
}

beforeEach(() => {
  vi.clearAllMocks();
  currentReport = report;
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

  it('renders a v1 report unchanged: none of the v2 blocks appear', async () => {
    const user = userEvent.setup();
    renderWithProviders(<SecauditPage />);
    await user.click(await screen.findByText('/tmp/repo'));
    await user.click(await screen.findByText('Hardcoded API key'));
    expect(screen.getByText('Evidence chain')).toBeInTheDocument();
    for (const txt of [
      'This audit did not finish',
      'Audit coverage',
      'Threat model',
      'Trace',
      'Conditions',
      'Open questions',
      'Suggested validation',
      'Automatic checks failed',
      'Model self-assessed',
    ]) {
      expect(screen.queryByText(txt)).not.toBeInTheDocument();
    }
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('renders every v2 block for a v2 report', async () => {
    currentReport = v2Report;
    const user = userEvent.setup();
    renderWithProviders(<SecauditPage />);
    await user.click(await screen.findByText('/tmp/repo'));
    await screen.findByText('Unchecked redirect target');

    // Header: incomplete banner, coverage card, chips, stats rows.
    const alert = screen.getByRole('alert');
    expect(within(alert).getByText('This audit did not finish')).toBeInTheDocument();
    expect(within(alert).getByText(/validation budget or candidate limit/)).toBeInTheDocument();
    expect(screen.getByText('Audit coverage')).toBeInTheDocument();
    expect(screen.getByText('Partial coverage')).toBeInTheDocument();
    expect(screen.getByText('Not reviewed yet')).toBeInTheDocument();
    expect(screen.getByText(/Reviewed by an independent agent/)).toBeInTheDocument();
    expect(screen.getByText(/audit: auditor, review: checker/)).toBeInTheDocument();
    expect(screen.getByText(/Carried from an earlier report: 3 kept as suppressed/)).toBeInTheDocument();
    expect(screen.getByText('Dropped by automatic checks')).toBeInTheDocument();
    expect(screen.getByText(/5 carried in total/)).toBeInTheDocument();
    expect(screen.getByText('Counted toward the pass/fail gate')).toBeInTheDocument();
    expect(screen.getByText(/self-reported by the model, not counted toward the gate/)).toBeInTheDocument();

    // List row hint, collapsed: detail blocks hidden.
    expect(screen.getByText('Model self-assessed')).toBeInTheDocument();
    expect(screen.queryByText('Threat model')).not.toBeInTheDocument();

    await user.click(screen.getByText('Unchecked redirect target'));
    expect(screen.getByText('Threat model')).toBeInTheDocument();
    expect(screen.getByText('Anonymous visitor')).toBeInTheDocument();
    expect(screen.getByText('Phishing redirect')).toBeInTheDocument();
    expect(screen.getByText('Trace')).toBeInTheDocument();
    expect(screen.getByText('Entry')).toBeInTheDocument();
    expect(screen.getByText('Sink')).toBeInTheDocument();
    expect(screen.getByText('src/web.py:30')).toBeInTheDocument();
    expect(screen.getByText('Conditions')).toBeInTheDocument();
    expect(screen.getByText('User action')).toBeInTheDocument();
    expect(screen.getByText('Open questions')).toBeInTheDocument();
    expect(screen.getByText('Is there a reverse-proxy rewrite?')).toBeInTheDocument();
    expect(screen.getByText('Suggested validation')).toBeInTheDocument();
    expect(screen.getByText('On your machine')).toBeInTheDocument();
    expect(screen.queryByText('In the deployed environment')).not.toBeInTheDocument();
    expect(screen.getByText('Automatic checks failed')).toBeInTheDocument();
    expect(screen.getByText('line out of range')).toBeInTheDocument();
  });

  it('says the gate includes needs-human items when gate_includes_needs_human is true', async () => {
    currentReport = {
      ...v2Report,
      run_status: 'complete',
      incomplete_reason: null,
      summary: { ...v2Report.summary, gate_includes_needs_human: true },
    };
    const user = userEvent.setup();
    renderWithProviders(<SecauditPage />);
    await user.click(await screen.findByText('/tmp/repo'));
    expect(await screen.findByText(/are counted toward the gate/)).toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });
});
