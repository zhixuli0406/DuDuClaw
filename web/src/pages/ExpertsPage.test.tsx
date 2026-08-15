import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { ExpertsPage } from './ExpertsPage';
import { useAgentsStore } from '@/stores/agents-store';
import type { ExpertCatalogEntry } from '@/lib/api';

function renderPage() {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={['/experts']}>
        <Routes>
          <Route path="/experts" element={<ExpertsPage />} />
          <Route path="/agents/:name" element={<div>agent-detail-probe</div>} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

/** A not-yet-installed team catalog entry (childcare-team style fixture —
 *  mirrors the real `team.toml` shape asserted server-side in
 *  `expert_generate.rs`'s `catalog_team_members_humans_excluded_and_examples_fallback`). */
const teamEntry: ExpertCatalogEntry = {
  kind: 'team',
  industry: 'childcare',
  category: 'education',
  departments: ['業務', '客服'],
  label: '托嬰中心／幼兒園',
  slug: 'childcare-team',
  description: '家長單一窗口，分診委派、兒虐線索進線即硬中斷、合規措辭把關',
  agents_count: 3,
  installed: false,
  members: [
    {
      role: 'front_desk',
      name: 'childcare-assistant',
      display_name: '托嬰幼兒園招生助理',
      summary: '接待家長、分派工作給部門同事',
    },
    {
      role: 'worker',
      name: 'cc-sales',
      display_name: '業務跟進助理',
      summary: '招生諮詢跟進、參觀預約草擬',
    },
  ],
  humans: [
    { title: '帶班老師／托育人員', summary: '本崗位不建 AI，由真人擔任' },
    { title: '園長', summary: '本崗位不建 AI，由真人擔任' },
  ],
  excluded: [],
  examples: ['把這週未回覆的諮詢名單整理出來並排跟進順序', '產出本月自費請款通知，逐筆核對系統金額'],
  lead_agent_name: null,
};

const installedEntry: ExpertCatalogEntry = {
  ...teamEntry,
  industry: 'clinic',
  category: 'health',
  slug: 'clinic-team',
  label: '醫美／牙醫診所',
  installed: true,
  lead_agent_name: 'clinic-assistant',
};

function mockCatalog(packs: ExpertCatalogEntry[]) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'experts.list') {
      return Promise.resolve({ packs: [] });
    }
    if (method === 'experts.catalog') {
      return Promise.resolve({ deployed: true, unlocked: true, present_but_locked: false, packs });
    }
    if (method === 'experts.install_builtin') {
      return Promise.resolve({ success: true, slug: 'childcare-team', output: '' });
    }
    return Promise.resolve({});
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  useAgentsStore.setState({
    agents: [],
    loading: false,
    loaded: true,
    fetchAgents: vi.fn().mockResolvedValue(undefined),
  } as never);
});

describe('ExpertsPage — built-in AI team catalog cards (P2-a)', () => {
  it('renders team composition: AI members with their one-line duty, and positions kept human', async () => {
    mockCatalog([teamEntry]);
    renderPage();

    expect(await screen.findByText('托嬰中心／幼兒園')).toBeInTheDocument();

    // AI roster — front desk + worker, name + duty summary, both real
    // team.toml strings verbatim (never re-authored client-side).
    expect(screen.getByText('托嬰幼兒園招生助理')).toBeInTheDocument();
    expect(screen.getByText(/接待家長、分派工作給部門同事/)).toBeInTheDocument();
    expect(screen.getByText('業務跟進助理')).toBeInTheDocument();
    expect(screen.getByText(/招生諮詢跟進、參觀預約草擬/)).toBeInTheDocument();

    // Honest "left to a human" disclosure — titles surfaced, not silently
    // dropped from the roster.
    expect(screen.getByText(/帶班老師／托育人員/)).toBeInTheDocument();
    expect(screen.getByText(/園長/)).toBeInTheDocument();

    // Task examples — real strings, not fabricated at render time.
    expect(screen.getByText('把這週未回覆的諮詢名單整理出來並排跟進順序')).toBeInTheDocument();
    expect(screen.getByText('產出本月自費請款通知，逐筆核對系統金額')).toBeInTheDocument();
  });

  it('shows the installed state with an entry point into the created lead agent, not just a dead badge', async () => {
    mockCatalog([installedEntry]);
    renderPage();

    expect(await screen.findByText('醫美／牙醫診所')).toBeInTheDocument();
    expect(screen.getByText('Added')).toBeInTheDocument();

    const viewButton = screen.getByRole('button', { name: /View team/i });
    const user = userEvent.setup();
    await user.click(viewButton);

    // Navigates straight to the actually-created agent id, not a guess.
    expect(await screen.findByText('agent-detail-probe')).toBeInTheDocument();
  });

  it('summons a team via experts.install_builtin from the card CTA', async () => {
    mockCatalog([teamEntry]);
    renderPage();
    const user = userEvent.setup();

    expect(await screen.findByText('托嬰中心／幼兒園')).toBeInTheDocument();

    // User-facing verb, not "Install" — team kind gets the team-flavored copy.
    const summonButton = screen.getByRole('button', { name: /Summon this team/i });
    await user.click(summonButton);

    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('Summon "托嬰中心／幼兒園"');

    const confirmButton = screen.getByRole('button', { name: 'Add to my team' });
    await user.click(confirmButton);

    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith(
        'experts.install_builtin',
        expect.objectContaining({ industry: 'childcare' }),
        expect.anything(),
        expect.anything(),
      ),
    );
  });
});
