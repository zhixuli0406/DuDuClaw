import { mockWsClient } from '@/test/mocks';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { forgetBreakGlass } from '@/lib/break-glass';
import { ApprovalRequestCard } from '@/components/console/ApprovalRequestCard';
import type { ApprovalItem } from '@/lib/api';
import { StepUpDialog } from './StepUpDialog';
import { BreakGlassPromptHost } from './BreakGlassPromptHost';

const TOKEN = Array(32).fill(9);
const DECIDE_REFUSAL = '這張審核卡屬於個人專屬員工。管理員需先以密碼與雙因素驗證確認身分（有效 5 分鐘），並在決定請求中帶入驗證後的 step_up_csrf；每次決定都會寫入稽核紀錄。';

let verified = false;
function serve(extra?: (method: string, params: Record<string, unknown>) => unknown) {
  mockWsClient.call.mockImplementation(async (method: string, params: Record<string, unknown> = {}) => {
    const answer = extra?.(method, params);
    if (answer !== undefined) return answer;
    if (method === 'isolation.employee_list') {
      return verified ? { step_up_valid: true, csrf_token: TOKEN, mfa_enrolled: true } : { step_up_valid: false, csrf_token: null, mfa_enrolled: true };
    }
    if (method === 'isolation.connector_step_up') {
      if (params.code !== '123456') throw '驗證失敗，請確認密碼與新的驗證碼後重試。';
      verified = true;
      return { verified: true };
    }
    return null;
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  verified = false;
  serve();
});
afterEach(() => forgetBreakGlass());

describe('<StepUpDialog>', () => {
  it('verifies password + code, then hands back the session token', async () => {
    const onDone = vi.fn();
    const user = userEvent.setup();
    renderWithProviders(<StepUpDialog open purpose="admin" onDone={onDone} />);
    await user.type(await screen.findByLabelText('Current password'), 'pw');
    await user.type(screen.getByLabelText('Verification code'), '123456');
    await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
    await waitFor(() => expect(onDone).toHaveBeenCalledWith(TOKEN));
    expect(mockWsClient.call).toHaveBeenCalledWith('isolation.connector_step_up', { password: 'pw', code: '123456' });
  });

  it('shows a plain failure and clears the secret fields', async () => {
    const onDone = vi.fn();
    const user = userEvent.setup();
    renderWithProviders(<StepUpDialog open purpose="admin" onDone={onDone} />);
    const password = await screen.findByLabelText('Current password');
    await user.type(password, 'pw');
    await user.type(screen.getByLabelText('Verification code'), '000000');
    await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
    expect(await screen.findByText('That did not work. Check your password and enter a new code.')).toBeInTheDocument();
    expect(password).toHaveValue('');
    expect(onDone).not.toHaveBeenCalled();
  });

  it('asks for explicit confirmation on a break-glass read even when the proof is still valid', async () => {
    verified = true;
    const onDone = vi.fn();
    const user = userEvent.setup();
    renderWithProviders(<StepUpDialog open purpose="read" onDone={onDone} />);
    expect(await screen.findByText(/You confirmed your identity in the last 5 minutes/)).toBeInTheDocument();
    expect(screen.getByText('Every view or change is recorded.')).toBeInTheDocument();
    expect(onDone).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Confirm and continue' }));
    expect(onDone).toHaveBeenCalledWith(TOKEN);
  });

  it('points to the account page when two-step verification is not set up', async () => {
    serve((method) => (method === 'isolation.employee_list' ? { step_up_valid: false, csrf_token: null, mfa_enrolled: false } : undefined));
    renderWithProviders(<StepUpDialog open purpose="decide" onDone={vi.fn()} />);
    expect(await screen.findByRole('link', { name: 'Go to Account' })).toHaveAttribute('href', '/me/account');
    expect(screen.queryByLabelText('Current password')).toBeNull();
  });

  it('cancel answers null', async () => {
    const onDone = vi.fn();
    const user = userEvent.setup();
    renderWithProviders(<StepUpDialog open purpose="admin" onDone={onDone} />);
    await screen.findByLabelText('Current password');
    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(onDone).toHaveBeenCalledWith(null);
  });
});

describe('P1 break-glass prompt wired into approvals.decide', () => {
  const ITEM: ApprovalItem = {
    id: 'appr-1', agent_id: 'alice-personal', kind: 'tool_call', summary: 'Send the weekly report',
    payload: {}, created_at: new Date().toISOString(), ttl_seconds: 300,
  };

  it('asks 需要再驗證身分 on the step-up refusal and retries once with step_up_csrf', async () => {
    serve((method, params) => {
      if (method !== 'approvals.decide') return undefined;
      if (!params.step_up_csrf) return Promise.reject(DECIDE_REFUSAL);
      return { id: 'appr-1', decided: 'approved' };
    });
    const user = userEvent.setup();
    renderWithProviders(<><BreakGlassPromptHost /><ApprovalRequestCard payload={ITEM} /></>);
    await user.click(screen.getByRole('button', { name: 'Approve' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText(/This approval belongs to a colleague's personal AI employee/)).toBeInTheDocument();
    await user.type(within(dialog).getByLabelText('Current password'), 'pw');
    await user.type(within(dialog).getByLabelText('Verification code'), '123456');
    await user.click(within(dialog).getByRole('button', { name: 'Verify and continue' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('approvals.decide', { id: 'appr-1', approve: true, step_up_csrf: TOKEN }));
    expect(await screen.findByText('Approved: Send the weekly report')).toBeInTheDocument();
    // First attempt carried no token.
    expect(mockWsClient.call).toHaveBeenCalledWith('approvals.decide', { id: 'appr-1', approve: true });
  });

  it('cancelling the prompt leaves the card undecided with a plain message', async () => {
    serve((method) => (method === 'approvals.decide' ? Promise.reject(DECIDE_REFUSAL) : undefined));
    const user = userEvent.setup();
    renderWithProviders(<><BreakGlassPromptHost /><ApprovalRequestCard payload={ITEM} /></>);
    await user.click(screen.getByRole('button', { name: 'Approve' }));
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.getByRole('button', { name: 'Approve' })).toBeEnabled());
    expect(screen.queryByText(/step_up_csrf/)).toBeNull();
  });
});
