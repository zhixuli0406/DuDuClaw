import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { GalleryPage } from './GalleryPage';
import { useAssignStore } from '@/stores/assign-store';
import type { GalleryCard } from '@/lib/api';

function renderPage() {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={['/gallery']}>
        <Routes>
          <Route path="/gallery" element={<GalleryPage />} />
          <Route path="/experts" element={<div>experts-page-probe</div>} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

/** A not-yet-installed team's example card. */
const notInstalledCard: GalleryCard = {
  id: 'childcare-team-0',
  industry: 'childcare',
  category: 'education',
  departments: ['業務', '客服'],
  team_slug: 'childcare-team',
  team_label: '托嬰中心／幼兒園',
  example: '把這週未回覆的諮詢名單整理出來並排跟進順序',
  team_installed: false,
  lead_agent_name: null,
};

/** An installed team's example card. */
const installedCard: GalleryCard = {
  id: 'clinic-team-0',
  industry: 'clinic',
  category: 'health',
  departments: ['客服'],
  team_slug: 'clinic-team',
  team_label: '醫美／牙醫診所',
  example: '產出本月自費請款通知，逐筆核對系統金額',
  team_installed: true,
  lead_agent_name: 'clinic-assistant',
};

function mockGallery(res: {
  deployed: boolean;
  unlocked: boolean;
  present_but_locked: boolean;
  cards: GalleryCard[];
}) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'gallery.list') {
      return Promise.resolve(res);
    }
    return Promise.resolve({});
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  useAssignStore.setState({ open: false, prefill: null });
});

describe('GalleryPage — curated inspiration gallery (P2-b MVP)', () => {
  it('renders a card with title, team tag, outcome description and fit-for line — all sourced from real data, nothing fabricated', async () => {
    mockGallery({ deployed: true, unlocked: true, present_but_locked: false, cards: [notInstalledCard] });
    renderPage();

    // Title is derived from the example's first clause (here, the whole
    // string — no delimiter present).
    expect(await screen.findByText('把這週未回覆的諮詢名單整理出來並排跟進順序')).toBeInTheDocument();
    // Team tag.
    expect(screen.getByText('托嬰中心／幼兒園')).toBeInTheDocument();
    // Outcome description embeds the real example verbatim (a second,
    // differently-worded occurrence alongside the title).
    expect(
      screen.getByText('This team can help with: 把這週未回覆的諮詢名單整理出來並排跟進順序'),
    ).toBeInTheDocument();
    // Fit-for line derived from real departments, not invented copy.
    expect(screen.getByText(/業務/)).toBeInTheDocument();
  });

  it('shows "Add this AI team first" for an uninstalled team and navigates to /experts', async () => {
    mockGallery({ deployed: true, unlocked: true, present_but_locked: false, cards: [notInstalledCard] });
    renderPage();
    const user = userEvent.setup();

    const joinButton = await screen.findByRole('button', { name: /Add this AI team first/i });
    await user.click(joinButton);

    expect(await screen.findByText('experts-page-probe')).toBeInTheDocument();
  });

  it('opens the 交辦 panel prefilled from the example when the team is already installed', async () => {
    mockGallery({ deployed: true, unlocked: true, present_but_locked: false, cards: [installedCard] });
    renderPage();
    const user = userEvent.setup();

    const remakeButton = await screen.findByRole('button', { name: /Make my own version/i });
    await user.click(remakeButton);

    await waitFor(() => expect(useAssignStore.getState().open).toBe(true));
    const { prefill } = useAssignStore.getState();
    expect(prefill?.agentId).toBe('clinic-assistant');
    expect(prefill?.mode).toBe('assign');
    // Description is the example rewritten into an editable template, not the
    // bare example string verbatim.
    expect(prefill?.description).toContain('產出本月自費請款通知，逐筆核對系統金額');
    expect(prefill?.description).not.toBe(installedCard.example);
    // Acceptance criteria is outcome-shaped and references the same example.
    expect(prefill?.acceptanceCriteria).toContain('產出本月自費請款通知，逐筆核對系統金額');
  });

  it('shows the license upsell message when the gallery is deployed but locked', async () => {
    mockGallery({ deployed: true, unlocked: false, present_but_locked: true, cards: [] });
    renderPage();
    expect(await screen.findByText(/Pro license/i)).toBeInTheDocument();
  });

  it('shows an honest empty state when no premium tree ships with this install', async () => {
    mockGallery({ deployed: false, unlocked: false, present_but_locked: false, cards: [] });
    renderPage();
    expect(await screen.findByText(/doesn't ship inspiration-gallery examples/i)).toBeInTheDocument();
  });
});
