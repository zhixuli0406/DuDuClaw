import { mockWsClient } from '@/test/mocks';
import { StrictMode } from 'react';
import { beforeEach, expect, it, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import zhTW from '@/i18n/zh-TW.json';
import { renderWithProviders } from '@/test/render';
import { useUserNames } from './use-user-names';

const IDS = ['secret-id-a', 'secret-id-b', 'secret-id-c'];
let hook: ReturnType<typeof useUserNames>;
function Host() {
  hook = useUserNames();
  return <>{IDS.map((id) => <span key={id}>{hook.nameOf(id)}</span>)}</>;
}
function host() {
  return <IntlProvider locale="zh-TW" messages={zhTW}><Host /></IntlProvider>;
}
beforeEach(() => vi.clearAllMocks());
it('returns names, falls back to email, and labels unknown members without exposing ids', async () => {
  mockWsClient.call.mockResolvedValue({ users: [
    { id: IDS[0], display_name: '小明', email: 'ming@example.com' },
    { id: IDS[1], display_name: '  ', email: 'member@example.com' },
  ] });
  const { rerender } = renderWithProviders(<StrictMode>{host()}</StrictMode>);
  expect(await screen.findByText('小明')).toBeInTheDocument();
  expect(screen.getByText('member@example.com')).toBeInTheDocument();
  expect(screen.getByText('已移除的成員')).toBeInTheDocument();
  expect(hook.loading).toBe(false);
  expect(hook.failed).toBe(false);
  expect(hook.nameOf(null)).toBe('');
  expect(hook.nameOf(undefined)).toBe('');
  for (const id of IDS) expect(hook.nameOf(id)).not.toBe(id);
  rerender(<StrictMode>{host()}</StrictMode>);
  expect(mockWsClient.call.mock.calls).toEqual([['users.list']]);
});
it('returns empty names while loading', () => {
  mockWsClient.call.mockReturnValue(new Promise(() => {}));
  renderWithProviders(host());
  expect(hook.loading).toBe(true);
  for (const id of IDS) expect(hook.nameOf(id)).toBe('');
});
it('returns a plain failure message without exposing ids', async () => {
  mockWsClient.call.mockRejectedValue(new Error('Unavailable'));
  renderWithProviders(host());
  await waitFor(() => expect(hook.failed).toBe(true));
  expect(hook.loading).toBe(false);
  for (const id of IDS) {
    expect(hook.nameOf(id)).toBe('無法讀取成員名稱');
    expect(screen.queryByText(id)).toBeNull();
  }
  expect(hook.nameOf(null)).toBe('');
});
