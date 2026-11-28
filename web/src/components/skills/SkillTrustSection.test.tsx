import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import zhTW from '@/i18n/zh-TW.json';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';
import type { SkillTrustSettings } from '@/lib/api';
import { SkillTrustSection } from './SkillTrustSection';

const pin = { publisher: 'company', key_id: '3fa907c218be55d1', added_by: 'user-private-id', added_at: '2026-11-20T10:00:00Z', conflicting: true, in_trust_store: false };
let data: SkillTrustSettings;
function mount() {
  return render(<IntlProvider locale="zh-TW" messages={zhTW}><SkillTrustSection /></IntlProvider>);
}
beforeEach(() => {
  vi.clearAllMocks();
  data = { publishers: [], auto_approve: [], skill_ttl_days: null, recent_auto_approvals: [] };
  useAuthStore.setState({ user: { role: 'admin' } as never });
  useAgentsStore.setState({ agents: [{ name: 'agent-private-id', display_name: '採購小陳' }] as never, loaded: true });
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'skill_trust.get') return Promise.resolve(data);
    if (method === 'users.list') return Promise.resolve({ users: [{ id: 'user-private-id', display_name: '王小明', email: 'boss@example.com' }] });
    if (method === 'skill_trust.unpin') return Promise.resolve({ removed: true, disabled: 3 });
    return Promise.resolve({});
  });
});
async function openAdd(publisher = 'company') {
  const user = userEvent.setup();
  await user.click(await screen.findByRole('button', { name: '＋ 新增自家來源' }));
  await user.type(screen.getByLabelText('發行者代號'), publisher);
  await user.type(screen.getByLabelText('公鑰'), 'public-key');
  return user;
}

