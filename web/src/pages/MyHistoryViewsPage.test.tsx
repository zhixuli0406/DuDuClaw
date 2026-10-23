import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { MyHistoryViewsPage } from './MyHistoryViewsPage';

const receipt = { viewer_display_name: 'Manager <img src=x>', agent_display_name: 'Sales', timestamp: '2026-10-23T10:00:00Z', type: 'webchat' };
beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ views: [receipt], next_before: 'opaque-next' });
});
describe('MyHistoryViewsPage', () => {
  it('renders only plain receipt metadata and follows the opaque self cursor', async () => {
    const user = userEvent.setup();
    renderWithProviders(<MyHistoryViewsPage />);
    expect(await screen.findByText('Manager <img src=x>')).toBeInTheDocument();
    expect(screen.getByText(/Sales · Your WebChat/)).toBeInTheDocument();
    expect(document.querySelector('img[src="x"]')).toBeNull();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Load more' }));
    expect(mockWsClient.call).toHaveBeenLastCalledWith('users.my_history_views', { before: 'opaque-next', limit: 50 });
    expect(mockWsClient.call.mock.calls.every(([, params]) => !params?.user_id && !params?.report_user_id)).toBe(true);
  });
  it('shows missing actors honestly and does not render unknown event kinds', async () => {
    mockWsClient.call.mockResolvedValue({ views: [{ ...receipt, viewer_display_name: null, agent_display_name: null }, { ...receipt, viewer_display_name: 'UNRECOGNIZED', type: 'raw_event' }], next_before: null });
    renderWithProviders(<MyHistoryViewsPage />);
    expect(await screen.findByText('Former manager')).toBeInTheDocument();
    expect(screen.getByText(/Former AI employee/)).toBeInTheDocument();
    expect(screen.queryByText('UNRECOGNIZED')).not.toBeInTheDocument();
  });
  it('clears previously shown receipts on a refused next page', async () => {
    const user = userEvent.setup();
    renderWithProviders(<MyHistoryViewsPage />);
    await screen.findByText('Manager <img src=x>');
    mockWsClient.call.mockRejectedValue(new Error('unavailable'));
    await user.click(screen.getByRole('button', { name: 'Load more' }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.queryByText('Manager <img src=x>')).not.toBeInTheDocument();
    expect(screen.queryByText(/No recorded views/)).not.toBeInTheDocument();
  });
  it('ignores an older in-flight page after refresh', async () => {
    const user = userEvent.setup();
    renderWithProviders(<MyHistoryViewsPage />);
    await screen.findByText('Manager <img src=x>');
    let finish!: (value: unknown) => void;
    mockWsClient.call.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    await user.click(screen.getByRole('button', { name: 'Load more' }));
    mockWsClient.call.mockResolvedValue({ views: [], next_before: null });
    await user.click(screen.getByRole('button', { name: 'Refresh' }));
    await screen.findByText(/No recorded views/);
    await act(async () => finish({ views: [{ ...receipt, viewer_display_name: 'STALE' }], next_before: null }));
    expect(screen.queryByText('STALE')).not.toBeInTheDocument();
  });
});
