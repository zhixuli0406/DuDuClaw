import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Route, Routes } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAgentsStore } from '@/stores/agents-store';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore } from '@/stores/auth-store';
import { AppSidebar } from '@/components/layout/AppSidebar';
import { MobileBottomNav } from '@/components/layout/MobileBottomNav';
import { SidebarProvider } from '@/components/mds';
import { takeChatAttachments } from '@/lib/chat-handoff';
import { formatRunTime } from '@/lib/skill-assign';
import type { SkillsListAll } from '@/lib/api';
import { AssignPage } from './AssignPage';

const mockNavigate = vi.fn();
vi.mock('react-router', async (importOriginal) => {
  const actual = await importOriginal<typeof import('react-router')>();
  return { ...actual, useNavigate: () => mockNavigate };
});

const toastError = vi.fn();
const toastSuccess = vi.fn();
vi.mock('@/lib/toast', async (importOriginal) => {
  const orig = await importOriginal<typeof import('@/lib/toast')>();
  return {
    ...orig,
    toast: { ...orig.toast, success: (m: string) => toastSuccess(m), error: (m: string) => toastError(m) },
  };
});

const ROSTER = [
  { name: 'scout', display_name: 'Scout', icon: '🐾', role: 'main', status: 'active' },
  { name: 'nova', display_name: 'Nova', icon: '🛰', role: 'worker', status: 'active' },
];

/** Quote: both ran it, Nova last. Beta: nobody ran it. Report: Nova's own
 *  skill, ran most recently. Alpha: Nova's own, never ran. Off: switched off
 *  everywhere. Risky: only copy failed the install scan. */
const SKILLS: SkillsListAll = {
  global_skills: [
    {
      name: 'quote',
      display_name: 'Quote',
      description: 'Make a quote',
      content: '',
      holders: [
        { agent_id: 'scout', display_name: 'Scout', enabled: true, use_count: 2, last_used_at: '2026-10-01T02:03:00Z' },
        { agent_id: 'nova', display_name: 'Nova', enabled: true, use_count: 1, last_used_at: '2026-10-05T04:05:00Z' },
      ],
    },
    {
      name: 'off',
      display_name: 'Switched off',
      content: '',
      holders: [{ agent_id: 'scout', display_name: 'Scout', enabled: false, use_count: 0 }],
    },
    {
      name: 'risky',
      display_name: 'Risky',
      content: '',
      security_status: 'fail',
      holders: [{ agent_id: 'scout', display_name: 'Scout', enabled: true, use_count: 0 }],
    },
    {
      name: 'beta',
      display_name: 'Beta skill',
      description: 'Never run',
      content: '',
      holders: [
        { agent_id: 'scout', display_name: 'Scout', enabled: true, use_count: 0, last_used_at: null },
        { agent_id: 'nova', display_name: 'Nova', enabled: true, use_count: 0 },
      ],
    },
  ],
  agents: [
    {
      agent_id: 'nova',
      display_name: 'Nova',
      skills: [
        { name: 'report', display_name: 'Weekly report', description: 'Write it', content: '', scope: 'agent', enabled: true, use_count: 4, last_used_at: '2026-10-07T00:00:00Z' },
        { name: 'alpha', display_name: 'Alpha', content: '', scope: 'agent', enabled: true, use_count: 0, last_used_at: null },
      ],
    },
  ],
};

const CREATED = { task: { id: 'gt-9', title: 'x' }, iteration_cap: 8, dispatch_enabled: true, plan_first: false };

function mockRpc(skills: SkillsListAll = SKILLS) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'skills.list') return Promise.resolve(skills);
    if (method === 'tasks.goal_create' || method === 'tasks.skill_assign') return Promise.resolve(CREATED);
    return Promise.resolve({});
  });
}

function renderAt(entry: string | { pathname: string; search?: string; state?: unknown }) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[entry]}>
        <Routes>
          <Route path="/assign" element={<AssignPage />} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

function visiblePanel(): HTMLElement {
  return screen.getByRole('tabpanel');
}

