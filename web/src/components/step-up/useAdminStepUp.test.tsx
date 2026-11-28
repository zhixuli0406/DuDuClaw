import { mockWsClient } from '@/test/mocks';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { useAdminStepUp } from './useAdminStepUp';
import type { StepUpToken } from '@/lib/step-up';

const TOKEN = Array(32).fill(7);
let hook: ReturnType<typeof useAdminStepUp>;
function Host() {
  hook = useAdminStepUp();
  return <>{hook.dialog}</>;
}
beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ verified: true, step_up_csrf: TOKEN });
});
afterEach(() => vi.restoreAllMocks());

async function verify() {
  const user = userEvent.setup();
  await user.type(screen.getByLabelText('Current password'), 'pw');
  await user.type(screen.getByLabelText('Verification code'), '123456');
  await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
}
function ensure() {
  let promise!: Promise<StepUpToken | null>;
  act(() => { promise = hook.ensure('Update access'); });
  return promise;
}

it('reuses the cached token within the window', async () => {
  renderWithProviders(<Host />);
  const first = ensure();
  await verify();
  expect(await first).toEqual(TOKEN);
  await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  expect(await hook.ensure()).toEqual(TOKEN);
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(mockWsClient.call).toHaveBeenCalledTimes(1);
});

it('opens again after invalidate and cancellation resolves null', async () => {
  renderWithProviders(<Host />);
  const first = ensure();
  await verify();
  await first;
  act(() => hook.invalidate());
  const second = ensure();
  expect(await screen.findByRole('dialog')).toBeInTheDocument();
  await userEvent.setup().click(screen.getByRole('button', { name: 'Cancel' }));
  expect(await second).toBeNull();
});

it('expires the cache at the 4.5 minute boundary', async () => {
  const now = vi.spyOn(Date, 'now').mockReturnValue(1000);
  renderWithProviders(<Host />);
  const first = ensure();
  await verify();
  await first;
  now.mockReturnValue(1000 + 270000);
  const second = ensure();
  expect(await screen.findByRole('dialog')).toBeInTheDocument();
  await userEvent.setup().click(screen.getByRole('button', { name: 'Cancel' }));
  expect(await second).toBeNull();
});

it('shares pending requests and resolves them on unmount', async () => {
  const { unmount } = renderWithProviders(<Host />);
  const first = ensure();
  const second = hook.ensure();
  expect(second).toBe(first);
  unmount();
  expect(await first).toBeNull();
});
