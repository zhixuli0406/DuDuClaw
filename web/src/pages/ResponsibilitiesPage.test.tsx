import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { ResponsibilitiesPage, buildInput, controlsFor, laneOf } from './ResponsibilitiesPage';
import { useConnectionStore } from '@/stores/connection-store';
import { useAuthStore } from '@/stores/auth-store';
import type { ResponsibilityRow } from '@/lib/api';

function renderPage() {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={['/responsibilities']}>
        <ResponsibilitiesPage />
      </MemoryRouter>
    </IntlProvider>,
  );
}

const row: ResponsibilityRow = {
  responsibility_id: 'r1',
  owner_agent_id: 'alice',
  created_by: 'op',
  objective: 'Morning briefing',
  acceptance_template: 'Five points',
  scope_json: '{"event_names":[],"lane":"explore"}',
  source_refs_json: '[]',
  notification_policy_json: '{}',
  schedule_json: '{"cron":"0 0 8 * * *","timezone":"Asia/Taipei"}',
  occurrence_hours: 1,
  occurrence_cost_cap_cents: 50,
  budget_period: 'day',
  budget_timezone: 'Asia/Taipei',
  period_cost_limit_cents: 50,
  period_occurrence_limit: 1,
  min_wake_interval_secs: 3600,
  max_consecutive_failures: 3,
  stop_at: '2026-11-01T00:00:00Z',
  state: 'active',
  state_reason: null,
  state_changed_by: null,
  state_changed_at: null,
  contract_revision: 1,
  contract_hash: 'h',
  control_epoch: 4,
  consecutive_failures: 0,
  last_occurrence_at: null,
  created_at: '2026-10-01T00:00:00Z',
  updated_at: '2026-10-01T00:00:00Z',
};

function mock(opts: { enabled?: boolean; rows?: ResponsibilityRow[]; onCall?: (m: string, p: unknown) => void }) {
  mockWsClient.call.mockImplementation((method: string, params?: unknown) => {
    opts.onCall?.(method, params);
    switch (method) {
      case 'responsibilities.status':
        return Promise.resolve({
          enabled: opts.enabled ?? true,
          dispatch_enabled: true,
          max_stop_at_days: 30,
          lanes: ['explore'],
        });
      case 'responsibilities.list':
        return Promise.resolve({ responsibilities: opts.rows ?? [] });
      case 'responsibilities.get':
        return Promise.resolve({
          summary: {
            responsibility: row,
            next_due_at: '2026-10-09T00:00:00Z',
            pending_fires: 0,
            period_key: 'd',
            period_occurrences: 1,
            period_spent: 12,
            open_occurrence: null,
            subscriptions: [],
            cost_not_counted: ['a'],
          },
        });
      case 'responsibilities.pause':
        return Promise.resolve({ responsibility: { ...row, state: 'paused' } });
      default:
        return Promise.resolve({});
    }
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' });
  useAuthStore.setState({
    user: { id: 'u', email: 'a@b', display_name: 'a', role: 'admin', status: 'active' },
    bindings: [],
  });
});

describe('ResponsibilitiesPage (P5)', () => {
  it('says honestly when the feature is off, naming the keys, with no sample rows', async () => {
    mock({ enabled: false, rows: [] });
    renderPage();
    expect(await screen.findByTestId('responsibilities-off')).toHaveTextContent('[responsibilities] enabled = true');
    expect(screen.getByTestId('responsibilities-off')).toHaveTextContent('[dispatch] enabled = true');
    expect(screen.queryByTestId('responsibility-row')).toBeNull();
  });

  it('lists a row with state, read-only badge, spend vs cap and runs, and pauses it with its epoch', async () => {
    const calls: Array<[string, unknown]> = [];
    mock({ rows: [row], onCall: (m, p) => calls.push([m, p]) });
    renderPage();
    expect(await screen.findByText('Morning briefing')).toBeInTheDocument();
    expect(screen.getByText('Read-only')).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText('$0.12 / $0.50')).toBeInTheDocument());
    await userEvent.setup().click(screen.getByRole('button', { name: 'Pause' }));
    await waitFor(() =>
      expect(calls.find(([m]) => m === 'responsibilities.pause')?.[1]).toMatchObject({
        responsibility_id: 'r1',
        expected_control_epoch: 4,
      }),
    );
  });

  it('hides controls from a viewer-only binding', async () => {
    useAuthStore.setState({
      user: { id: 'u', email: 'a@b', display_name: 'a', role: 'employee', status: 'active' },
      bindings: [{ user_id: 'u', agent_name: 'alice', access_level: 'viewer', bound_at: '' }],
    });
    mock({ rows: [row] });
    renderPage();
    expect(await screen.findByText('Morning briefing')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Pause' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'New responsibility' })).toBeNull();
  });
});

describe('responsibility helpers', () => {
  it('reads the lane and the allowed controls', () => {
    expect(laneOf(row)).toBe('explore');
    expect(laneOf({ scope_json: '{"event_names":[]}' })).toBe('normal');
    expect(controlsFor('active')).toEqual(['pause', 'disable']);
    expect(controlsFor('failure_paused')).toEqual(['resume', 'disable']);
    expect(controlsFor('expired')).toEqual([]);
  });

  it('safe presets are always read-only, once per period', () => {
    const form = {
      ownerAgentId: 'alice',
      objective: ' brief ',
      acceptance: 'ok',
      time: '07:30',
      timezone: 'Asia/Taipei',
      readOnly: false,
      runCapCents: 40,
      periodCapCents: 10,
      stopAfterDays: 7,
    };
    const daily = buildInput('daily_briefing', form, new Date('2026-10-08T00:00:00Z'));
    expect(daily.lane).toBe('explore');
    expect(daily.schedule?.cron).toBe('0 30 7 * * *');
    expect(daily.period_occurrence_limit).toBe(1);
    expect(daily.period_cost_limit_cents).toBe(40);
    expect(daily.objective).toBe('brief');
    expect(buildInput('weekly_review', form).schedule?.cron).toBe('0 30 7 * * Mon');
    expect(buildInput('custom', form).lane).toBeUndefined();
    expect(buildInput('custom', { ...form, readOnly: true }).lane).toBe('explore');
  });
});
