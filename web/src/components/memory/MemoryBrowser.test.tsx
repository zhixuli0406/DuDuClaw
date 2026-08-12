import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, within, act } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route, useSearchParams } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { MemoryBrowser } from './MemoryBrowser';

/** Route each RPC this component issues to a canned response. `signals` is the
 *  gateway's WP15 second list — platform learning telemetry, already split out
 *  of `entries` server-side. */
function mockCalls(entries: unknown[], signals: unknown[] = []) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'memory.browse') return Promise.resolve({ entries, signals });
    if (method === 'memory.forget') return Promise.resolve({ success: true, forgotten: true });
    if (method === 'memory.history') return Promise.resolve({ subject: '', predicate: '', current_id: null, chain: [] });
    return Promise.resolve({});
  });
}

/** One prediction-deviation row, exactly as the router writes it — legacy
 *  shape, written before the fix, with no conversation context at all. */
const SIGNAL = {
  id: 'p1',
  agent_id: 'agnes',
  content:
    'Prediction deviation: expected satisfaction 0.70, inferred 0.52 (delta 0.18). Topic surprise: 1.00. Corrections: yes. Follow-ups: no.',
  timestamp: '2026-08-03T12:00:00Z',
  tags: [],
  layer: 'episodic',
  source_event: 'prediction_episodic',
};

/** Same telemetry, current backend shape: carries the conversation's topics. */
const SIGNAL_WITH_TOPICS = {
  ...SIGNAL,
  id: 'p3',
  content: `${SIGNAL.content} Topics: 報價, 合約.`,
};

const ENTRIES = [
  {
    id: 'm1',
    agent_id: 'agnes',
    content: '客戶的合約報價是三萬元',
    timestamp: '2026-08-01T10:00:00Z',
    tags: [],
    layer: 'semantic',
  },
  {
    id: 'm2',
    agent_id: 'agnes',
    content: '老闆喜歡簡短的回覆語氣',
    timestamp: '2026-08-03T10:00:00Z',
    tags: [],
    layer: 'episodic',
    source_event: 'user_profile',
    access_count: 4,
  },
  {
    id: 'm3',
    agent_id: 'agnes',
    content: '2026-07-29 活躍時段 Top3（UTC 小時）：01:00、05:00、07:00',
    timestamp: '2026-08-02T10:00:00Z',
    tags: ['footprint-distill'],
    source_event: 'footprint_distill',
  },
];

/** Handlers registered via `client.subscribe`, keyed by event name — lets a
 *  test fire a server push the way the gateway would. */
const wsHandlers = new Map<string, (payload: unknown) => void>();

beforeEach(() => {
  vi.clearAllMocks();
  wsHandlers.clear();
  mockCalls(ENTRIES);
  mockWsClient.subscribe.mockImplementation((event: string, handler: (p: unknown) => void) => {
    wsHandlers.set(event, handler);
    return () => wsHandlers.delete(event);
  });
  // M3: the browser now reads only while the socket is authenticated.
  useConnectionStore.setState({ state: 'authenticated' });
});

