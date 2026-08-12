import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route, useSearchParams } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { AgentDetailPage } from './AgentDetailPage';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';

const DETAIL = {
  name: 'my-bot',
  display_name: 'My Bot',
  status: 'active',
  role: 'main',
  trigger: '@bot',
  archived: false,
  avatar: null,
  department: '',
  skills: ['research', 'writing'],
  model: { preferred: 'claude-sonnet', api_mode: 'cli' },
  budget: { spent_cents: 120, monthly_limit_cents: 5000 },
  heartbeat: { enabled: true },
};

/** Probe for `/agents/:id/edit?tab=...` — echoes the `tab` query param so
 *  tests can assert which EditAgentPage sub-tab a CrossLink targets without
 *  mounting the real (heavy) edit form. */
function EditTabProbe() {
  const [params] = useSearchParams();
  return <div>edit-probe:{params.get('tab')}</div>;
}

function renderAt(id: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[`/agents/${id}`]}>
        <Routes>
          <Route path="/agents/:id" element={<AgentDetailPage />} />
          <Route path="/agents/:id/edit" element={<EditTabProbe />} />
          <Route path="/agents/:id/:tab" element={<AgentDetailPage />} />
          <Route path="/agents" element={<div>roster-probe</div>} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  // Combined payload: inspect reads the whole object; tasks.list / activity.list
  // pick their `.tasks` / `.events` fields off the same envelope.
  mockWsClient.call.mockResolvedValue({ ...DETAIL, tasks: [], events: [], agents: [] });
  useAgentsStore.setState({ agents: [], loading: false, loaded: true });
  useConnectionStore.setState({ state: 'authenticated' } as never);
});

describe('AgentDetailPage', () => {
  it('renders the hero header, breadcrumb, and tab strip', async () => {
    renderAt('my-bot');

    // Name renders in the hero once the inspect resolves.
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'My Bot' })).toBeInTheDocument();
    });
    // Breadcrumb root back to the roster.
    expect(screen.getAllByText('Agents').length).toBeGreaterThan(0);
    // Three-tab strip.
    expect(screen.getByRole('tab', { name: 'Overview' })).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: 'Work' })).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: 'Records' })).toBeInTheDocument();
  });

  it('shows the runtime-true GVU toggle on the overview summary card', async () => {
    mockWsClient.call.mockResolvedValue({
      ...DETAIL,
      evolution: { gvu_enabled: true, cognitive_memory: true, skill_auto_activate: true, skill_security_scan: true, max_silence_hours: 12 },
      tasks: [],
      events: [],
      agents: [],
    });
    renderAt('my-bot');
    await waitFor(() => {
      expect(screen.getByText('Self-evolution')).toBeInTheDocument();
    });
    // "Enabled" also appears for the heartbeat row — scope to the row text
    // being present at all rather than asserting a single unique match.
    expect(screen.getAllByText('Enabled').length).toBeGreaterThan(0);
  });

  // WP-C (§2-2 / R-EDIT-SOURCE): the spend/limit pair used to be dead text.
  it('makes the overview spend/limit open the budget settings', async () => {
    renderAt('my-bot');
    const budgetLink = await screen.findByRole('button', {
      name: /\$1\.20 \/ \$50\.00/,
    });
    expect(budgetLink).toHaveAttribute(
      'title',
      "Go to this staff member's budget settings",
    );
  });

  // X03 (UX audit §3.3): 模型/執行環境/心跳/自主進化 used to be dead text with
  // no route to the 腦袋與引擎 / 自動化 tabs that actually set them.
  it('opens the brain edit tab from the model row', async () => {
    const user = userEvent.setup();
    renderAt('my-bot');
    const link = await screen.findByRole('button', { name: 'claude-sonnet' });
    await user.click(link);
    await waitFor(() => {
      expect(screen.getByText('edit-probe:brain')).toBeInTheDocument();
    });
  });

  it('opens the brain edit tab from the runtime row', async () => {
    const user = userEvent.setup();
    renderAt('my-bot');
    // en.json → "agents.apiMode.cli": "Subscription account (CLI)"
    const link = await screen.findByRole('button', { name: 'Subscription account (CLI)' });
    await user.click(link);
    await waitFor(() => {
      expect(screen.getByText('edit-probe:brain')).toBeInTheDocument();
    });
  });

  it('opens the automation edit tab from the heartbeat row', async () => {
    const user = userEvent.setup();
    renderAt('my-bot');
    const link = await screen.findByRole('button', { name: 'Enabled' });
    await user.click(link);
    await waitFor(() => {
      expect(screen.getByText('edit-probe:automation')).toBeInTheDocument();
    });
  });

  it('opens the automation edit tab from the evolution row', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockResolvedValue({
      ...DETAIL,
      evolution: { gvu_enabled: true, cognitive_memory: true, skill_auto_activate: true, skill_security_scan: true, max_silence_hours: 12 },
      tasks: [],
      events: [],
      agents: [],
    });
    renderAt('my-bot');
    const link = await screen.findByRole('button', { name: 'Edit' });
    await user.click(link);
    await waitFor(() => {
      expect(screen.getByText('edit-probe:automation')).toBeInTheDocument();
    });
  });

  // A failed inspect used to render "this staff member could not be found",
  // which asserts something the page does not know (P05 Blocker, phase-4
  // audit). It must report the failure and offer a retry instead.
  it('reports a failed load as a retryable error, not as "not found"', async () => {
    mockWsClient.call.mockRejectedValue(new Error('nope'));
    renderAt('ghost');
    await waitFor(() => {
      expect(screen.getByRole('alert')).toBeInTheDocument();
    });
    expect(
      screen.getByRole('button', { name: 'Try again' }),
    ).toBeInTheDocument();
    expect(
      screen.queryByText('This staff member could not be found'),
    ).not.toBeInTheDocument();
    // The raw thrown message never reaches the page body.
    expect(screen.queryByText(/nope/)).not.toBeInTheDocument();
  });
});
