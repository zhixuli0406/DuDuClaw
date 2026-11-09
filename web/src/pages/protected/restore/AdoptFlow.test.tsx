import { mockWsClient } from '@/test/mocks';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import type { RestorableEmployee } from '@/lib/isolation-restore-api';
import { AdoptFlow } from './AdoptFlow';

// `me.isolation.list_restorable` row whose runtime is gone (`root_flag: ROOT_GONE`).
const GONE: RestorableEmployee = {
  employee_id: 'e2', incarnation: 1, label: 'gone-helper', display_name: 'Gone Helper', kind: 'personal',
  restorable: false, root_flag: 'ROOT_GONE', restore_deadline: new Date(Date.now() + 30 * 86_400_000).toISOString(),
};

let challenge: () => unknown;
beforeEach(() => {
  vi.clearAllMocks();
  challenge = () => ({ factor: 'email', challenge_id: 'c3', sent: true, expires_in: 300 });
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'me.isolation.restore_challenge') return challenge();
    if (method === 'me.isolation.request_adoption') return { request_id: 'ad1', state: 'pending' };
    return null;
  });
});

describe('<AdoptFlow> — 由新員工接手 (ROOT_GONE)', () => {
  it('explains what is carried, defaults the new name, and asks the adoption challenge', async () => {
    renderWithProviders(<AdoptFlow employee={GONE} onFiled={vi.fn()} />);
    expect(screen.getByText(/original workspace no longer exists/)).toBeInTheDocument();
    expect(screen.getByText('Goes to the new employee')).toBeInTheDocument();
    expect(screen.getByText('Does not go over')).toBeInTheDocument();
    expect(screen.getByLabelText('New employee ID')).toHaveValue('gone-helper');
    const steps = screen.getByRole('list', { name: 'Hand-over progress' });
    expect(steps.querySelector('[aria-current="step"]')?.textContent).toContain('Confirm it is you');
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('me.isolation.restore_challenge', {
      employee_id: 'e2', incarnation: 1, for_adoption: true,
    }));
  });

  it('sends the adoption with the new name and the email code, then shows the admin step', async () => {
    const onFiled = vi.fn();
    const user = userEvent.setup();
    renderWithProviders(<AdoptFlow employee={GONE} onFiled={onFiled} />);
    const label = screen.getByLabelText('New employee ID');
    await user.clear(label);
    await user.type(label, 'gone-helper-2');
    await user.type(await screen.findByLabelText('Email code'), '654321');
    await user.click(screen.getByRole('button', { name: 'Send hand-over request' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('me.isolation.request_adoption', {
      employee_id: 'e2', incarnation: 1, target_label: 'gone-helper-2', factor: 'email', code: '654321', challenge_id: 'c3',
    }));
    expect(onFiled).toHaveBeenCalled();
    expect(await screen.findByText(/Hand-over request sent/)).toBeInTheDocument();
    const steps = screen.getByRole('list', { name: 'Hand-over progress' });
    expect(steps.querySelector('[aria-current="step"]')?.textContent).toContain('Admin approves');
  });

  it('keeps the original name when unchanged (no target_label sent)', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AdoptFlow employee={GONE} onFiled={vi.fn()} />);
    await user.type(await screen.findByLabelText('Email code'), '111111');
    await user.click(screen.getByRole('button', { name: 'Send hand-over request' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('me.isolation.request_adoption', {
      employee_id: 'e2', incarnation: 1, factor: 'email', code: '111111', challenge_id: 'c3',
    }));
  });

  it('an invalid new name blocks sending', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AdoptFlow employee={GONE} onFiled={vi.fn()} />);
    const label = screen.getByLabelText('New employee ID');
    await user.clear(label);
    await user.type(await screen.findByLabelText('Email code'), '111111');
    expect(screen.getByRole('button', { name: 'Send hand-over request' })).toBeDisabled();
  });

  it('a factor changed after the tombstone sends the owner to an admin', async () => {
    challenge = () => { throw { code: 'FACTOR_NOT_PREEXISTING', message: '驗證方式在註銷後變更過。' }; };
    renderWithProviders(<AdoptFlow employee={GONE} onFiled={vi.fn()} />);
    expect(await screen.findByText('Your check changed after the removal. Please contact an admin')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Send hand-over request' })).toBeNull();
  });

  it.each(['FACTOR_NOT_ENROLLED', 'FACTOR_BASELINE_MISSING', 'FACTOR_DENIED'])('%s sends the owner to an admin', async (code) => {
    challenge = () => { throw { code, message: '請聯絡管理員。' }; };
    renderWithProviders(<AdoptFlow employee={GONE} onFiled={vi.fn()} />);
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Send hand-over request' })).toBeNull();
  });

  it.each(['FACTOR_RATE_LIMITED', 'FACTOR_UNAVAILABLE'])('%s is retryable, not an admin matter', async (code) => {
    challenge = () => { throw { code, message: '請稍後再試。' }; };
    renderWithProviders(<AdoptFlow employee={GONE} onFiled={vi.fn()} />);
    expect(await screen.findByRole('button', { name: 'Try again' })).toBeInTheDocument();
  });

  it('authenticator-app factor', async () => {
    challenge = () => ({ factor: 'totp', challenge_id: null });
    renderWithProviders(<AdoptFlow employee={GONE} onFiled={vi.fn()} />);
    expect(await screen.findByText('Confirm with your authenticator app')).toBeInTheDocument();
    expect(screen.getByLabelText('Verification code')).toBeInTheDocument();
  });

  it('an open adoption request shows the waiting state', () => {
    renderWithProviders(<AdoptFlow employee={{ ...GONE, open_request: { kind: 'adoption' } }} onFiled={vi.fn()} />);
    expect(screen.getByText(/Hand-over request sent/)).toBeInTheDocument();
    expect(screen.queryByLabelText('New employee ID')).toBeNull();
  });
});
