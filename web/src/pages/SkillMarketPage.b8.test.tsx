import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor, within, render } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, useLocation } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { useConnectionStore } from '@/stores/connection-store';
import { useAuthStore } from '@/stores/auth-store';
import { SkillMarketPage } from './SkillMarketPage';

/** B8 — ordering, search, tab in the URL, and per-employee management. */

let lastSearch = '';
function LocationProbe() {
  lastSearch = useLocation().search;
  return null;
}

function renderAt(url = '/skills') {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[url]}>
        <SkillMarketPage />
        <LocationProbe />
      </MemoryRouter>
    </IntlProvider>,
  );
}

const twoAgents = [
  { name: 'bruno', display_name: 'Bruno' },
  { name: 'agnes', display_name: 'agnes' },
];

/** Two employees each with an own `quote`, a company-wide `Tone`, and `alpha`. */
function aggregate() {
  return {
    global_skills: [
      {
        name: 'Tone',
        content: '',
        scope: 'global',
        description: 'company voice',
        use_count: 3,
        enabled: true,
        enabled_count: 2,
        holders: [
          { agent_id: 'agnes', enabled: true, use_count: 0 },
          { agent_id: 'bruno', enabled: true, use_count: 3, last_used_at: '2026-10-20T00:00:00Z' },
        ],
      },
    ],
    agents: [
      {
        agent_id: 'agnes',
        can_manage: true,
        skill_state: { ok: true },
        skills: [{ name: 'quote', content: '', scope: 'agent', agent_id: 'agnes', enabled: true, description: 'price list', security_status: 'pass' }],
      },
      {
        agent_id: 'bruno',
        can_manage: true,
        skill_state: { ok: true },
        skills: [
          { name: 'quote', content: '', scope: 'agent', agent_id: 'bruno', enabled: false, tags: ['sales'] },
          { name: 'alpha', content: '', scope: 'agent', agent_id: 'bruno', enabled: true },
        ],
      },
    ],
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.subscribe.mockImplementation(() => () => undefined);
  useConnectionStore.setState({ state: 'authenticated' });
  useAuthStore.setState({ user: { id: 'u1', display_name: 'Owner', role: 'admin' } } as never);
  try {
    localStorage.clear();
  } catch {
    /* jsdom */
  }
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'agents.list') return Promise.resolve({ agents: twoAgents });
    if (method === 'skills.list') return Promise.resolve(aggregate());
    if (method === 'skills.shared')
      return Promise.resolve({
        skills: [
          { name: 'zeta', description: 'handles refunds', shared_by: 'agnes', shared_at: '2026-10-01', adopted_by: [], usage_count: 1, tags: [] },
          { name: 'Beta', description: 'greeting', shared_by: 'bruno', shared_at: '2026-10-02', adopted_by: [], usage_count: 5, tags: ['front-desk'] },
        ],
      });
    if (method === 'skills.set_enabled' || method === 'skills.remove')
      return Promise.resolve({ results: [{ agent_id: 'bruno', skill_name: 'quote', ok: true }] });
    return Promise.resolve({});
  });
});

/** Row names in the order they are rendered. */
function rowNames(): string[] {
  return screen
    .getAllByRole('row')
    .map((r) => r.querySelector('span[title]')?.textContent ?? '')
    .filter(Boolean);
}

