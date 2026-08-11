import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route, useSearchParams } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useSystemStore } from '@/stores/system-store';
import { MemoryPage } from './MemoryPage';

/** Point the edition gate at one profile for the duration of a test. */
function setEdition(profile: 'personal' | 'enterprise') {
  useSystemStore.setState({
    status: { edition_profile: profile },
  } as Parameters<typeof useSystemStore.setState>[0]);
}

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
  setEdition('enterprise');
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('MemoryPage', () => {
  it('renders the collection header', () => {
    renderWithProviders(<MemoryPage />);
    expect(screen.getByRole('heading', { name: 'Memory' })).toBeInTheDocument();
  });

  it('renders memory, knowledge and learning segments (enterprise)', () => {
    renderWithProviders(<MemoryPage />);
    expect(screen.getByRole('radio', { name: 'Memories' })).toBeInTheDocument();
    // The knowledge base merged in here (2026-07-30 client feedback).
    expect(screen.getByRole('radio', { name: 'Personal' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Shared' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Key Insights' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Self-Improvement' })).toBeInTheDocument();
  });

  it('collapses the two knowledge bases into one tab on Personal', () => {
    setEdition('personal');
    renderWithProviders(<MemoryPage />);
    expect(screen.getByRole('radio', { name: 'Knowledge base' })).toBeInTheDocument();
    expect(screen.queryByRole('radio', { name: 'Shared' })).not.toBeInTheDocument();
    expect(screen.queryByRole('radio', { name: 'Personal' })).not.toBeInTheDocument();
  });

  // Evolution v3 dashboard convergence: stagnation watch, rejection
  // distribution, and the Playbook section only render once an agent is
  // selected (the picker defaults to the first agent from `agents.list`).
  it('evolution tab surfaces stagnation health and the playbook section for the selected agent', async () => {
    mockWsClient.call.mockImplementation((method: unknown) => {
      switch (method) {
        case 'agents.list':
          return Promise.resolve({ agents: [{ name: 'agent-a', display_name: 'Agent A' }] });
        case 'evolution.status':
          return Promise.resolve({
            enabled: true,
            mode: 'prediction_driven',
            total_agents: 1,
            gvu_enabled_count: 1,
            total_versions: 0,
            last_applied_at: null,
            agents: [{
              agent_id: 'agent-a',
              gvu_enabled: true,
              cognitive_memory: true,
              skill_auto_activate: true,
              skill_security_scan: true,
              max_silence_hours: 12,
              max_gvu_generations: 3,
              observation_period_hours: 24,
            }],
          });
        case 'evolution.versions':
          return Promise.resolve({ versions: [] });
        case 'evolution.stagnation':
          return Promise.resolve({
            snapshots: [{ agent_id: 'agent-a', is_stagnant: false, signals: [], summary: null, checked_at: '' }],
          });
        case 'evolution.telemetry':
          return Promise.resolve({ agent_id: 'agent-a', days: 7, total: 0, by_stage_layer: {} });
        case 'evolution.consolidations':
          return Promise.resolve({ consolidations: [] });
        case 'playbook.list':
          return Promise.resolve({ agent_id: 'agent-a', entries: [] });
        default:
          return Promise.resolve({});
      }
    });

    const user = userEvent.setup();
    renderWithProviders(<MemoryPage />);
    await user.click(screen.getByRole('radio', { name: 'Self-Improvement' }));

    await waitFor(() => {
      expect(screen.getByText('Evolution loop health')).toBeInTheDocument();
    });
    expect(await screen.findByText('Running normally — no stagnation signal')).toBeInTheDocument();
    expect(await screen.findByText('Rejection distribution')).toBeInTheDocument();
    expect(await screen.findByText('Experience rules')).toBeInTheDocument();
    expect(
      screen.getByText("This AI staff member hasn't learned any rules from experience yet"),
    ).toBeInTheDocument();
  });

  // W3-2 (D-O3 = A): the card leads with the gateway's plain-language rewrite,
  // folds the model-facing text away, and never shows an internal term.
  it('experience rule cards lead with plain language and keep the raw wording collapsed', async () => {
    mockWsClient.call.mockImplementation((method: unknown) => {
      switch (method) {
        case 'agents.list':
          return Promise.resolve({ agents: [{ name: 'agent-a', display_name: 'Agent A' }] });
        case 'evolution.status':
          return Promise.resolve({ enabled: true, mode: 'prediction_driven', total_agents: 1, gvu_enabled_count: 1, total_versions: 0, last_applied_at: null, agents: [] });
        case 'evolution.versions':
          return Promise.resolve({ versions: [] });
        case 'evolution.stagnation':
          return Promise.resolve({ snapshots: [] });
        case 'evolution.telemetry':
          return Promise.resolve({ agent_id: 'agent-a', days: 7, total: 0, by_stage_layer: {} });
        case 'evolution.consolidations':
          return Promise.resolve({ consolidations: [] });
        case 'playbook.list':
          return Promise.resolve({
            agent_id: 'agent-a',
            entries: [{
              id: 'e1',
              content: 'RAW-MODEL-FACING-TEXT',
              category: 'repair',
              state: 'probation',
              signals_match: ['mistake:capability'],
              eval_cases: ['suite/case'],
              success_streak: 2,
              revision: 0,
              helpful: 3,
              harmful: 1,
              net_score: 2,
              origin: 'agent_derived',
              created_at: new Date().toISOString(),
              humanized: {
                sentence: 'PLAIN-LANGUAGE-SENTENCE',
                condition: 'X',
                action: 'Y',
                purpose: 'Z',
                purpose_key: 'repair',
                status: '試用中',
                status_key: 'trial',
                why: 'PROVENANCE-SENTENCE',
                fallback: false,
                evidence: { eval_cases: 1, failure_notes: 2, applications: 4, helpful: 3, harmful: 1, success_streak: 2 },
              },
            }],
          });
        default:
          return Promise.resolve({});
      }
    });

    const user = userEvent.setup();
    renderWithProviders(<MemoryPage />);
    await user.click(screen.getByRole('radio', { name: 'Self-Improvement' }));

    expect(await screen.findByText('PLAIN-LANGUAGE-SENTENCE')).toBeInTheDocument();
    // 「為什麼有這條」 is always visible — it is the whole point of the card.
    expect(screen.getByText('PROVENANCE-SENTENCE')).toBeInTheDocument();
    // Status badge uses the §C.9 vocabulary keyed off `status_key`.
    expect(screen.getByText('On trial')).toBeInTheDocument();
    expect(screen.getByText('2 related failures on record')).toBeInTheDocument();
    // The raw text is present but behind a disclosure, and the machine-token
    // trigger list is gone from the card entirely.
    expect(screen.getByText('Show the original wording')).toBeInTheDocument();
    expect(screen.queryByText('mistake:capability')).not.toBeInTheDocument();
  });

  it('falls back to the raw wording, clearly labelled, when it cannot be rephrased', async () => {
    mockWsClient.call.mockImplementation((method: unknown) => {
      switch (method) {
        case 'agents.list':
          return Promise.resolve({ agents: [{ name: 'agent-a', display_name: 'Agent A' }] });
        case 'evolution.status':
          return Promise.resolve({ enabled: true, mode: 'prediction_driven', total_agents: 1, gvu_enabled_count: 1, total_versions: 0, last_applied_at: null, agents: [] });
        case 'evolution.versions':
          return Promise.resolve({ versions: [] });
        case 'evolution.stagnation':
          return Promise.resolve({ snapshots: [] });
        case 'evolution.telemetry':
          return Promise.resolve({ agent_id: 'agent-a', days: 7, total: 0, by_stage_layer: {} });
        case 'evolution.consolidations':
          return Promise.resolve({ consolidations: [] });
        case 'playbook.list':
          return Promise.resolve({
            agent_id: 'agent-a',
            entries: [{
              id: 'e2', content: 'UNREPHRASABLE', category: 'innovate', state: 'active',
              signals_match: [], eval_cases: [], success_streak: 0, revision: 0,
              helpful: 0, harmful: 0, net_score: 0, origin: 'agent_derived',
              created_at: new Date().toISOString(),
              humanized: {
                sentence: 'UNREPHRASABLE', condition: '', action: '', purpose: '',
                purpose_key: 'innovate', status: '生效中', status_key: 'active',
                why: 'WHY-TEXT', fallback: true,
                evidence: { eval_cases: 0, failure_notes: 0, applications: 0, helpful: 0, harmful: 0, success_streak: 0 },
              },
            }],
          });
        default:
          return Promise.resolve({});
      }
    });

    const user = userEvent.setup();
    renderWithProviders(<MemoryPage />);
    await user.click(screen.getByRole('radio', { name: 'Self-Improvement' }));

    expect(
      await screen.findByText(
        "This one can't be rephrased automatically yet — original wording below",
      ),
    ).toBeInTheDocument();
    expect(screen.getByText('UNREPHRASABLE')).toBeInTheDocument();
    // No disclosure when the visible text already IS the raw wording.
    expect(screen.queryByText('Show the original wording')).not.toBeInTheDocument();
  });
});

// ── W3-3: state-as-URL (Stripe pattern B4) — the memories search term is
// bookmarkable/shareable via `?q=`. Scoped to the `memories` tab only — the
// `evolution` tab above is out of bounds for this change.

function SearchParamsProbe() {
  const [params] = useSearchParams();
  return <div data-testid="search-probe">{params.toString()}</div>;
}

/** `renderWithProviders`'s router has no way to seed a starting query string. */
function renderAt(path: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <Routes>
          <Route
            path="/memory"
            element={
              <>
                <MemoryPage />
                <SearchParamsProbe />
              </>
            }
          />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

describe('MemoryPage — search term as URL (W3-3)', () => {
  it('seeds the search box from ?q= on load', () => {
    renderAt('/memory?tab=memories&q=contract');
    expect(screen.getByPlaceholderText('Search memories...')).toHaveValue('contract');
  });

  it('mirrors a typed search term back into the URL without disturbing ?tab=', async () => {
    renderAt('/memory?tab=memories');
    const input = screen.getByPlaceholderText('Search memories...');
    fireEvent.change(input, { target: { value: 'invoice' } });

    await waitFor(() => {
      const probe = screen.getByTestId('search-probe').textContent ?? '';
      expect(probe).toContain('q=invoice');
      expect(probe).toContain('tab=memories');
    });
  });

  it('clears the "q" param once the search box is emptied', async () => {
    renderAt('/memory?tab=memories&q=invoice');
    await waitFor(() => {
      expect(screen.getByTestId('search-probe')).toHaveTextContent('q=invoice');
    });

    fireEvent.change(screen.getByPlaceholderText('Search memories...'), { target: { value: '' } });

    await waitFor(() => {
      expect(screen.getByTestId('search-probe')).not.toHaveTextContent('q=');
    });
  });
});
