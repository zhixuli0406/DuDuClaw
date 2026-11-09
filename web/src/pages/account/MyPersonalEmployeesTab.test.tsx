import { mockWsClient } from '@/test/mocks';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { authenticatePasskey } from '@/lib/passkey';
import type { PersonalEmployeeRequest, RestorableEmployee, RestoreChallenge } from '@/lib/isolation-restore-api';
import { MyPersonalEmployeesTab } from './MyPersonalEmployeesTab';

vi.mock('@/lib/passkey', () => ({
  authenticatePasskey: vi.fn(),
  passkeyAvailable: () => true,
}));

const LONG_TASK = `Summarise the Q3 numbers\n${'x'.repeat(5000)}\n‮evil‬ end of task`;

const applied: PersonalEmployeeRequest = {
  request_id: 'r1', label: 'alice-helper', display_name: 'Alice Helper',
  created_at: '2026-11-01T00:00:00Z', status: 'applied', stage: 'proposed',
};
const DAY = 86_400_000;
// `me.isolation.list_restorable` row (backend contract field names).
const restorable: RestorableEmployee[] = [
  { employee_id: 'e1', incarnation: 2, label: 'old-helper', display_name: 'Old Helper', kind: 'personal',
    owner_user_id: 'u1', seal: 'sealed', restorable: true, root_flag: null, open_request: null,
    tombstoned_at: new Date(Date.now() - 3 * DAY).toISOString(),
    restore_deadline: new Date(Date.now() + 12 * DAY - 60_000).toISOString() },
];

interface World {
  requests: PersonalEmployeeRequest[];
  employees: RestorableEmployee[];
  challenge: RestoreChallenge | (() => never);
  requestRestore: (params: Record<string, unknown>) => unknown;
  delegations: unknown[];
}
let world: World;