function fileInput(): HTMLInputElement {
  const input = visiblePanel().querySelector('input[type="file"]');
  if (!input) throw new Error('no file input in the visible tab');
  return input as HTMLInputElement;
}

const fetchMock = vi.fn();

beforeEach(() => {
  vi.clearAllMocks();
  useAgentsStore.setState({ agents: ROSTER as never, loaded: true });
  useSystemStore.setState({ status: { edition_profile: 'personal', channels_connected: 0 } as never });
  useAuthStore.setState({ jwt: 'test-jwt' } as never);
  mockRpc();
  fetchMock.mockResolvedValue({
    ok: true,
    status: 200,
    json: () => Promise.resolve({ type: 'res', id: '', ok: true, payload: CREATED }),
  });
  vi.stubGlobal('fetch', fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('entry points', () => {
  it('the sidebar and the mobile ＋ both open the /assign page (Personal included)', async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <SidebarProvider>
        <AppSidebar />
        <MobileBottomNav />
      </SidebarProvider>,
    );
    const triggers = await screen.findAllByRole('button', { name: 'New task' });
    expect(triggers.length).toBeGreaterThanOrEqual(2);
    await user.click(triggers[0]);
    await user.click(triggers[1]);
    expect(mockNavigate).toHaveBeenNthCalledWith(1, '/assign', undefined);
    expect(mockNavigate).toHaveBeenNthCalledWith(2, '/assign', undefined);
  });
});

describe('AssignPage tabs', () => {
  it('opens on 一般 by default', async () => {
    renderAt('/assign');
    expect(screen.getByRole('heading', { name: 'Assign task' })).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: 'Standard' })).toHaveAttribute('aria-selected', 'true');
    expect(await screen.findByRole('radio', { name: /Quote/ })).toBeInTheDocument();
  });

  it('a seeded entry opens 複雜任務 with the seed', async () => {
    renderAt({ pathname: '/assign', state: { assignPrefill: { agentId: 'nova', description: 'seeded goal' } } });
    expect(screen.getByRole('tab', { name: 'Complex task' })).toHaveAttribute('aria-selected', 'true');
    expect(await screen.findByDisplayValue('seeded goal')).toBeInTheDocument();
    expect(within(visiblePanel()).getByRole('button', { name: /Nova/ })).toBeInTheDocument();
  });

  it('?tab=complex selects 複雜任務 without a seed', () => {
    renderAt('/assign?tab=complex');
    expect(screen.getByRole('tab', { name: 'Complex task' })).toHaveAttribute('aria-selected', 'true');
  });
});

