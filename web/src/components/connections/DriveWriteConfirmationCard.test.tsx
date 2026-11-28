import { act, fireEvent, screen } from '@testing-library/react';
import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { IntlProvider } from 'react-intl';
import { mockWsClient } from '@/test/mocks';
import { render } from '@/test/render';
import messages from '@/i18n/zh-TW.json';
import { api, type DriveWriteConfirmation } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { decodeCardValue, DriveWriteConfirmationCard, DriveWriteConfirmations } from './DriveWriteConfirmationCard';

function encode(value: unknown) {
  return JSON.stringify(value).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/([*_`\[\]#!])/g, '\\$1');
}
const empty = encode(null);
const request: DriveWriteConfirmation = {
  id: 'request-1', expires_at: '2099-01-01T00:00:00Z',
  agent: encode('助理_小美'), employee: encode('private-employee-id'), account: encode('me@example.com'), action: encode('create'),
  file_id: encode('private-file-id'), file_name: encode('報價單.docx'), destination: encode('private-destination-id'), destination_name: encode('業務部 > 客戶'),
  parents_before: encode(['private-parent-id']), source_names: encode(['來源 A', '來源 B']), size: encode(18432),
  preview: encode('<script>alert("你好")</script>\nA & B * _ ` [ ] # ! \\'), attachment_name: empty, rename_before: empty, rename_after: empty,
};
const onDecided = vi.fn();
function mount(value = request) {
  return render(<IntlProvider locale="zh-TW" messages={messages}><DriveWriteConfirmationCard request={value} onDecided={onDecided} /></IntlProvider>);
}
function mountList() { return render(<IntlProvider locale="zh-TW" messages={messages}><DriveWriteConfirmations /></IntlProvider>); }
beforeEach(() => { vi.clearAllMocks(); mockWsClient.call.mockResolvedValue({ decided: true }); useAgentsStore.setState({ agents: [] }); });
afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks(); });

describe('decodeCardValue', () => {
  it.each([
    ['"hello"', 'hello'],
    ['"&lt;b&gt; &amp; &amp;lt;"', '<b> & &lt;'],
    [encode('* _ ` [ ] # ! \\'), '* _ ` [ ] # ! \\'],
    ['null', null],
    [encode(['A_[x]', 'B & C']), ['A_[x]', 'B & C']],
    ['broken \\* &lt;text&gt;', 'broken * <text>'],
  ])('decodes %s', (raw, decoded) => { expect(decodeCardValue(raw)).toEqual(decoded); });
});

describe('Drive write confirmations', () => {
  it('renders decoded display values as plain text, never internal identifiers', () => {
    const { container } = mount();
    expect(screen.getByText('無法顯示的 AI 員工')).toBeInTheDocument();
    expect(screen.queryByText('助理_小美')).not.toBeInTheDocument();
    expect(screen.getByText('me@example.com')).toBeInTheDocument();
    expect(screen.getByText('建立檔案')).toBeInTheDocument();
    expect(screen.getByText('業務部 > 客戶')).toBeInTheDocument();
    expect(screen.getByText('來源 A、來源 B')).toBeInTheDocument();
    expect(screen.getByText('18 KB')).toBeInTheDocument();
    expect(container.querySelector('pre')?.textContent).toBe(decodeCardValue(request.preview));
    expect(container.querySelector('script')).toBeNull();
    for (const id of ['private-employee-id', 'private-file-id', 'private-destination-id', 'private-parent-id']) expect(container.innerHTML).not.toContain(id);
    expect(screen.getByRole('timer')).toHaveTextContent('剩');
  });
  it('ignores agent ids and resolves only the employee through the store', () => {
    useAgentsStore.setState({ agents: [{ name: 'private-employee-id', display_name: '客服小明' }] as never });
    const value = { ...request, agent: request.employee };
    const view = mount(value);
    expect(screen.getByText('客服小明')).toBeInTheDocument();
    expect(document.body.textContent).not.toContain('private-employee-id');
    view.unmount(); useAgentsStore.setState({ agents: [] }); mount(value);
    expect(document.body.textContent).not.toContain('private-employee-id');
    expect(screen.getByText('無法顯示的 AI 員工')).toBeInTheDocument();
  });
  it('renders attachment and rename details', () => {
    mount({ ...request, attachment_name: encode('上傳.pdf'), rename_before: encode('舊名'), rename_after: encode('新名') });
    for (const text of ['附件', '上傳.pdf', '改名', '原本', '舊名', '改成', '新名']) expect(screen.getByText(text)).toBeInTheDocument();
  });
  it.each([['modify', '修改檔案'], ['move', '搬移'], ['trash', '移到垃圾桶']])('labels action %s', (action, label) => {
    mount({ ...request, action: encode(action) }); expect(screen.getByText(label)).toBeInTheDocument();
  });
  it.each([[true, '確認寫入'], [false, '取消']] as const)('sends approve=%s and removes a decided card', async (approve, label) => {
    mockWsClient.call.mockImplementation(async (method: string) => method === 'me.google.write_confirmations' ? { requests: [request] } : { decided: true });
    mountList();
    fireEvent.click(await screen.findByRole('button', { name: label }));
    await act(async () => {});
    expect(mockWsClient.call).toHaveBeenCalledWith('me.google.write_confirm', { id: request.id, approve });
    expect(screen.queryByText('雲端硬碟寫入確認')).toBeNull();
  });
  it('disables both actions after expiry', () => {
    mount({ ...request, expires_at: '2000-01-01T00:00:00Z' });
    expect(screen.getByRole('status')).toHaveTextContent('超過 5 分鐘沒有確認，這次寫入已取消。');
    screen.getAllByRole('button').forEach(button => expect(button).toBeDisabled());
  });
  it('updates the countdown and prevents writes at expiry', async () => {
    vi.useFakeTimers(); vi.setSystemTime(new Date('2026-11-28T00:00:00Z'));
    mount({ ...request, expires_at: '2026-11-28T00:00:02Z' });
    expect(screen.getByRole('timer')).toHaveTextContent('0:02');
    await act(async () => { vi.advanceTimersByTime(1000); });
    expect(screen.getByRole('timer')).toHaveTextContent('0:01');
    await act(async () => { vi.advanceTimersByTime(1000); });
    expect(screen.getByRole('status')).toHaveTextContent('超過 5 分鐘');
    fireEvent.click(screen.getByRole('button', { name: '確認寫入' })); expect(mockWsClient.call).not.toHaveBeenCalled();
  });
  it('shows server refusal and keeps the card', async () => {
    mockWsClient.call.mockRejectedValue(new Error('檔案或資料夾在你確認後有變動，這次寫入已作廢。請讓 AI 員工重新提出。'));
    mount(); fireEvent.click(screen.getByRole('button', { name: '確認寫入' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('檔案或資料夾在你確認後有變動'); expect(onDecided).not.toHaveBeenCalled();
  });
  it('polls every three seconds, avoids resurrecting decided cards and stops on unmount', async () => {
    vi.useFakeTimers();
    const list = vi.spyOn(api.personalGoogle, 'writeConfirmations').mockResolvedValue({ requests: [request] });
    const view = mountList(); await act(async () => {}); expect(list).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole('button', { name: '確認寫入' })); await act(async () => {});
    await act(async () => { vi.advanceTimersByTime(3000); }); expect(list).toHaveBeenCalledTimes(2);
    expect(screen.queryByText('雲端硬碟寫入確認')).toBeNull();
    view.unmount(); await act(async () => { vi.advanceTimersByTime(3000); }); expect(list).toHaveBeenCalledTimes(2);
  });
});
