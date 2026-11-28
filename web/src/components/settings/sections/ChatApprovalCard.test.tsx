import { beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { useSystemStore } from '@/stores/system-store';
import type { ChatAdminSettings } from '@/lib/api';
import en from '@/i18n/en.json';
import { ChatApprovalCard } from './ChatApprovalCard';

const kind = (key: string, name: string) => ({ key, label: { 'zh-TW': name, en: name, ja: name }, impact: '影響', tier: 'T2' as const, step_up_required: true });
const baseline = (): ChatAdminSettings => ({ enabled: false, channels: ['telegram'], step_up_methods: ['web'], card_valid_hours: 168, readable: true, hash: 'hash-1', enabled_ack: null, kinds: [kind('skill', '技能'), kind('cli_egress', '對外傳送'), kind('egress_guard', '對外傳送')], t1_kinds: [], admins: [{ user_id: 'secret-id', name: '王小明', proven_channels: [], has_totp: true }], web_link_available: true, unsupported_channels_ignored: false });
let data: ReturnType<typeof baseline>;
beforeEach(() => {
  vi.clearAllMocks();
  data = baseline();
  useSystemStore.setState({ status: null });
  mockWsClient.call.mockImplementation(async (method: string) => method === 'approvals.chat_admin.get' ? data : { success: true, hash: 'hash-2' });
});
async function mount() { renderWithProviders(<ChatApprovalCard />); await screen.findByText('王小明 · Not linked'); }
const save = () => fireEvent.click(screen.getByRole('button', { name: 'Save chat approval settings' }));
const toggle = () => screen.getByRole('switch', { name: 'Let admins approve in chat' });
async function exempt() {
  fireEvent.click(screen.getByRole('checkbox', { name: '對外傳送 Require extra confirmation' }));
  const dialog = screen.getByRole('dialog');
  fireEvent.click(within(dialog).getByRole('checkbox'));
  fireEvent.click(within(dialog).getByRole('button', { name: 'Remove extra confirmation' }));
}
it('loads checked groups and displays names without internal IDs', async () => {
  await mount();
  expect(screen.getAllByRole('checkbox', { name: /Require extra confirmation/ })).toHaveLength(2);
  for (const row of screen.getAllByRole('checkbox', { name: /Require extra confirmation/ })) expect(row).toBeChecked();
  expect(screen.queryByText('secret-id')).not.toBeInTheDocument();
  expect(mockWsClient.call).toHaveBeenCalledWith('approvals.chat_admin.get', {});
});
it('requires acknowledgement to enable and sends the full form', async () => {
  await mount(); fireEvent.click(toggle());
  const dialog = screen.getByRole('dialog');
  const confirm = within(dialog).getByRole('button', { name: 'Enable' });
  expect(confirm).toBeDisabled();
  fireEvent.click(within(dialog).getByRole('checkbox')); fireEvent.click(confirm);
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('approvals.chat_admin.set', { expected_hash: 'hash-1', enabled: true, channels: ['telegram'], step_up_methods: ['web'], card_valid_hours: 168, step_up_exempt_kinds: [], acknowledge_risk: true }));
});
it('disables immediately without acknowledgement', async () => {
  data.enabled = true; await mount(); fireEvent.click(toggle());
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('approvals.chat_admin.set', expect.objectContaining({ enabled: false })));
  const payload = mockWsClient.call.mock.calls.find(([method]) => method === 'approvals.chat_admin.set')![1];
  expect(payload).not.toHaveProperty('acknowledge_risk');
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
});
it('acknowledges exactly both grouped keys on save', async () => {
  await mount(); await exempt(); save();
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('approvals.chat_admin.set', expect.objectContaining({ step_up_exempt_kinds: ['cli_egress', 'egress_guard'], acknowledge_exempt_risk: ['cli_egress', 'egress_guard'] })));
});
it('rechecks without a dialog and omits exemption acknowledgement', async () => {
  await mount(); await exempt();
  fireEvent.click(screen.getByRole('checkbox', { name: '對外傳送 Require extra confirmation' }));
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument(); save();
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('approvals.chat_admin.set', { expected_hash: 'hash-1', enabled: false, channels: ['telegram'], step_up_methods: ['web'], card_valid_hours: 168, step_up_exempt_kinds: [] }));
});
it('validates methods, hours and enabled channels', async () => {
  await mount(); fireEvent.click(screen.getByRole('checkbox', { name: 'Web link' })); save();
  expect(screen.getByRole('alert')).toHaveTextContent('Select at least one extra confirmation method.');
  fireEvent.click(screen.getByRole('checkbox', { name: 'Web link' }));
  fireEvent.change(screen.getByRole('spinbutton'), { target: { value: '721' } }); save();
  expect(screen.getByRole('alert')).toHaveTextContent('Chat cards must be valid for 1 to 720 hours.');
  fireEvent.change(screen.getByRole('spinbutton'), { target: { value: '168' } });
  fireEvent.click(screen.getByRole('checkbox', { name: 'Telegram' })); fireEvent.click(toggle());
  fireEvent.click(within(screen.getByRole('dialog')).getByRole('checkbox'));
  fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Enable' }));
  expect(screen.getAllByRole('alert')[0]).toHaveTextContent('Select at least one channel.');
  expect(mockWsClient.call.mock.calls.some(([method]) => method === 'approvals.chat_admin.set')).toBe(false);
});
it('renders Personal as read-only with a single edition note', async () => {
  useSystemStore.setState({ status: { edition_profile: 'personal' } as never }); await mount();
  expect(screen.getByText('Chat approvals are not available in Personal.')).toBeInTheDocument();
  for (const input of [...screen.getAllByRole('checkbox'), screen.getByRole('spinbutton')]) expect(input).toBeDisabled();
  expect(toggle()).toHaveAttribute('aria-disabled', 'true');
  fireEvent.click(toggle());
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  expect(screen.queryByRole('link')).not.toBeInTheDocument();
});
it('shows unreadable state and blocks writes', async () => {
  data.readable = false; data.hash = null; await mount();
  expect(screen.getByText(/The admin who last enabled this setting/)).toBeInTheDocument();
  expect(toggle()).toHaveAttribute('aria-disabled', 'true'); expect(screen.getByRole('button', { name: 'Save chat approval settings' })).toBeDisabled();
});
it('shows revoked acknowledgement state', async () => {
  data.enabled_ack = { by_name: 'Former admin', at: '2026-11-27T06:03:00Z' }; await mount();
  expect(screen.getByText(/The admin who last enabled this setting/)).toBeInTheDocument();
});
it('retries failed loads and displays server save refusal', async () => {
  mockWsClient.call.mockRejectedValueOnce(new Error('Load refused'));
  renderWithProviders(<ChatApprovalCard />); await screen.findByText(en['errorState.reason.unknown']);
  expect(document.body.textContent).not.toContain('Load refused');
  fireEvent.click(screen.getByRole('button', { name: 'Try again' })); await screen.findByText('王小明 · Not linked');
  mockWsClient.call.mockRejectedValueOnce(new Error('設定已變更，請重新整理後再試。')); save();
  await screen.findByText('設定已變更，請重新整理後再試。');
});