describe('一般 tab', () => {
  it('lists recently run skills newest first, then the rest by name, and hides unusable ones', async () => {
    renderAt('/assign');
    await screen.findByRole('radio', { name: /Quote/ });
    const recent = screen.getByRole('radiogroup', { name: 'Recently run' });
    const other = screen.getByRole('radiogroup', { name: 'Other skills' });
    const names = (g: HTMLElement) => within(g).getAllByRole('radio').map((r) => (r as HTMLInputElement).value);
    expect(names(recent)).toEqual(['report', 'quote']);
    expect(names(other)).toEqual(['alpha', 'beta']);
    expect(screen.queryByRole('radio', { name: /Switched off/ })).not.toBeInTheDocument();
    expect(screen.queryByRole('radio', { name: /Risky/ })).not.toBeInTheDocument();
    // Right-side meta: when and who, or "not run yet".
    expect(within(recent).getByText(`${formatRunTime('2026-10-05T04:05:00Z')} · Nova`)).toBeInTheDocument();
    expect(within(other).getAllByText('Not run yet')).toHaveLength(2);
  });

  it('filters with the search box', async () => {
    const user = userEvent.setup();
    renderAt('/assign');
    await screen.findByRole('radio', { name: /Quote/ });
    await user.type(screen.getByRole('searchbox', { name: 'Search skills' }), 'weekly');
    expect(screen.getAllByRole('radio').map((r) => (r as HTMLInputElement).value)).toEqual(['report']);
  });

  it('names the runner the gateway will pick', async () => {
    const user = userEvent.setup();
    renderAt('/assign');
    expect(await screen.findByText('Pick a skill first')).toBeInTheDocument();
    await user.click(await screen.findByRole('radio', { name: /Quote/ }));
    expect(screen.getByText(/the AI employee who ran this skill most recently/)).toHaveTextContent(/^Nova will do it/);
    // Nobody ran Beta: the first holder by employee id (nova < scout).
    await user.click(screen.getByRole('radio', { name: /Beta skill/ }));
    expect(screen.getByText(/nobody has run this skill yet/)).toHaveTextContent(/^Nova will do it/);
  });

  it('submits tasks.skill_assign with the skill and the note, then lands on the task', async () => {
    const user = userEvent.setup();
    renderAt('/assign');
    await user.click(await screen.findByRole('radio', { name: /Quote/ }));
    await user.type(screen.getByLabelText('Anything to add? (optional)'), 'for Wang');
    await user.click(within(visiblePanel()).getByRole('button', { name: 'Assign task' }));
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('tasks.skill_assign', { skill_name: 'quote', note: 'for Wang' }),
    );
    expect(fetchMock).not.toHaveBeenCalled();
    expect(mockNavigate).toHaveBeenCalledWith('/goals?task=gt-9');
    expect(toastSuccess).toHaveBeenCalled();
  });

  it('sends files over HTTP with the request', async () => {
    const user = userEvent.setup();
    renderAt('/assign');
    await user.click(await screen.findByRole('radio', { name: /Quote/ }));
    await user.upload(fileInput(), [
      new File(['hello'], 'brief.txt', { type: 'text/plain' }),
      new File(['png'], 'logo.png', { type: 'image/png' }),
    ]);
    expect(await screen.findByText('brief.txt')).toBeInTheDocument();
    expect(screen.getByText('logo.png')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Remove logo.png' }));
    await user.click(within(visiblePanel()).getByRole('button', { name: 'Assign task' }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe('/api/tasks/rpc');
    expect(init.headers.Authorization).toBe('Bearer test-jwt');
    const body = JSON.parse(init.body);
    expect(body.method).toBe('tasks.skill_assign');
    expect(body.params.skill_name).toBe('quote');
    expect(body.params.attachments).toEqual([{ name: 'brief.txt', mime: 'text/plain', data_base64: btoa('hello') }]);
    expect(mockWsClient.call.mock.calls.filter((c) => c[0] === 'tasks.skill_assign')).toHaveLength(0);
    expect(mockNavigate).toHaveBeenCalledWith('/goals?task=gt-9');
  });

  it('refuses a file over 20 MB and an eleventh file', async () => {
    const user = userEvent.setup();
    renderAt('/assign');
    await screen.findByRole('radio', { name: /Quote/ });
    const big = new File(['x'], 'huge.bin');
    Object.defineProperty(big, 'size', { value: 21 * 1024 * 1024 });
    await user.upload(fileInput(), big);
    expect(toastError).toHaveBeenCalledWith('huge.bin is over 20 MB and was not added');
    const many = Array.from({ length: 11 }, (_, i) => new File([String(i)], `f${i}.txt`));
    await user.upload(fileInput(), many);
    await waitFor(() => expect(toastError).toHaveBeenCalledWith('A task can carry at most 10 files'));
    expect(within(screen.getByRole('list', { name: 'Files to hand over' })).getAllByRole('listitem')).toHaveLength(10);
  });

  it('shows the gateway refusal as a sentence', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'skills.list') return Promise.resolve(SKILLS);
      if (method === 'tasks.skill_assign') return Promise.reject('skill_not_enabled: no AI employee has this skill switched on');
      return Promise.resolve({});
    });
    const user = userEvent.setup();
    renderAt('/assign');
    await user.click(await screen.findByRole('radio', { name: /Quote/ }));
    await user.click(within(visiblePanel()).getByRole('button', { name: 'Assign task' }));
    await waitFor(() =>
      expect(toastError).toHaveBeenCalledWith(
        'No AI employee has this skill switched on. Turn it on in the skill library first.',
      ),
    );
    expect(mockNavigate).not.toHaveBeenCalled();
  });

  it('says so and links to the skill library when nothing can be handed over', async () => {
    mockRpc({ global_skills: [SKILLS.global_skills[1]], agents: [] });
    renderAt('/assign');
    expect(await screen.findByText('No skill is ready to hand over yet.')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Go to the skill library' })).toHaveAttribute('href', '/skills');
  });
});