describe('SkillMarketPage — B8', () => {
  it('opens the tab named in ?tab= and writes the tab back to the URL', async () => {
    const user = userEvent.setup();
    renderAt('/skills?tab=shared');
    await waitFor(() => expect(screen.getByText('zeta')).toBeInTheDocument());
    expect(screen.getByRole('radio', { name: 'Team Skills' })).toBeChecked();
    await user.click(screen.getByRole('radio', { name: 'My Skills' }));
    expect(lastSearch).toBe('?tab=mySkills');
    await user.click(screen.getByRole('radio', { name: 'Market' }));
    expect(lastSearch).toBe('');
  });

  it('searches team skills by description and sorts them by name', async () => {
    const user = userEvent.setup();
    renderAt('/skills?tab=shared');
    await waitFor(() => expect(screen.getByText('zeta')).toBeInTheDocument());
    const headings = screen.getAllByRole('heading', { level: 3 }).map((h) => h.textContent);
    expect(headings).toEqual(['Beta', 'zeta']);
    await user.type(screen.getByPlaceholderText('Search team skills…'), 'refund');
    expect(screen.queryByText('Beta')).not.toBeInTheDocument();
    expect(screen.getByText('zeta')).toBeInTheDocument();
  });

  it('lists same-named skills of two employees as separate rows, ordered case-insensitively', async () => {
    renderAt('/skills?tab=mySkills');
    await waitFor(() => expect(screen.getAllByText('quote')).toHaveLength(2));
    expect(rowNames()).toEqual(['alpha', 'quote', 'quote', 'Tone']);
    // Bruno's copy is switched off, Agnes' is not.
    expect(screen.getAllByText('Off')).toHaveLength(1);
    expect(screen.getByText('2 of 2 staff using it')).toBeInTheDocument();
  });

  it('filters installed skills by tag and description', async () => {
    const user = userEvent.setup();
    renderAt('/skills?tab=mySkills');
    await waitFor(() => expect(screen.getAllByText('quote')).toHaveLength(2));
    await user.type(screen.getByPlaceholderText('Filter my skills…'), 'sales');
    expect(rowNames()).toEqual(['quote']);
    await user.clear(screen.getByPlaceholderText('Filter my skills…'));
    await user.type(screen.getByPlaceholderText('Filter my skills…'), 'company voice');
    expect(rowNames()).toEqual(['Tone']);
  });

  it('remembers the chosen sort per viewer', async () => {
    const user = userEvent.setup();
    renderAt('/skills?tab=mySkills');
    await waitFor(() => expect(screen.getAllByText('quote')).toHaveLength(2));
    await user.click(screen.getByRole('combobox', { name: 'Sort' }));
    await user.click(await screen.findByRole('option', { name: 'Most used' }));
    await waitFor(() => expect(rowNames()[0]).toBe('Tone'));
    expect(localStorage.getItem('duduclaw.skills.sort.installed')).toBe('mostUsed');
  });

  it('switches a company-wide skill off for every holder from its row', async () => {
    const user = userEvent.setup();
    renderAt('/skills?tab=mySkills');
    await waitFor(() => expect(screen.getAllByText('quote')).toHaveLength(2));
    const toneRow = screen.getAllByRole('row').find((r) => within(r).queryByText('Tone'))!;
    await user.click(within(toneRow).getByRole('button', { name: 'More actions' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Switch off for every staffer' }));
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('skills.set_enabled', {
        enabled: false,
        items: [
          { agent_id: 'agnes', skill_name: 'Tone' },
          { agent_id: 'bruno', skill_name: 'Tone' },
        ],
      }),
    );
  });

  it('bulk-removes only employees’ own skills after confirmation', async () => {
    const user = userEvent.setup();
    renderAt('/skills?tab=mySkills');
    await waitFor(() => expect(screen.getAllByText('quote')).toHaveLength(2));
    await user.click(screen.getByRole('checkbox', { name: 'Select all' }));
    await user.click(screen.getByRole('button', { name: 'Remove (3)' }));
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('button', { name: 'Remove' }));
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('skills.remove', {
        items: [
          { agent_id: 'bruno', skill_name: 'alpha' },
          { agent_id: 'agnes', skill_name: 'quote' },
          { agent_id: 'bruno', skill_name: 'quote' },
        ],
      }),
    );
  });

  it('hides bulk selection from non-admins', async () => {
    useAuthStore.setState({ user: { id: 'u2', display_name: 'Emp', role: 'employee' } } as never);
    renderAt('/skills?tab=mySkills');
    await waitFor(() => expect(screen.getAllByText('quote')).toHaveLength(2));
    expect(screen.queryByRole('checkbox', { name: 'Select all' })).not.toBeInTheDocument();
  });

  it('shows an unreadable switch file loudly', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'agents.list') return Promise.resolve({ agents: twoAgents });
      if (method === 'skills.list') {
        const a = aggregate();
        a.agents[0]!.skill_state = { ok: false, error: 'not valid JSON' } as never;
        return Promise.resolve(a);
      }
      return Promise.resolve({});
    });
    renderAt('/skills?tab=mySkills');
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('loads no skills'));
    expect(screen.getByText('not valid JSON')).toBeInTheDocument();
  });
});