describe('MemoryBrowser', () => {
  it('lists every memory in one recency-ordered list', async () => {
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    // The default view is the flat list, newest first — no per-category cards.
    await screen.findByText(/Recent memories/);
    const rows = screen.getAllByRole('button', { expanded: false });
    const summaries = rows.map((r) => r.textContent ?? '');
    expect(summaries[0]).toContain('老闆喜歡簡短的回覆語氣');
    expect(summaries[1]).toContain('活躍時段');
    expect(summaries[2]).toContain('客戶的合約報價是三萬元');
  });

  it('summarizes a long memory on the row and keeps the full text for the detail', async () => {
    const user = userEvent.setup();
    const long = `${'客戶的合約報價是三萬元，付款條件為簽約後三十天內付清。'.repeat(4)}逾期則加計利息。`;
    mockCalls([
      { id: 'L1', agent_id: 'agnes', content: long, timestamp: '2026-08-03T10:00:00Z', tags: [] },
    ]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    // The row carries a shortened summary — never the whole blob.
    const row = await screen.findByRole('button', { expanded: false });
    expect(row.textContent?.length ?? 0).toBeLessThan(long.length);
    expect(screen.queryByText(long)).not.toBeInTheDocument();

    // Expanding reveals the memory verbatim.
    await user.click(row);
    expect(screen.getByText(long)).toBeInTheDocument();
  });

  it('groups entries into topic categories with counts', async () => {
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    // The rail lists every non-empty category plus an "All" entry.
    expect(await screen.findByRole('button', { name: /All\s*3/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Customers & deals\s*1/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Preferences & habits\s*1/ })).toBeInTheDocument();
    // Origin beats keywords: the footprint roll-up lands in its own bucket.
    expect(screen.getByRole('button', { name: /Usage footprint\s*1/ })).toBeInTheDocument();
    // Categories with nothing in them are not rendered at all.
    expect(screen.queryByRole('button', { name: /Costs & billing/ })).not.toBeInTheDocument();
  });

  it('drills into one category from the rail', async () => {
    const user = userEvent.setup();
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    await user.click(await screen.findByRole('button', { name: /Customers & deals\s*1/ }));

    expect(screen.getByText('客戶的合約報價是三萬元')).toBeInTheDocument();
    expect(screen.queryByText('老闆喜歡簡短的回覆語氣')).not.toBeInTheDocument();
  });

  it('expands a row into the full text and its provenance', async () => {
    const user = userEvent.setup();
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    await user.click(await screen.findByText('老闆喜歡簡短的回覆語氣'));

    // Layer, origin and recall count are all rendered in end-user words.
    expect(screen.getByText('Kind')).toBeInTheDocument();
    expect(screen.getAllByText('From recent conversations').length).toBeGreaterThan(0);
    expect(screen.getByText('A preference or name you mentioned')).toBeInTheDocument();
    expect(screen.getByText('4 times')).toBeInTheDocument();
    // The supersession chain is fetched lazily, only once expanded.
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('memory.history', {
        agent_id: 'agnes',
        memory_id: 'm2',
      });
    });
  });

  it('never renders an edit affordance', async () => {
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);
    await screen.findByText('老闆喜歡簡短的回覆語氣');
    expect(screen.queryByRole('button', { name: /edit/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
  });

  it('deletes a memory only after a confirm click', async () => {
    const user = userEvent.setup();
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    await user.click(await screen.findByRole('button', { name: /Preferences & habits\s*1/ }));
    await user.click(screen.getByRole('button', { name: 'Delete this memory' }));

    // First click only arms the confirm — nothing has been sent yet.
    expect(mockWsClient.call).not.toHaveBeenCalledWith('memory.forget', expect.anything());

    await user.click(screen.getByRole('button', { name: 'Delete' }));

    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('memory.forget', {
        agent_id: 'agnes',
        memory_id: 'm2',
      });
    });
    // The row leaves the list without a refetch.
    await waitFor(() => {
      expect(screen.queryByText('老闆喜歡簡短的回覆語氣')).not.toBeInTheDocument();
    });
  });

  it('explains how memories get created when there are none', async () => {
    mockCalls([]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    expect(await screen.findByText('Nothing remembered yet')).toBeInTheDocument();
    expect(screen.getByText(/gets noted here automatically/)).toBeInTheDocument();
    // WP5b — no "go enable cognitive memory" call to action any more, and the
    // per-agent flag is not even looked up.
    expect(screen.queryByRole('link', { name: /enable/i })).not.toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('agents.inspect', expect.anything());
  });

  it('shows a search-specific empty state', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'memory.search') return Promise.resolve({ entries: [] });
      if (method === 'memory.browse') return Promise.resolve({ entries: [] });
      return Promise.resolve({});
    });
    renderWithProviders(<MemoryBrowser agentId="agnes" query="nothing-here" />);

    expect(await screen.findByText(/No memory matches "nothing-here"/)).toBeInTheDocument();
  });

  // ── WP15: learning telemetry lives in its own section ──────────────────

  it('keeps learning signals out of the memory list', async () => {
    mockCalls(ENTRIES, [SIGNAL]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    // The main list is exactly the three user memories — the telemetry row is
    // nowhere in it, collapsed or otherwise.
    const list = (await screen.findByText(/Recent memories/)).parentElement as HTMLElement;
    expect(within(list).getAllByRole('button', { expanded: false })).toHaveLength(3);
    expect(within(list).queryByText(/Learning signal/)).not.toBeInTheDocument();
    // And the rail — which counts the same list — never grows a signals bucket.
    expect(await screen.findByRole('button', { name: /All\s*3/ })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Learning signals\s*\d/ })).not.toBeInTheDocument();
  });

  it('files learning signals under a collapsed system section', async () => {
    const user = userEvent.setup();
    mockCalls(ENTRIES, [SIGNAL]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    // Collapsed by default: the count and the plain-words explanation show,
    // the rows do not.
    const toggle = await screen.findByRole('button', { name: /System learning log\s*1/ });
    expect(screen.getByText(/Not things you told me/)).toBeInTheDocument();
    expect(screen.queryByText(/Learning signal · 70% → 52%/)).not.toBeInTheDocument();

    await user.click(toggle);
    const row = screen.getByText(/Learning signal · 70% → 52%/);
    // Even expanded, the raw English telemetry is never shown verbatim.
    expect(screen.queryByText(/Prediction deviation:/)).not.toBeInTheDocument();

    await user.click(row);
    const detail = screen.getByText('Self-check on how a reply landed').closest('dl');
    expect(detail).not.toBeNull();
    expect(within(detail as HTMLElement).getByText('Self-check on how a reply landed')).toBeInTheDocument();
    expect(screen.getByText(/You corrected me/i)).toBeInTheDocument();
  });

  it('keeps the split while searching', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'memory.search')
        return Promise.resolve({ entries: [ENTRIES[0]], signals: [SIGNAL] });
      if (method === 'memory.browse') return Promise.resolve({ entries: [], signals: [] });
      return Promise.resolve({});
    });
    renderWithProviders(<MemoryBrowser agentId="agnes" query="合約" />);

    // A search result set is partitioned the same way a browse is.
    const list = (await screen.findByText(/Recent memories/)).parentElement as HTMLElement;
    expect(within(list).getByText('客戶的合約報價是三萬元')).toBeInTheDocument();
    expect(within(list).queryByText(/Learning signal/)).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: /System learning log\s*1/ })).toBeInTheDocument();
  });

  // ── Bug fix: the learning-signal card carries no conversation context ──

  it('appends the first topic to the row summary when the signal carries one', async () => {
    const user = userEvent.setup();
    mockCalls(ENTRIES, [SIGNAL_WITH_TOPICS]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    const toggle = await screen.findByRole('button', { name: /System learning log\s*1/ });
    await user.click(toggle);
    expect(screen.getByText(/Learning signal · 70% → 52% · 報價/)).toBeInTheDocument();
  });

  it('does not append a topic to the row summary for a legacy signal with none', async () => {
    const user = userEvent.setup();
    mockCalls(ENTRIES, [SIGNAL]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    const toggle = await screen.findByRole('button', { name: /System learning log\s*1/ });
    await user.click(toggle);
    expect(screen.getByText('Learning signal · 70% → 52%')).toBeInTheDocument();
  });

  it('shows the topics badge in the expanded detail card', async () => {
    const user = userEvent.setup();
    mockCalls(ENTRIES, [SIGNAL_WITH_TOPICS]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    const toggle = await screen.findByRole('button', { name: /System learning log\s*1/ });
    await user.click(toggle);
    await user.click(screen.getByText(/Learning signal · 70% → 52% · 報價/));

    expect(screen.getByText('Topics: 報價, 合約')).toBeInTheDocument();
  });

  it('renders no topics badge for a legacy signal with none', async () => {
    const user = userEvent.setup();
    mockCalls(ENTRIES, [SIGNAL]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    const toggle = await screen.findByRole('button', { name: /System learning log\s*1/ });
    await user.click(toggle);
    await user.click(screen.getByText('Learning signal · 70% → 52%'));

    expect(screen.queryByText(/^Topics:/)).not.toBeInTheDocument();
  });

  it('hides the section entirely when there are no signals', async () => {
    mockCalls(ENTRIES, []);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);
    await screen.findByText(/Recent memories/);
    expect(screen.queryByText('System learning log')).not.toBeInTheDocument();
  });

  // The client's exact question — "記憶還沒更新對嗎?" — with telemetry piling up
  // and nothing remembered. Both halves of the answer have to be on screen.
  it('shows the empty memory state alongside the signals that are not memories', async () => {
    mockCalls([], [SIGNAL, { ...SIGNAL, id: 'p2' }]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    expect(await screen.findByText('Nothing remembered yet')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /System learning log\s*2/ })).toBeInTheDocument();
  });

  // ── WP6: channel-action → dashboard live feedback ──────────────────────

  it('refetches when a memory.changed push names this agent', async () => {
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);
    await screen.findByText(/Recent memories/);

    const before = mockWsClient.call.mock.calls.filter((c) => c[0] === 'memory.browse').length;
    // A conversation on Telegram just wrote a memory for this agent.
    wsHandlers.get('memory.changed')?.({ action: 'stored', agent_id: 'agnes', memory_id: 'm9' });

    await waitFor(
      () => {
        const after = mockWsClient.call.mock.calls.filter((c) => c[0] === 'memory.browse').length;
        expect(after).toBeGreaterThan(before);
      },
      { timeout: 2000 },
    );
  });

  it('ignores a memory.changed push for a different agent', async () => {
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);
    await screen.findByText(/Recent memories/);

    const before = mockWsClient.call.mock.calls.filter((c) => c[0] === 'memory.browse').length;
    wsHandlers.get('memory.changed')?.({ action: 'stored', agent_id: 'someone-else' });

    // Past the trailing debounce window — a filtered-out event never even
    // schedules a refetch, so nothing should land.
    await new Promise((r) => setTimeout(r, 600));
    const after = mockWsClient.call.mock.calls.filter((c) => c[0] === 'memory.browse').length;
    expect(after).toBe(before);
  });

  it('refetches on an agent-less memory.changed push rather than risk missing it', async () => {
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);
    await screen.findByText(/Recent memories/);

    const before = mockWsClient.call.mock.calls.filter((c) => c[0] === 'memory.browse').length;
    wsHandlers.get('memory.changed')?.({ action: 'stored' });

    await waitFor(
      () => {
        const after = mockWsClient.call.mock.calls.filter((c) => c[0] === 'memory.browse').length;
        expect(after).toBeGreaterThan(before);
      },
      { timeout: 2000 },
    );
  });

  // M4 — distilling one pasted document raises one event per persisted fact.
  it('collapses a burst of memory.changed pushes into a single refetch', async () => {
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);
    await screen.findByText(/Recent memories/);

    const before = mockWsClient.call.mock.calls.filter((c) => c[0] === 'memory.browse').length;
    const push = wsHandlers.get('memory.changed');
    expect(push).toBeDefined();
    for (let i = 0; i < 6; i++) push?.({ action: 'distilled', agent_id: 'agnes' });

    await new Promise((r) => setTimeout(r, 700));
    const after = mockWsClient.call.mock.calls.filter((c) => c[0] === 'memory.browse').length;
    expect(after).toBe(before + 1);
  });

  // M3 — a reconnect re-reads; events raised while offline are gone.
  it('refetches after the socket reconnects', async () => {
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);
    await screen.findByText(/Recent memories/);
    const before = mockWsClient.call.mock.calls.filter((c) => c[0] === 'memory.browse').length;

    await act(async () => {
      useConnectionStore.setState({ state: 'disconnected' });
    });
    await act(async () => {
      useConnectionStore.setState({ state: 'authenticated' });
    });

    await waitFor(() => {
      const after = mockWsClient.call.mock.calls.filter((c) => c[0] === 'memory.browse').length;
      expect(after).toBeGreaterThan(before);
    });
  });
});

