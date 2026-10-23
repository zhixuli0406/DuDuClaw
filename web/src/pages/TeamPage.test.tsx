import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import type { MyTeam, TeamMember } from '@/lib/api';
import { TeamPage, nestTeam } from './TeamPage';

const navigate = vi.fn();
vi.mock('react-router', async () => {
  const actual = await vi.importActual<typeof import('react-router')>('react-router');
  return { ...actual, useNavigate: () => navigate };
});

const member = (over: Partial<TeamMember> & { id: string }): TeamMember => ({
  display_name: over.id,
  role: 'employee',
  status: 'active',
  manager_user_id: 'mgr',
  depth: 1,
  agents: [],
  ...over,
});

const TEAM: MyTeam = {
  manager: { id: 'mgr', display_name: 'Chen' },
  max_depth: 8,
  members: [
    member({ id: 'lead', display_name: 'Lin', role: 'manager' }),
    member({
      id: 'emp2',
      display_name: 'Lee',
      agents: [
        {
          name: 'hr',
          display_name: 'HR Helper',
          call_text: '@HR Helper',
          status: 'active',
          operational: true,
          level: 'viewer',
          assignable: false,
          active_tasks: 0,
        },
      ],
    }),
    member({
      id: 'emp1',
      display_name: 'Wang',
      manager_user_id: 'lead',
      depth: 2,
      status: 'suspended',
      agents: [
        {
          name: 'sales',
          display_name: 'Sales Rep',
          call_text: '@Sales Rep',
          status: 'active',
          operational: true,
          level: 'operator',
          assignable: true,
          active_tasks: 2,
        },
        {
          name: 'paused-bot',
          display_name: 'Paused Bot',
          call_text: '@Paused Bot',
          status: 'paused',
          operational: false,
          level: 'operator',
          assignable: false,
          active_tasks: null,
        },
      ],
    }),
  ],
};

function answer(team: MyTeam) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'users.my_team') return Promise.resolve(team);
    if (method === 'tasks.create') return Promise.resolve({ task: { id: 'task-1' } });
    return Promise.resolve({});
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  answer(TEAM);
});

describe('nestTeam', () => {
  it('nests reports under their manager and keeps the caller out of the tree', () => {
    const tree = nestTeam(TEAM.members, 'mgr');
    expect(tree.map((n) => n.member.id)).toEqual(['lead', 'emp2']);
    expect(tree[0].reports.map((n) => n.member.id)).toEqual(['emp1']);
  });

  it('terminates on a cycle in malformed data and still shows everyone', () => {
    const cyclic = [
      member({ id: 'a', manager_user_id: 'b' }),
      member({ id: 'b', manager_user_id: 'a' }),
    ];
    const tree = nestTeam(cyclic, 'mgr');
    expect(tree.map((n) => n.member.id).sort()).toEqual(['a', 'b']);
  });
});

describe('TeamPage', () => {
  it('shows the tree, each report’s AI staff and what the viewer may do with them', async () => {
    renderWithProviders(<TeamPage />);
    expect(await screen.findByRole('heading', { name: 'My team' })).toBeInTheDocument();
    expect(await screen.findByText('Sales Rep')).toBeInTheDocument();
    expect(screen.getByText('3 people · 3 AI staff members')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('users.my_team');

    const wang = screen.getByRole('region', { name: 'Wang' });
    expect(within(wang).getByText('Suspended')).toBeInTheDocument();
    expect(within(wang).getByText('2 open tasks')).toBeInTheDocument();
    const [chatSales, chatPaused] = within(wang).getAllByRole('button', { name: 'Chat' });
    const [assignSales, assignPaused] = within(wang).getAllByRole('button', { name: 'Assign' });
    expect(chatSales).toBeEnabled();
    expect(assignSales).toBeEnabled();
    expect(chatPaused).toBeDisabled();
    expect(assignPaused).toBeDisabled();
    expect(within(wang).getByText('Not available')).toBeInTheDocument();

    const lee = screen.getByRole('region', { name: 'Lee' });
    expect(within(lee).getByText('Chat only')).toBeInTheDocument();
    expect(within(lee).getByRole('button', { name: 'Assign' })).toBeDisabled();
    expect(within(lee).getByRole('button', { name: 'Chat' })).toBeEnabled();

    const lin = screen.getByRole('region', { name: 'Lin' });
    expect(within(lin).getByText('No AI staff available through this person.')).toBeInTheDocument();
  });

  it('opens a chat with the employee already addressed', async () => {
    const user = userEvent.setup();
    renderWithProviders(<TeamPage />);
    const lee = await screen.findByRole('region', { name: 'Lee' });
    await user.click(within(lee).getByRole('button', { name: 'Chat' }));
    expect(navigate).toHaveBeenCalledWith('/chat', { state: { draft: '@HR Helper ' } });
  });

  it('assigns a goal with tasks.create kind=goal and stays on the team page', async () => {
    const user = userEvent.setup();
    renderWithProviders(<TeamPage />);
    const wang = await screen.findByRole('region', { name: 'Wang' });
    await user.click(within(wang).getAllByRole('button', { name: 'Assign' })[0]);
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('Assign to Sales Rep')).toBeInTheDocument();
    expect(within(dialog).queryByText(/Goals page/)).not.toBeInTheDocument();
    const submit = within(dialog).getByRole('button', { name: 'Assign' });
    expect(submit).toBeDisabled();
    await user.type(within(dialog).getByLabelText('What should be done'), 'Tidy up the quotes');
    await user.click(submit);
    expect(mockWsClient.call).toHaveBeenCalledWith('tasks.create', {
      kind: 'goal',
      assigned_to: 'sales',
      agent_id: 'sales',
      description: 'Tidy up the quotes',
    });
    // A manager cannot open a report's task yet: no jump to a page that
    // would only say "permission denied".
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(navigate).not.toHaveBeenCalled();
    expect(screen.getByRole('heading', { name: 'My team' })).toBeInTheDocument();
  });

  it('explains how to get a team when nobody reports to the viewer', async () => {
    answer({ manager: { id: 'mgr', display_name: 'Chen' }, max_depth: 8, members: [] });
    renderWithProviders(<TeamPage />);
    expect(await screen.findByText('No one reports to you yet')).toBeInTheDocument();
  });

  it('shows the gateway refusal instead of an empty team', async () => {
    mockWsClient.call.mockRejectedValue(new Error('permission denied'));
    const { container } = renderWithProviders(<TeamPage />);
    await waitFor(() => expect(container.querySelector('.text-destructive')).not.toBeNull());
    expect(screen.queryByText('No one reports to you yet')).not.toBeInTheDocument();
  });

  it('clears the team it showed when a reload is refused', async () => {
    const user = userEvent.setup();
    const { container } = renderWithProviders(<TeamPage />);
    const wang = await screen.findByRole('region', { name: 'Wang' });
    // Assigning reloads the team on close; this time the gateway refuses.
    await user.click(within(wang).getAllByRole('button', { name: 'Assign' })[0]);
    const dialog = await screen.findByRole('dialog');
    mockWsClient.call.mockRejectedValue({ code: 'enterprise_edition_required', message: 'refused' });
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(container.querySelector('.text-destructive')).not.toBeNull());
    expect(screen.queryByText('Wang')).not.toBeInTheDocument();
    expect(screen.queryByText('Sales Rep')).not.toBeInTheDocument();
  });
});