function serve() {
  mockWsClient.call.mockImplementation(async (method: string, params: Record<string, unknown> = {}) => {
    switch (method) {
      case 'me.isolation.list_personal_employee_requests': return { requests: world.requests };
      case 'me.isolation.list_restorable': return { restorable: world.employees };
      case 'me.isolation.list_personal_delegations': return { delegations: world.delegations };
      case 'me.isolation.decide_personal_delegation': return { id: params.id, decided: params.approve ? 'approved' : 'denied' };
      case 'me.isolation.restore_challenge': return typeof world.challenge === 'function' ? world.challenge() : world.challenge;
      case 'me.isolation.request_restore': return world.requestRestore(params);
      case 'me.isolation.request_personal_employee': return { ...params, request_id: 'new', status: 'pending', stage: 'requested', approval_required: true };
      case 'me.google.status': return {
        connection: { owner_id: 'u1', account_email: 'alice@corp.example', granted_scopes: [], status: 'connected', connected_at: '2026-10-01T00:00:00Z' },
        login_email: 'alice@corp.example', client_configured: true, integration_enabled: true, requires_login: false,
        requests: [], delegated: [{ agent_id: 'alice-helper', email: 'shared-box@corp.example' }], recent_usage: [],
      };
      default: return null;
    }
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  world = {
    requests: [applied],
    employees: restorable,
    challenge: { factor: 'totp', challenge_id: 'c1' },
    requestRestore: () => ({ request_id: 'rr1', state: 'pending', label: 'old-helper', expires_at: null, approval_required: true, notified: 2 }),
    delegations: [{ id: 'd1', task: LONG_TASK, source_agent: 'alice-helper', target_agent: 'sales-shared', created_at: 1762600000, expires_at: 1762700000 }],
  };
  serve();
});

async function openRestore(user: ReturnType<typeof userEvent.setup>) {
  // Board "Mine": the days left sit in the row's meta line, the button says 申請還原.
  expect(await screen.findByText(/12 days left to ask for a restore/)).toBeInTheDocument();
  await user.click(screen.getByRole('button', { name: 'Ask to restore' }));
}

describe('我的個人 AI 員工 tab', () => {
  it('shows my working employee with my own and company-shared accounts, and links to reconnect Google', async () => {
    renderWithProviders(<MyPersonalEmployeesTab />);
    expect(await screen.findByRole('link', { name: 'Alice Helper' })).toHaveAttribute('href', '/agents/alice-helper');
    expect(await screen.findByText('alice@corp.example')).toBeInTheDocument();
    expect(screen.getByText('Company-shared account')).toBeInTheDocument();
    expect(screen.getByText('shared-box@corp.example')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Reconnect Google' })).toHaveAttribute('href', '/me/connections');
  });

  it('shows the full delegation task, never shortened, with direction controls made visible', async () => {
    renderWithProviders(<MyPersonalEmployeesTab />);
    const region = await screen.findByRole('region', { name: 'Full task that would be handed over' });
    expect(region.textContent).toContain('x'.repeat(5000));
    expect(region.textContent).toContain('end of task');
    expect(region.textContent).toContain('[U+202E]');
    expect(region.textContent).not.toContain('‮');
    expect(region).toHaveAttribute('dir', 'auto');
    expect(region.className).toContain('whitespace-pre-wrap');
    expect(region.className).not.toMatch(/truncate|line-clamp/);
    expect(screen.getByText(/hidden characters that change text direction/)).toBeInTheDocument();
    expect(screen.getByText(/"sales-shared" is a shared AI employee\. Other people who can use it may be able to see/)).toBeInTheDocument();
  });

  it('agreeing decides the card as the owner and removes it', async () => {
    const user = userEvent.setup();
    renderWithProviders(<MyPersonalEmployeesTab />);
    await user.click(await screen.findByRole('button', { name: 'Agree' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('me.isolation.decide_personal_delegation', { id: 'd1', approve: true }));
    await waitFor(() => expect(screen.queryByRole('region', { name: 'Full task that would be handed over' })).toBeNull());
  });

  it('restore request: lists what comes back and proves it with the authenticator code', async () => {
    const user = userEvent.setup();
    renderWithProviders(<MyPersonalEmployeesTab />);
    await openRestore(user);
    expect(screen.getByText('What comes back')).toBeInTheDocument();
    expect(screen.getByText('An admin needs to set up again')).toBeInTheDocument();
    expect(screen.getByText(/stays paused after the restore/)).toBeInTheDocument();
    expect(screen.getByText(/If someone else changed the sign-in details/)).toBeInTheDocument();
    await user.type(await screen.findByLabelText('Verification code'), '123456');
    world.employees = [{ ...restorable[0], open_request: { kind: 'restore' } }];
    await user.click(screen.getByRole('button', { name: 'Send restore request' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('me.isolation.request_restore', {
      employee_id: 'e1', incarnation: 2, factor: 'totp', code: '123456', challenge_id: 'c1',
    }));
    expect(await screen.findByText('Restore request sent. Waiting for an admin to approve.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Withdraw request' })).toBeInTheDocument();
  });

  it('restore request: passkey when the server asks for it', async () => {
    world.challenge = { factor: 'passkey', challenge_id: 'c2', publicKey: { challenge: 'Y2hhbGxlbmdl' } };
    vi.mocked(authenticatePasskey).mockResolvedValue({ id: 'cred', rawId: 'cred', type: 'public-key', extensions: {}, response: {} } as never);
    const user = userEvent.setup();
    renderWithProviders(<MyPersonalEmployeesTab />);
    await openRestore(user);
    await user.click(await screen.findByRole('button', { name: 'Use passkey: Send restore request' }));
    expect(authenticatePasskey).toHaveBeenCalledWith({ challenge: 'Y2hhbGxlbmdl' });
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('me.isolation.request_restore', expect.objectContaining({
      employee_id: 'e1', incarnation: 2, factor: 'passkey', challenge_id: 'c2', credential: expect.objectContaining({ id: 'cred' }),
    })));
  });

  it('restore request: email code when that is the strongest factor', async () => {
    world.challenge = { factor: 'email', challenge_id: 'c3', email_hint: 'a***@corp.example' };
    const user = userEvent.setup();
    renderWithProviders(<MyPersonalEmployeesTab />);
    await openRestore(user);
    expect(await screen.findByText('We sent a 6-digit code to a***@corp.example.')).toBeInTheDocument();
    await user.type(screen.getByLabelText('Email code'), '654321');
    await user.click(screen.getByRole('button', { name: 'Send restore request' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('me.isolation.request_restore', {
      employee_id: 'e1', incarnation: 2, factor: 'email', code: '654321', challenge_id: 'c3',
    }));
  });

  it('no second factor before the tombstone: tells the owner to contact an admin', async () => {
    world.challenge = { factor: 'none' };
    const user = userEvent.setup();
    renderWithProviders(<MyPersonalEmployeesTab />);
    await openRestore(user);
    expect(await screen.findByText('Please contact an admin')).toBeInTheDocument();
    expect(screen.queryByLabelText('Verification code')).toBeNull();
  });

  it('maps a refused factor to friendly text and keeps the server sentence', async () => {
    world.requestRestore = () => Promise.reject({ code: 'FACTOR_NOT_STRONGEST', message: '你有通行金鑰，請改用通行金鑰。' });
    const user = userEvent.setup();
    renderWithProviders(<MyPersonalEmployeesTab />);
    await openRestore(user);
    await user.type(await screen.findByLabelText('Verification code'), '123456');
    await user.click(screen.getByRole('button', { name: 'Send restore request' }));
    const alert = await screen.findByRole('alert');
    expect(within(alert).getByText(/Please use your most secure sign-in check/)).toBeInTheDocument();
    expect(within(alert).getByText('你有通行金鑰，請改用通行金鑰。')).toBeInTheDocument();
  });

  it('a credential changed by someone else blocks the restore and sends the owner to an admin', async () => {
    world.requestRestore = () => Promise.reject({ code: 'CREDENTIAL_CHANGED', message: '登入資料已被他人變更。' });
    const user = userEvent.setup();
    renderWithProviders(<MyPersonalEmployeesTab />);
    await openRestore(user);
    await user.type(await screen.findByLabelText('Verification code'), '123456');
    await user.click(screen.getByRole('button', { name: 'Send restore request' }));
    expect(await screen.findByText('Someone changed the sign-in details. Please contact an admin')).toBeInTheDocument();
  });

  it('new personal employee: checks the name rules and sends the request', async () => {
    const user = userEvent.setup();
    renderWithProviders(<MyPersonalEmployeesTab />);
    const label = await screen.findByLabelText('Employee ID');
    const submit = screen.getByRole('button', { name: 'Send request' });
    await user.type(label, '-bad-');
    await user.type(screen.getByLabelText('Display name'), 'My Helper');
    expect(label).toHaveAttribute('aria-invalid', 'true');
    expect(submit).toBeDisabled();
    await user.clear(label);
    await user.type(label, 'my-helper');
    expect(submit).toBeEnabled();
    await user.click(submit);
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('me.isolation.request_personal_employee', {
      label: 'my-helper', display_name: 'My Helper',
    }));
  });

  it('new personal employee: at most three waiting requests', async () => {
    world.requests = [1, 2, 3].map((n) => ({ ...applied, request_id: `p${n}`, label: `wait-${n}`, display_name: `Wait ${n}`, status: 'pending' as const, stage: 'requested' as const }));
    renderWithProviders(<MyPersonalEmployeesTab />);
    expect(await screen.findByText('You already have 3 waiting requests. Please wait for an admin before asking again.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Send request' })).toBeDisabled();
    expect(screen.getAllByText('Waiting for approval')).toHaveLength(3);
  });
});