// ── W3-3: state-as-URL (Stripe pattern B4) — the category rail is
// bookmarkable/shareable via `?cat=`.

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
                <MemoryBrowser agentId="agnes" query="" />
                <SearchParamsProbe />
              </>
            }
          />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

describe('MemoryBrowser — category rail as URL (W3-3)', () => {
  it('seeds the selected category from ?cat= on load', async () => {
    renderAt('/memory?cat=business');

    expect(await screen.findByText('客戶的合約報價是三萬元')).toBeInTheDocument();
    // The other categories' entries are filtered out by the seeded selection.
    expect(screen.queryByText('老闆喜歡簡短的回覆語氣')).not.toBeInTheDocument();
  });

  it('ignores an unrecognized ?cat= value and falls back to "all"', async () => {
    renderAt('/memory?cat=not-a-real-category');

    // All three seeded entries are visible — the bad param never narrowed anything.
    expect(await screen.findByText('客戶的合約報價是三萬元')).toBeInTheDocument();
    expect(screen.getByText('老闆喜歡簡短的回覆語氣')).toBeInTheDocument();
  });

  it('mirrors an in-app category click back into the URL', async () => {
    const user = userEvent.setup();
    renderAt('/memory');

    await user.click(await screen.findByRole('button', { name: /Customers & deals\s*1/ }));

    await waitFor(() => {
      expect(screen.getByTestId('search-probe')).toHaveTextContent('cat=business');
    });
  });

  it('clears the "cat" param when the rail returns to "All"', async () => {
    const user = userEvent.setup();
    renderAt('/memory?cat=business');
    await waitFor(() => {
      expect(screen.getByTestId('search-probe')).toHaveTextContent('cat=business');
    });

    await user.click(screen.getByRole('button', { name: /^All\s*3/ }));

    await waitFor(() => {
      expect(screen.getByTestId('search-probe')).not.toHaveTextContent('cat=');
    });
  });
});