describe('SkillTrustSection', () => {
  it('hides all content and makes no requests for non-admins', () => {
    useAuthStore.setState({ user: { role: 'member' } as never });
    const view = mount();
    expect(view.container).toBeEmptyDOMElement();
    expect(mockWsClient.call).not.toHaveBeenCalled();
  });
  it('renders the empty state and default seven-day lifetime', async () => {
    mount();
    expect(await screen.findByText('還沒有自家技能來源。含腳本的技能都要管理員核准。')).toBeInTheDocument();
    expect(screen.getByRole('spinbutton', { name: '核准期限' })).toHaveValue(7);
  });
  it('renders the grouped fingerprint, resolved name, missing key and conflict', async () => {
    data.auto_approve = [pin];
    mount();
    expect(await screen.findByText('金鑰指紋 3FA9 07C2 18BE 55D1')).toBeInTheDocument();
    expect(await screen.findByText(/自動放行 · 王小明在/)).toBeInTheDocument();
    expect(screen.queryByText(/user-private-id/)).not.toBeInTheDocument();
    expect(screen.getByText('金鑰已不在信任清單，不會再自動放行')).toBeInTheDocument();
    expect(screen.getByText('這把金鑰和其他設定衝突，暫不自動放行')).toBeInTheDocument();
  });
  it('requires acknowledgement, sends both credentials, clears them on refusal and shows the server message', async () => {
    mount();
    const user = await openAdd();
    const add = screen.getByRole('button', { name: '新增' });
    expect(add).toBeDisabled();
    await user.click(screen.getByLabelText('我了解新增後的影響'));
    expect(add).toBeEnabled();
    await user.type(screen.getByLabelText('目前密碼'), 'secret-password');
    await user.type(screen.getByLabelText('新的二次驗證碼或備援碼'), '123456');
    mockWsClient.call.mockRejectedValueOnce(new Error('密碼或驗證碼不正確，請重新輸入。'));
    await user.click(add);
    expect(mockWsClient.call).toHaveBeenCalledWith('skill_trust.pin', { publisher: 'company', public_key: 'public-key', acknowledge_risk: true, password: 'secret-password', code: '123456' });
    expect(await screen.findByRole('alert')).toHaveTextContent('密碼或驗證碼不正確，請重新輸入。');
    expect(screen.getByLabelText('目前密碼')).toHaveValue('');
    expect(screen.getByLabelText('新的二次驗證碼或備援碼')).toHaveValue('');
  });
  it('blocks the official publisher before calling pin', async () => {
    mount();
    const user = await openAdd('duduclaw');
    await user.click(screen.getByLabelText('我了解新增後的影響'));
    await user.click(screen.getByRole('button', { name: '新增' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('官方發行者不能加入自家技能來源。');
    expect(mockWsClient.call.mock.calls.some(([method]) => method === 'skill_trust.pin')).toBe(false);
  });
  it('reloads the list after a successful pin', async () => {
    mount();
    const user = await openAdd();
    await user.click(screen.getByLabelText('我了解新增後的影響'));
    data = { ...data, auto_approve: [pin] };
    await user.click(screen.getByRole('button', { name: '新增' }));
    expect(await screen.findByText('金鑰指紋 3FA9 07C2 18BE 55D1')).toBeInTheDocument();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'skill_trust.get')).toHaveLength(2);
  });
  it.each([true, false])('removes with disable_skills=%s and reports the disabled count', async disable => {
    data.auto_approve = [pin];
    mount();
    const user = userEvent.setup();
    await user.click(await screen.findByRole('button', { name: '移除' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByRole('checkbox')).toBeChecked();
    if (!disable) await user.click(within(dialog).getByRole('checkbox'));
    data = { ...data, auto_approve: [] };
    await user.click(within(dialog).getByRole('button', { name: '移除' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('skill_trust.unpin', { publisher: 'company', key_id: pin.key_id, disable_skills: disable });
    expect(await screen.findByText('已移除，並停用了 3 個技能。')).toBeInTheDocument();
    expect(await screen.findByText('還沒有自家技能來源。含腳本的技能都要管理員核准。')).toBeInTheDocument();
  });
  it.each(['0', '31', '2.5', ''])('rejects invalid TTL %s without updating configuration', async value => {
    mount();
    const user = userEvent.setup();
    const input = await screen.findByRole('spinbutton', { name: '核准期限' });
    await user.clear(input);
    if (value) await user.type(input, value);
    await user.click(screen.getByRole('button', { name: '儲存' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('核准期限要在 1 到 30 天之間。');
    expect(mockWsClient.call.mock.calls.some(([method]) => method === 'system.update_config')).toBe(false);
  });
  it.each([1, 30])('saves valid TTL boundary %s using the config wrapper', async value => {
    data.skill_ttl_days = 10;
    mount();
    const user = userEvent.setup();
    const input = await screen.findByRole('spinbutton', { name: '核准期限' });
    expect(input).toHaveValue(10);
    await user.clear(input); await user.type(input, String(value));
    await user.click(screen.getByRole('button', { name: '儲存' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('system.update_config', { approval: { skill_ttl_days: value } }));
    expect(await screen.findByText('已儲存核准期限。')).toBeInTheDocument();
  });
  it('shows recent skill names and agent display names with a safe fallback', async () => {
    data.recent_auto_approvals = [{ id: 'recent', slug: 'pack', publisher: 'company', key_id: pin.key_id, decided_at: '2026-11-27T10:02:00Z', skills: [
      { agent: 'agent-private-id', name: '報價單整理', digest: 'abc' }, { agent: 'missing-agent-id', name: '出貨通知', digest: 'def' },
    ] }];
    mount();
    expect(await screen.findByText('最近自動放行（20 筆）')).toBeInTheDocument();
    expect(screen.getByText('報價單整理')).toBeInTheDocument();
    expect(screen.getByText('採購小陳')).toBeInTheDocument();
    expect(screen.getByText('出貨通知')).toBeInTheDocument();
    expect(screen.getByText('無法顯示的 AI 員工')).toBeInTheDocument();
    expect(screen.queryByText(/agent-private-id|missing-agent-id/)).not.toBeInTheDocument();
    expect(document.querySelector('time')).toHaveAttribute('datetime', '2026-11-27T10:02:00Z');
  });
});
