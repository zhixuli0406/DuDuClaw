import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAgentsStore } from '@/stores/agents-store';
import { useAssignStore } from '@/stores/assign-store';
import { useSystemStore } from '@/stores/system-store';
import { AssignSheet } from './AssignSheet';
import { AppSidebar } from '@/components/layout/AppSidebar';
import { MobileBottomNav } from '@/components/layout/MobileBottomNav';
import { SidebarProvider } from '@/components/mds';

const mockNavigate = vi.fn();
vi.mock('react-router', async (importOriginal) => {
  const actual = await importOriginal<typeof import('react-router')>();
  return { ...actual, useNavigate: () => mockNavigate };
});

const ROSTER = [
  { name: 'scout', display_name: 'Scout', icon: '🐾', role: 'main', status: 'active' },
  { name: 'nova', display_name: 'Nova', icon: '🛰', role: 'worker', status: 'active' },
];

/** Personal edition — the profile whose primary action used to be gated off. */
function personalEdition() {
  useSystemStore.setState({
    status: { edition_profile: 'personal', channels_connected: 0 } as never,
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  try {
    localStorage.clear();
  } catch {
    /* jsdom */
  }
  useAgentsStore.setState({ agents: ROSTER as never, loaded: true });
  useAssignStore.setState({ open: false, prefill: null });
  personalEdition();
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'tasks.goal_create') {
      return Promise.resolve({
        task: { id: 'gt-9', title: 'x' },
        iteration_cap: 8,
        dispatch_enabled: true,
      });
    }
    return Promise.resolve({});
  });
});

describe('AssignSheet', () => {
  it('renders on the Personal edition — the primary action is no longer gated off', async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <SidebarProvider>
        <AppSidebar />
        <AssignSheet />
      </SidebarProvider>,
    );
    const trigger = await screen.findByRole('button', { name: 'New task' });
    await user.click(trigger);
    expect(await screen.findByRole('heading', { name: 'Assign task' })).toBeInTheDocument();
  });

  it('opens the SAME panel from the sidebar and the mobile ＋', async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <SidebarProvider>
        <AppSidebar />
        <MobileBottomNav />
        <AssignSheet />
      </SidebarProvider>,
    );
    const triggers = await screen.findAllByRole('button', { name: 'New task' });
    // Both editions render both entries; each one flips the same store.
    expect(triggers.length).toBeGreaterThanOrEqual(2);

    await user.click(triggers[0]);
    expect(await screen.findByRole('heading', { name: 'Assign task' })).toBeInTheDocument();
    expect(useAssignStore.getState().open).toBe(true);

    act(() => useAssignStore.getState().closeAssign());
    await waitFor(() => expect(useAssignStore.getState().open).toBe(false));

    await user.click(triggers[1]);
    expect(await screen.findByRole('heading', { name: 'Assign task' })).toBeInTheDocument();
    // One mounted panel, not one per entry point.
    expect(screen.getAllByRole('heading', { name: 'Assign task' })).toHaveLength(1);
  });

  it('shows the four-element guidance (goal / input / output format / limits)', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AssignSheet />);
    act(() => useAssignStore.getState().openAssign());
    await user.click(await screen.findByRole('button', { name: /these four cover it/i }));
    expect(screen.getByText(/^Goal:/)).toBeInTheDocument();
    expect(screen.getByText(/^Input:/)).toBeInTheDocument();
    expect(screen.getByText(/^Output format:/)).toBeInTheDocument();
    expect(screen.getByText(/^Limits and done-criteria:/)).toBeInTheDocument();
  });

  it('submits 交辦 mode through tasks.goal_create and lands on the new task', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AssignSheet />);
    act(() => useAssignStore.getState().openAssign());
    await user.type(
      await screen.findByLabelText('What should the AI employee do?'),
      'Sort the feedback',
    );
    await user.type(screen.getByLabelText(/What does done look like/), 'One page max');
    await user.click(screen.getByRole('button', { name: 'Assign task' }));

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
    // A passive board card is never created by this path.
    expect(mockWsClient.call.mock.calls.filter((c) => c[0] === 'tasks.create')).toHaveLength(0);
  });

  it('carries the chosen output format into the acceptance criteria (no new backend field)', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AssignSheet />);
    act(() => useAssignStore.getState().openAssign());
    await user.type(await screen.findByLabelText('What should the AI employee do?'), 'Do a thing');
    await user.type(screen.getByLabelText(/What does done look like/), 'Short');
    await user.click(screen.getByRole('combobox', { name: 'Output format' }));
    await user.click(await screen.findByRole('option', { name: 'Table' }));
    await user.click(screen.getByRole('button', { name: 'Assign task' }));

    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith(
        'tasks.goal_create',
        expect.objectContaining({ acceptance_criteria: 'Short\nOutput format: Table' }),
      );
    });
  });

  it('想一想 mode sends plan_first and still lands on the created task', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'tasks.goal_create') {
        return Promise.resolve({
          task: { id: 'gt-9', title: 'x' },
          iteration_cap: 8,
          dispatch_enabled: true,
          plan_first: true,
        });
      }
      return Promise.resolve({});
    });
    const user = userEvent.setup();
    renderWithProviders(<AssignSheet />);
    act(() => useAssignStore.getState().openAssign());
    await user.type(
      await screen.findByLabelText('What should the AI employee do?'),
      'Sort the feedback',
    );
    await user.click(screen.getByRole('radio', { name: 'Plan first' }));
    await user.click(screen.getByRole('button', { name: 'Generate plan' }));

    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith(
        'tasks.goal_create',
        expect.objectContaining({
          agent_id: 'scout',
          description: 'Sort the feedback',
          plan_first: true,
        }),
      );
    });
    expect(mockNavigate).toHaveBeenCalledWith('/goals?task=gt-9');
  });

  it('問一問 mode goes to the chat page instead of filing a task', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AssignSheet />);
    act(() => useAssignStore.getState().openAssign({ mode: 'ask' }));
    await user.type(await screen.findByLabelText('What should the AI employee do?'), 'quick q');
    await user.click(screen.getByRole('button', { name: 'Start a chat' }));

    expect(mockNavigate).toHaveBeenCalledWith('/chat?agent=scout', {
      state: { draft: 'quick q' },
    });
    expect(mockWsClient.call.mock.calls.filter((c) => c[0] === 'tasks.goal_create')).toHaveLength(0);
  });

  it('opens preselected when an entry point names an employee', async () => {
    renderWithProviders(<AssignSheet />);
    act(() => useAssignStore.getState().openAssign({ agentId: 'nova', description: 'seeded goal' }));
    expect(await screen.findByDisplayValue('seeded goal')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Nova/ })).toBeInTheDocument();
  });
});