// ── R1: memory decay made visible (2026-08-12) ──
//
// A memory is not a permanent record — unused ones fade and are eventually
// filed away. That used to be invisible right up until a row disappeared.

describe('MemoryBrowser freshness', () => {
  /** Same memory, with the decay figures the gateway now sends. */
  const FADING = {
    id: 'd1',
    agent_id: 'agnes',
    content: '客戶偏好下午開會',
    timestamp: '2026-05-01T10:00:00Z',
    tags: [],
    layer: 'semantic',
    retrievability: 0.2,
    stability_days: 14,
    last_accessed: null,
  };

  it('shows the state on the row, in words a user can act on', async () => {
    mockCalls([FADING]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    const badge = await screen.findByLabelText(/^Fading —/);
    expect(badge).toHaveTextContent('Fading');
    // The badge is a diagnosis; the explanation has to carry the treatment.
    expect(badge).toHaveAccessibleName(/Using it again makes it last longer/);
  });

  it('explains the four states once, above the list', async () => {
    mockCalls([FADING]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    const legend = await screen.findByRole('list', {
      name: 'What the freshness labels mean',
    });
    expect(within(legend).getAllByRole('listitem')).toHaveLength(4);
  });

  it('claims no state when the gateway sent no figure', async () => {
    // An older gateway sends rows without the decay fields. Guessing a state
    // would be worse than showing none.
    mockCalls([{ ...FADING, retrievability: undefined, stability_days: undefined }]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    await screen.findByText('客戶偏好下午開會');
    expect(screen.queryByLabelText(/^Fading —/)).not.toBeInTheDocument();
  });

  it('opens the full detail — curve and revision history — in one place', async () => {
    const user = userEvent.setup();
    mockCalls([FADING]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    await user.click(await screen.findByText('客戶偏好下午開會'));

    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('How long this one lasts')).toBeInTheDocument();
    // Every number the curve encodes is also written out, so nothing about it
    // is gated behind hovering a plot.
    expect(within(dialog).getByText(/could be filed away at any time/)).toBeInTheDocument();
    expect(within(dialog).getByText('Filed away')).toBeInTheDocument();
    // The provenance panel still loads lazily, only once opened.
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('memory.history', {
        agent_id: 'agnes',
        memory_id: 'd1',
      });
    });
  });

  it('omits the curve when there is no figure to draw one from', async () => {
    const user = userEvent.setup();
    mockCalls([{ ...FADING, stability_days: undefined }]);
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    await user.click(await screen.findByText('客戶偏好下午開會'));

    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).queryByText('How long this one lasts')).not.toBeInTheDocument();
    // The rest of the detail is unaffected.
    expect(within(dialog).getByText('Kind')).toBeInTheDocument();
  });
});
