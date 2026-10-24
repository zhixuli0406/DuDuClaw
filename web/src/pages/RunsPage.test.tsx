import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, act } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route, useSearchParams } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { RunsPage } from './RunsPage';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';

const AGENTS = [
  { name: 'nova', display_name: 'Nova', status: 'active', role: 'main', sandboxed: false },
  { name: 'agnes', display_name: 'Agnes', status: 'active', role: 'worker', sandboxed: false },
];

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ runs: [], agents: AGENTS });
  useAuthStore.setState({ user: { display_name: 'Boss', role: 'admin' }, bindings: [] } as never);
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  useAgentsStore.setState({ agents: [], loading: false } as never);
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('RunsPage', () => {
  it('renders the run-log header in the split layout', () => {
    renderWithProviders(<RunsPage />);
    expect(screen.getByRole('heading', { name: 'Run log' })).toBeInTheDocument();
  });

  it('shows the empty transcript prompt when no run is selected', () => {
    renderWithProviders(<RunsPage />);
    expect(screen.getByText('Pick a run on the left')).toBeInTheDocument();
  });
});

// ── W3-3: state-as-URL (Stripe pattern B4) — the agent filter is
// bookmarkable/shareable.

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
            path="/runs"
            element={
              <>
                <RunsPage />
                <SearchParamsProbe />
              </>
            }
          />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

describe('RunsPage — agent filter as URL (W3-3)', () => {
  it('seeds the agent filter from ?agent= on load', async () => {
    renderAt('/runs?agent=agnes');
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('runs.list', { agent_id: 'agnes' });
    });
  });

  it('mirrors the seeded filter back into the URL (round-trip, no stale param left behind)', async () => {
    renderAt('/runs?agent=agnes');
    await waitFor(() => {
      expect(screen.getByTestId('search-probe')).toHaveTextContent('agent=agnes');
    });
  });

  it('does not write an "agent" param when no filter is selected', async () => {
    renderAt('/runs');
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Run log' })).toBeInTheDocument();
    });
    expect(screen.getByTestId('search-probe')).not.toHaveTextContent('agent=');
  });
});

// ── P11: the open transcript is deep-linkable too (`?run=<id>`) ───────────────

/** A minimal but well-formed `runs.get` payload (the page reads `detail.run`). */
const RUN_DETAIL = {
  run: {
    id: 'run-42',
    agent_id: 'nova',
    channel: 'webchat',
    started_at: '2026-08-12T00:00:00Z',
    status: 'ok',
  },
  events: [],
};

describe('RunsPage — selected run as URL (P11)', () => {
  beforeEach(() => {
    mockWsClient.call.mockImplementation((method: string) =>
      method === 'runs.get'
        ? Promise.resolve(RUN_DETAIL)
        : Promise.resolve({ runs: [], agents: AGENTS }),
    );
  });

  it('fetches the transcript named by ?run= on load', async () => {
    renderAt('/runs?run=run-42');
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('runs.get', { run_id: 'run-42' });
    });
  });

  it('shows the transcript pane instead of the "pick a run" prompt', async () => {
    renderAt('/runs?run=run-42');
    await waitFor(() => {
      expect(screen.queryByText('Pick a run on the left')).not.toBeInTheDocument();
    });
  });

  it('writes the selected run back into the URL when a row is clicked', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockImplementation((method: string) =>
      method === 'runs.list'
        ? Promise.resolve({
            runs: [
              {
                id: 'run-7',
                agent_id: 'nova',
                channel: 'webchat',
                started_at: new Date().toISOString(),
                status: 'ok',
              },
            ],
            agents: AGENTS,
          })
        : Promise.resolve({ run: {}, events: [] }),
    );
    renderAt('/runs');

    const row = await screen.findByRole('button', { name: /run-7|Nova/i });
    await user.click(row);
    await waitFor(() => {
      expect(screen.getByTestId('search-probe')).toHaveTextContent('run=run-7');
    });
  });
});

// ── Thinking rows: seconds and tokens only (the text is never stored) ─────────

describe('RunsPage — thinking step', () => {
  it('renders a thinking row as localized seconds + tokens', async () => {
    mockWsClient.call.mockImplementation((method: string) =>
      method === 'runs.get'
        ? Promise.resolve({
            ...RUN_DETAIL,
            events: [
              {
                kind: 'thinking',
                label: 'thinking',
                seq: 0,
                ts: '2026-08-12T00:00:01Z',
                preview: '思考 8 秒 · 1,240 token',
              },
            ],
          })
        : Promise.resolve({ runs: [], agents: AGENTS }),
    );
    renderAt('/runs?run=run-42');
    const row = await screen.findByTestId('run-thinking-row');
    expect(row).toHaveTextContent('Thought for 8s · ~1.2k tokens');
    // Nothing to expand: no <details> around it.
    expect(row.closest('details')).toBeNull();
  });
});


describe('RunsPage restricted history',()=>{
  it('shows the installed-detection boundary for masked run text',async()=>{
    mockWsClient.call.mockImplementation((method:string)=>Promise.resolve(method==='runs.get'?{...RUN_DETAIL,read_only:true,privacy_mode:'masked',ner:false,omitted:['tool_steps'],events:[{kind:'text',role:'user',ts:'2026-10-24T00:00:00Z',preview:'[PHONE_1]'}]}:{runs:[],agents:AGENTS}));
    renderAt('/runs?run=run-42');
    expect(await screen.findByText(/AI detection is not installed/)).toBeInTheDocument();
  });
  it('never installs an old account transcript after a scope switch',async()=>{
    let resolveOld!:(v:unknown)=>void;
    mockWsClient.call.mockImplementation((method:string)=>method==='runs.get'?new Promise(r=>{resolveOld=r;}):Promise.resolve({runs:[],agents:AGENTS}));
    renderAt('/runs?run=run-42');
    await waitFor(()=>expect(resolveOld).toBeTypeOf('function'));
    const finishOld=resolveOld;
    act(()=>useAuthStore.setState({user:{id:'new-account',display_name:'New',role:'employee',status:'active'},bindings:[]} as never));
    await act(async()=>finishOld({...RUN_DETAIL,events:[{kind:'text',role:'user',ts:'2026-10-24T00:00:00Z',preview:'PREVIOUS ACCOUNT PRIVATE RUN'}]}));
    expect(screen.queryByText('PREVIOUS ACCOUNT PRIVATE RUN')).not.toBeInTheDocument();
  });
});