it('keeps existing exemptions out of the new-risk acknowledgement', async () => {
  data.kinds[0].step_up_required = false;
  await mount(); await exempt(); save();
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('approvals.chat_admin.set', expect.objectContaining({
    step_up_exempt_kinds: ['skill', 'cli_egress', 'egress_guard'],
    acknowledge_exempt_risk: ['cli_egress', 'egress_guard'],
  })));
});
it('shows link and web availability warnings with display names', async () => {
  data.enabled = true; data.web_link_available = false; data.admins[0].has_totp = false;
  await mount();
  expect(screen.getByText(/No admin has linked their account yet/)).toBeInTheDocument();
  expect(screen.getByRole('link', { name: 'Go to My connections' })).toHaveAttribute('href', '/me/connections');
  expect(screen.getByText(/Web link unavailable:/)).toBeInTheDocument();
  expect(screen.getByText(/王小明 has no authenticator app/)).toBeInTheDocument();
});
it('groups using the current locale label and impact together', async () => {
  data.kinds[2].label = { ...data.kinds[2].label, en: 'Different English label' };
  await mount();
  expect(screen.getAllByRole('checkbox', { name: /Require extra confirmation/ })).toHaveLength(3);
});
it('does not merge identical labels with different impacts', async () => {
  data.kinds[2].impact = '不同影響';
  await mount();
  expect(screen.getAllByRole('checkbox', { name: /Require extra confirmation/ })).toHaveLength(3);
});
it('sends locally edited fields and re-reads the effective state after saving', async () => {
  await mount();
  fireEvent.click(screen.getByRole('checkbox', { name: 'Google Chat' }));
  fireEvent.click(screen.getByRole('checkbox', { name: 'Code in a direct message' }));
  fireEvent.change(screen.getByRole('spinbutton'), { target: { value: '24' } });
  save();
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('approvals.chat_admin.set', expect.objectContaining({
    channels: ['telegram', 'googlechat'], step_up_methods: ['web', 'dm_totp'], card_valid_hours: 24,
  })));
  await screen.findByText('Saved');
  expect(screen.getByRole('spinbutton')).toHaveValue(168);
  expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'approvals.chat_admin.get')).toHaveLength(2);
});

it('hides technical load errors behind the generic sentence', async () => {
  mockWsClient.call.mockRejectedValueOnce(new Error('commit failed: /home/x/config.toml'));
  renderWithProviders(<ChatApprovalCard />);
  expect(await screen.findByRole('alert')).toHaveTextContent(en['errorState.reason.unknown']);
  expect(document.body.textContent).not.toContain('commit failed');
  expect(document.body.textContent).not.toContain('/home/x/config.toml');
});
it.each([false, true])('hides technical save errors including risk dialog %s', async inDialog => {
  await mount();
  mockWsClient.call.mockRejectedValueOnce(new Error('commit failed: /home/x/config.toml'));
  if (inDialog) {
    fireEvent.click(toggle());
    const dialog = screen.getByRole('dialog');
    fireEvent.click(within(dialog).getByRole('checkbox'));
    fireEvent.click(within(dialog).getByRole('button', { name: 'Enable' }));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent(en['errorState.reason.unknown']);
  } else {
    save();
    expect(await screen.findByRole('alert')).toHaveTextContent(en['errorState.reason.unknown']);
  }
  expect(document.body.textContent).not.toContain('commit failed');
  expect(document.body.textContent).not.toContain('/home/x/config.toml');
});
