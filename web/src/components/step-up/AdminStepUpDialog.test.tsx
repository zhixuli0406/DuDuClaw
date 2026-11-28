import { mockWsClient } from '@/test/mocks';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { stepUpApi } from '@/lib/step-up';
import { AdminStepUpDialog } from './AdminStepUpDialog';

const TOKEN = Array(32).fill(9);
beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ verified: true, step_up_csrf: TOKEN });
});

async function submit(code = '123456') {
  const user = userEvent.setup();
  await user.type(screen.getByLabelText('Current password'), 'pw');
  await user.type(screen.getByLabelText('Verification code'), code);
  await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
}

describe('AdminStepUpDialog', () => {
  it('returns the direct token and clears both fields without listing employees', async () => {
    const onDone = vi.fn();
    renderWithProviders(<AdminStepUpDialog open reason="Change sign-in options" onDone={onDone} />);
    expect(screen.getByText('Change sign-in options')).toBeInTheDocument();
    await submit();
    await waitFor(() => expect(onDone).toHaveBeenCalledWith(TOKEN));
    expect(screen.getByLabelText('Current password')).toHaveValue('');
    expect(screen.getByLabelText('Verification code')).toHaveValue('');
    expect(mockWsClient.call.mock.calls).toEqual([['isolation.connector_step_up', { password: 'pw', code: '123456' }]]);
  });

  it('shows a plain error for a wrong code and clears both fields', async () => {
    mockWsClient.call.mockRejectedValue('驗證失敗，請重試。');
    const onDone = vi.fn();
    renderWithProviders(<AdminStepUpDialog open onDone={onDone} />);
    await submit('000000');
    expect(await screen.findByRole('alert')).toHaveTextContent('That did not work. Check your password and enter a new code.');
    expect(screen.getByLabelText('Current password')).toHaveValue('');
    expect(screen.getByLabelText('Verification code')).toHaveValue('');
    expect(onDone).not.toHaveBeenCalled();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('isolation.employee_list', expect.anything());
  });

  it.each([null, {}, { verified: false, step_up_csrf: TOKEN }, { verified: true },
    { verified: true, step_up_csrf: [1] }, { verified: true, step_up_csrf: Array(32).fill(256) },
    { verified: true, step_up_csrf: Array(32).fill(1.5) }])('rejects malformed replies: %j', async (reply) => {
    mockWsClient.call.mockResolvedValue(reply);
    await expect(stepUpApi.adminVerify('pw', '123456')).rejects.toThrow();
  });

  it('ignores a verification reply arriving after cancellation', async () => {
    let resolve!: (reply: unknown) => void;
    mockWsClient.call.mockReturnValue(new Promise((done) => { resolve = done; }));
    const onDone = vi.fn();
    renderWithProviders(<AdminStepUpDialog open onDone={onDone} />);
    await submit();
    await userEvent.setup().click(screen.getByRole('button', { name: 'Cancel' }));
    resolve({ verified: true, step_up_csrf: TOKEN });
    await waitFor(() => expect(screen.getByLabelText('Current password')).toHaveValue(''));
    expect(onDone.mock.calls).toEqual([[null]]);
  });
});