describe('複雜任務 tab', () => {
  function openComplex() {
    renderAt('/assign?tab=complex');
  }

  it('shows the four-element guidance (goal / input / output format / limits)', async () => {
    const user = userEvent.setup();
    openComplex();
    await user.click(screen.getByRole('button', { name: /these four cover it/i }));
    expect(screen.getByText(/^Goal:/)).toBeInTheDocument();
    expect(screen.getByText(/^Input:/)).toBeInTheDocument();
    expect(screen.getByText(/^Output format:/)).toBeInTheDocument();
    expect(screen.getByText(/^Limits and done-criteria:/)).toBeInTheDocument();
  });

  it('submits 交辦 mode through tasks.goal_create and lands on the new task', async () => {
    const user = userEvent.setup();
    openComplex();
    await user.type(screen.getByLabelText('What should the AI employee do?'), 'Sort the feedback');
    await user.type(screen.getByLabelText(/What does done look like/), 'One page max');
    await user.click(within(visiblePanel()).getByRole('button', { name: 'Assign task' }));
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith(
        'tasks.goal_create',
        expect.objectContaining({
          agent_id: 'scout',
          description: 'Sort the feedback',
          acceptance_criteria: 'One page max',
        }),
      );
    });
    expect(mockNavigate).toHaveBeenCalledWith('/goals?task=gt-9');
    expect(mockWsClient.call.mock.calls.filter((c) => c[0] === 'tasks.create')).toHaveLength(0);
  });

  it('sends a positive integer cost cap when assigning a goal', async () => {
    const user = userEvent.setup();
    openComplex();
    await user.type(screen.getByLabelText('What should the AI employee do?'), 'Sort the feedback');
    await user.click(screen.getByRole('button', { name: /^More settings/ }));
    await user.type(screen.getByRole('spinbutton', { name: 'Cost cap (US cents)' }), '500');
    await user.click(within(visiblePanel()).getByRole('button', { name: 'Assign task' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('tasks.goal_create', expect.objectContaining({ cost_cap_cents: 500 })));
  });

  it('preserves the cap on scheduled dashboard assignments', async () => {
    const user = userEvent.setup();
    openComplex();
    await user.type(screen.getByLabelText('What should the AI employee do?'), 'Sort the feedback');
    await user.click(screen.getByRole('button', { name: /^More settings/ }));
    await user.type(screen.getByRole('spinbutton', { name: 'Cost cap (US cents)' }), '500');
    await user.click(screen.getByRole('combobox', { name: en['assign.when.label'] }));
    await user.click(await screen.findByRole('option', { name: en['assign.when.kind.hourly'] }));
    await user.click(within(visiblePanel()).getByRole('button', { name: en['assign.when.submit'] }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('tasks.goal_create', expect.objectContaining({ cost_cap_cents: 500, schedule: expect.objectContaining({ kind: 'hourly' }) })));
  });

  it('refuses fractional cost caps before creating a goal', async () => {
    const user = userEvent.setup();
    openComplex();
    await user.type(screen.getByLabelText('What should the AI employee do?'), 'Sort the feedback');
    await user.click(screen.getByRole('button', { name: /^More settings/ }));
    await user.type(screen.getByRole('spinbutton', { name: 'Cost cap (US cents)' }), '2.5');
    await user.click(within(visiblePanel()).getByRole('button', { name: 'Assign task' }));
    expect(toastError).toHaveBeenCalledWith('Enter a positive whole number of US cents.');
    expect(mockWsClient.call.mock.calls.some(([method]) => method === 'tasks.goal_create')).toBe(false);
  });

  it('carries the chosen output format into the acceptance criteria (no new backend field)', async () => {
    const user = userEvent.setup();
    openComplex();
    await user.type(screen.getByLabelText('What should the AI employee do?'), 'Do a thing');
    await user.type(screen.getByLabelText(/What does done look like/), 'Short');
    await user.click(screen.getByRole('combobox', { name: 'Output format' }));
    await user.click(await screen.findByRole('option', { name: 'Table' }));
    await user.click(within(visiblePanel()).getByRole('button', { name: 'Assign task' }));
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith(
        'tasks.goal_create',
        expect.objectContaining({ acceptance_criteria: 'Short\nOutput format: Table' }),
      );
    });
  });

  it('想一想 mode sends plan_first and still lands on the created task', async () => {
    const user = userEvent.setup();
    openComplex();
    await user.type(screen.getByLabelText('What should the AI employee do?'), 'Sort the feedback');
    await user.click(screen.getByRole('radio', { name: 'Plan first' }));
    await user.click(within(visiblePanel()).getByRole('button', { name: 'Generate plan' }));
    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith(
        'tasks.goal_create',
        expect.objectContaining({ agent_id: 'scout', description: 'Sort the feedback', plan_first: true }),
      );
    });
    expect(mockNavigate).toHaveBeenCalledWith('/goals?task=gt-9');
  });

  it('sends attached files with the goal over HTTP', async () => {
    const user = userEvent.setup();
    openComplex();
    await user.type(screen.getByLabelText('What should the AI employee do?'), 'Check the list');
    await user.upload(fileInput(), new File(['a,b'], 'list.csv', { type: 'text/csv' }));
    expect(await screen.findByText('list.csv')).toBeInTheDocument();
    expect(screen.getByText('Up to 20 MB per file')).toBeInTheDocument();
    await user.click(within(visiblePanel()).getByRole('button', { name: 'Assign task' }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const body = JSON.parse(fetchMock.mock.calls[0][1].body);
    expect(body.method).toBe('tasks.goal_create');
    expect(body.params).toEqual(
      expect.objectContaining({
        agent_id: 'scout',
        description: 'Check the list',
        attachments: [{ name: 'list.csv', mime: 'text/csv', data_base64: btoa('a,b') }],
      }),
    );
  });

  it('問一問 mode goes to the chat page instead of filing a task', async () => {
    const user = userEvent.setup();
    renderAt({ pathname: '/assign', state: { assignPrefill: { mode: 'ask' } } });
    await user.type(screen.getByLabelText('What should the AI employee do?'), 'quick q');
    await user.click(screen.getByRole('button', { name: 'Start a chat' }));
    expect(mockNavigate).toHaveBeenCalledWith('/chat?agent=scout', { state: { draft: 'quick q' } });
    expect(mockWsClient.call.mock.calls.filter((c) => c[0] === 'tasks.goal_create')).toHaveLength(0);
  });

  it('問一問 hands the files to the chat page too', async () => {
    const user = userEvent.setup();
    renderAt({ pathname: '/assign', state: { assignPrefill: { mode: 'ask' } } });
    await user.type(screen.getByLabelText('What should the AI employee do?'), 'look at this');
    await user.upload(fileInput(), new File(['hi'], 'note.txt', { type: 'text/plain' }));
    await screen.findByText('note.txt');
    await user.click(screen.getByRole('button', { name: 'Start a chat' }));
    const [to, opts] = mockNavigate.mock.calls[0];
    expect(to).toBe('/chat?agent=scout');
    expect(opts.state.draft).toBe('look at this');
    // Files never ride in history state, only a one-time key.
    expect(JSON.stringify(opts.state)).not.toContain(btoa('hi'));
    expect(takeChatAttachments(opts.state.attachmentsKey)).toEqual([
      { name: 'note.txt', mime: 'text/plain', dataBase64: btoa('hi') },
    ]);
    expect(takeChatAttachments(opts.state.attachmentsKey)).toEqual([]);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
