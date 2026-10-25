import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, waitFor, within, act } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { AgentConnectionsTab } from './AgentConnectionsTab';
import { api, type AgentGoogleIdentity } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';

const HOME: AgentGoogleIdentity = {
  agent_id: 'bot',
  kind: 'home',
  email: null,
  explicit: false,
  fallback: 'none',
  requesters: 'bound_people',
  state: 'home',
  message: '',
};

function setRole(role: 'admin' | 'manager' | 'employee') {
  useAuthStore.setState({ user: { id: 'u1', email: 'u1@test.invalid', display_name: 'U', status: 'active', role } } as never);
}

beforeEach(() => {
  vi.restoreAllMocks();
  useAuthStore.setState({bindings:[]});
});

describe('AgentConnectionsTab', () => {
  const owners = [
    { user_id: 'alice', name: 'Alice', email: 'alice@test.invalid', is_primary: true },
    { user_id: 'bob', name: 'Bob', email: 'bob@test.invalid', is_primary: false },
  ];

  it('keeps the current employee visible when an old primary-owner request finishes late', async () => {
    setRole('admin');
    vi.spyOn(api.googleIdentity, 'get').mockImplementation(async (id) => ({
      ...HOME, agent_id: id, message: id === 'bot' ? 'Scope A only' : 'Scope B only',
      owners: id === 'bot' ? owners : [
        { user_id: 'carol', name: 'Carol', email: 'carol@test.invalid', is_primary: true },
        { user_id: 'dan', name: 'Dan', email: 'dan@test.invalid', is_primary: false },
      ],
    }));
    let finish!: (value: unknown) => void;
    const pending = new Promise(resolve => { finish = resolve; });
    vi.spyOn(api.personalGoogle, 'primaryOwner').mockReturnValue(pending);
    const { rerender } = renderWithProviders(<AgentConnectionsTab agentId="bot" />);
    const user = userEvent.setup();
    await user.click((await screen.findAllByRole('button', { name: 'Set primary owner' }))[1]);
    const dialog = await screen.findByRole('dialog', { name: 'Change primary owner?' });
    await user.click(within(dialog).getByRole('button', { name: 'Confirm' }));
    rerender(<AgentConnectionsTab agentId="other" />);
    await screen.findByText('Scope B only');
    await act(async () => { finish({}); await pending; });
    expect(screen.getByText('Scope B only')).toBeInTheDocument();
    expect(screen.queryByText('Scope A only')).not.toBeInTheDocument();
    expect(screen.getByText(/Dan/)).toBeInTheDocument();
    expect(screen.queryByText(/Alice/)).not.toBeInTheDocument();
  });

  it('discards a late initial response for the previous employee', async () => {
    setRole('admin');
    let finish!: (value: AgentGoogleIdentity) => void;
    const pending = new Promise<AgentGoogleIdentity>(resolve => { finish = resolve; });
    vi.spyOn(api.googleIdentity, 'get')
      .mockReturnValueOnce(pending)
      .mockResolvedValueOnce({ ...HOME, agent_id: 'other', message: 'Scope B only' });
    const { rerender } = renderWithProviders(<AgentConnectionsTab agentId="bot" />);
    rerender(<AgentConnectionsTab agentId="other" />);
    await screen.findByText('Scope B only');
    await act(async () => { finish({ ...HOME, message: 'Scope A only', owners }); await pending; });
    expect(screen.getByText('Scope B only')).toBeInTheDocument();
    expect(screen.queryByText('Scope A only')).not.toBeInTheDocument();
  });

  it.each(['account', 'status', 'bindings'] as const)('immediately clears old candidates and confirmation after an Admin %s change', async (change) => {
    setRole('admin');
    vi.spyOn(api.googleIdentity, 'get')
      .mockResolvedValueOnce({ ...HOME, message: 'Old actor only', owners })
      .mockReturnValue(new Promise<AgentGoogleIdentity>(() => {}));
    renderWithProviders(<AgentConnectionsTab agentId="bot" />);
    const user = userEvent.setup();
    await user.click((await screen.findAllByRole('button', { name: 'Set primary owner' }))[1]);
    await screen.findByRole('dialog', { name: 'Change primary owner?' });
    act(() => {
      const current = useAuthStore.getState().user!;
      if (change === 'account') useAuthStore.setState({user: {...current, id: 'u2', email: 'u2@test.invalid'}});
      if (change === 'status') useAuthStore.setState({user: {...current, status: 'suspended'}});
      if (change === 'bindings') useAuthStore.setState({bindings: [{user_id: current.id, agent_name: 'bot', access_level: 'viewer', bound_at: 'changed'}]});
    });
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.queryByText('Old actor only')).not.toBeInTheDocument();
    expect(screen.queryByText(/Alice/)).not.toBeInTheDocument();
  });

  it('explains browser reset and waits for confirmation before changing the primary owner', async () => {
    setRole('admin');
    vi.spyOn(api.googleIdentity, 'get').mockResolvedValue({ ...HOME, owners: [
      { user_id: 'alice', name: 'Alice', email: 'alice@test.invalid', is_primary: true },
      { user_id: 'bob', name: 'Bob', email: 'bob@test.invalid', is_primary: false },
    ] });
    const primary = vi.spyOn(api.personalGoogle, 'primaryOwner').mockResolvedValue({});
    renderWithProviders(<AgentConnectionsTab agentId="bot" />);
    const user = userEvent.setup();
    const buttons = await screen.findAllByRole('button', { name: 'Set primary owner' });
    expect(buttons[0]).toBeDisabled();
    await user.click(buttons[1]);
    expect(primary).not.toHaveBeenCalled();
    let dialog = await screen.findByRole('dialog', { name: 'Change primary owner?' });
    expect(dialog).toHaveTextContent('Bob');
    expect(dialog).toHaveTextContent('stops the employee’s active browser sessions');
    expect(dialog).toHaveTextContent('clears saved browser sign-ins');
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(primary).not.toHaveBeenCalled();
    await user.click(buttons[1]);
    dialog = await screen.findByRole('dialog', { name: 'Change primary owner?' });
    await user.click(within(dialog).getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(primary).toHaveBeenCalledExactlyOnceWith('bot', 'bob'));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

  it('discards a pending primary-owner confirmation when the employee changes', async () => {
    setRole('admin');
    vi.spyOn(api.googleIdentity, 'get').mockResolvedValue({ ...HOME, owners: [
      { user_id: 'alice', name: 'Alice', email: 'alice@test.invalid', is_primary: true },
      { user_id: 'bob', name: 'Bob', email: 'bob@test.invalid', is_primary: false },
    ] });
    const primary = vi.spyOn(api.personalGoogle, 'primaryOwner').mockResolvedValue({});
    const { rerender } = renderWithProviders(<AgentConnectionsTab agentId="bot" />);
    const user = userEvent.setup();
    await user.click((await screen.findAllByRole('button', { name: 'Set primary owner' }))[1]);
    await screen.findByRole('dialog', { name: 'Change primary owner?' });
    rerender(<AgentConnectionsTab agentId="other" />);
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(primary).not.toHaveBeenCalled();
  });

  it('requires an Owner to acknowledge shared access before selecting their own account', async()=>{
    setRole('employee');
    useAuthStore.setState({bindings:[{user_id:'u1',agent_name:'bot',access_level:'owner',bound_at:'now'}]});
    vi.spyOn(api.googleIdentity,'get').mockResolvedValue({...HOME,bound_people:[{user_id:'bob',name:'Bob',email:'bob@example.com'}],allowed_channels:['line']});
    const set=vi.spyOn(api.googleIdentity,'set').mockResolvedValue({...HOME,kind:'user',identity:'user:u1',state:'configured'});
    renderWithProviders(<AgentConnectionsTab agentId="bot"/>);
    const user=userEvent.setup();
    await user.click(await screen.findByRole('button',{name:'Change'}));
    expect(screen.getByText(/Bob/)).toBeInTheDocument();
    expect(screen.getByRole('button',{name:'Save'})).toBeDisabled();
    await user.click(screen.getByRole('checkbox',{name:/checked who can use/i}));
    await user.click(screen.getByRole('button',{name:'Save'}));
    await waitFor(()=>expect(set).toHaveBeenCalledWith({agent_id:'bot',identity:'user:u1',fallback:'none',requesters:'bound_people',confirm_shared_access:true}));
  });
  it('shows the identity in effect and hides changes from non-admins', async () => {
    setRole('employee');
    vi.spyOn(api.googleIdentity, 'get').mockResolvedValue({
      ...HOME,
      kind: 'delegate',
      email: 'alice@customer.com',
      state: 'blocked',
      message: '公司授權代表還沒匯入服務帳號金鑰。',
    });
    renderWithProviders(<AgentConnectionsTab agentId="bot" />);
    await waitFor(() => expect(screen.getByTestId('agent-connections-current')).toHaveTextContent('alice@customer.com'));
    expect(screen.getByText('公司授權代表還沒匯入服務帳號金鑰。')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /變更|Change/ })).not.toBeInTheDocument();
  });

  it('lets an admin switch to a delegate and sends the selector', async () => {
    setRole('admin');
    vi.spyOn(api.googleIdentity, 'get').mockResolvedValue(HOME);
    const set = vi.spyOn(api.googleIdentity, 'set').mockResolvedValue({
      ...HOME,
      kind: 'delegate',
      email: 'bob@customer.com',
      state: 'ready',
    });
    renderWithProviders(<AgentConnectionsTab agentId="bot" />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /變更|Change/ }));
    await user.click(screen.getByRole('radio', { name: /代表某位同事|Act as a colleague/ }));
    await user.type(screen.getByLabelText(/同事的公司信箱|work email/), 'bob@customer.com');
    await user.click(screen.getByRole('button', { name: /儲存|Save/ }));
    await waitFor(() =>
      expect(set).toHaveBeenCalledWith({ agent_id: 'bot', identity: 'delegate:bob@customer.com', fallback: 'none', requesters: 'bound_people' }),
    );
    await waitFor(() => expect(screen.getByTestId('agent-connections-current')).toHaveTextContent('bob@customer.com'));
  });

  it('preserves owner_only when saving an existing delegate', async () => {
    setRole('admin');
    const view = { ...HOME, kind: 'delegate' as const, email: 'alice@c.com', requesters: 'owner_only' as const };
    vi.spyOn(api.googleIdentity, 'get').mockResolvedValue(view);
    const set = vi.spyOn(api.googleIdentity, 'set').mockResolvedValue(view);
    renderWithProviders(<AgentConnectionsTab agentId="bot" />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /變更|Change/ }));
    await user.click(screen.getByRole('button', { name: /儲存|Save/ }));
    await waitFor(() => expect(set).toHaveBeenCalledWith(expect.objectContaining({ requesters: 'owner_only' })));
  });

  it('lists every check of the connection test', async () => {
    setRole('admin');
    vi.spyOn(api.googleIdentity, 'get').mockResolvedValue({ ...HOME, kind: 'delegate', email: 'a@c.com', state: 'ready' });
    vi.spyOn(api.googleIdentity, 'test').mockResolvedValue({
      agent_id: 'bot',
      email: 'a@c.com',
      ok: false,
      steps: [
        { id: 'identity', label: '身分設定', ok: true, detail: '公司授權代表 a@c.com' },
        { id: 'authorize', label: '授權清單完整', ok: false, detail: 'Google 管理員還沒授權這組服務' },
      ],
    });
    renderWithProviders(<AgentConnectionsTab agentId="bot" />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: /測試連線|Test connection/ }));
    const list = await screen.findByTestId('agent-connections-steps');
    expect(list).toHaveTextContent('授權清單完整');
    expect(list).toHaveTextContent('Google 管理員還沒授權這組服務');
  });
});
