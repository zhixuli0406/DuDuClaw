import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { ComputerWatchPanel } from './ComputerWatchPanel';

vi.mock('./ComputerSessionPanel', () => ({ ComputerSessionPanel: () => <p>Direct session panel</p> }));
const PNG = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=';
const ACTIVE = { ok: true, active: true, view_mode: 'masked', can_control: false, session_id: 'session-a', state: 'running', seconds_left: 60 };
const SNAPSHOT = { ok: true, session_id: 'session-a', png_base64: PNG, captured_at: 1700000000000, stale: false, fully_masked: false, mask_reason: null, width: 1, height: 1 };
const panel = (id = 'alice') => <IntlProvider locale="en" messages={en}><ComputerWatchPanel agentId={id} /></IntlProvider>;

beforeEach(() => {
  mockWsClient.call.mockReset();
  mockWsClient.call.mockImplementation(async (method: string) => method === 'computer_sessions.status' ? ACTIVE : SNAPSHOT);
});

describe('ComputerWatchPanel', () => {
  it('shows only the masked image and a session-bound stop action for a team viewer', async () => {
    render(panel());
    expect(await screen.findByRole('img', { name: 'Masked browser snapshot' })).toHaveAttribute('src', `data:image/png;base64,${PNG}`);
    expect(screen.queryByText('Direct session panel')).not.toBeInTheDocument();
    for (const name of [/Take over/, /Resume/, /Clear login/, /Watch live/]) expect(screen.queryByRole('button', { name })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Stop browser' }));
    expect(mockWsClient.call).not.toHaveBeenCalledWith('computer_sessions.stop', expect.anything());
    await userEvent.click(screen.getByRole('button', { name: 'Yes, stop' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('computer_sessions.stop', { agent_id: 'alice', session_id: 'session-a' });
    expect(mockWsClient.call).toHaveBeenCalledWith('computer_sessions.snapshot', { agent_id: 'alice', session_id: 'session-a' });
    expect(mockWsClient.call.mock.calls.every(([method]) => String(method).startsWith('computer_sessions.'))).toBe(true);
  });

  it('clears an old image as soon as a fresh permission check refuses', async () => {
    render(panel());
    await screen.findByRole('img');
    mockWsClient.call.mockRejectedValue(new Error('permission denied'));
    act(() => document.dispatchEvent(new Event('visibilitychange')));
    await waitFor(() => expect(screen.queryByRole('img')).not.toBeInTheDocument());
    expect(await screen.findByText('You can no longer view this browser.')).toBeInTheDocument();
  });

  it('discards an in-flight response from the previous employee', async () => {
    let finish!: (value: unknown) => void;
    mockWsClient.call.mockImplementation(async (method: string, args: { agent_id: string }) => {
      if (args.agent_id === 'bob') throw new Error('permission denied');
      if (method === 'computer_sessions.status') return ACTIVE;
      return new Promise((resolve) => { finish = resolve; });
    });
    const view = render(panel());
    await waitFor(() => expect(finish).toBeTypeOf('function'));
    view.rerender(panel('bob'));
    await screen.findByText('You can no longer view this browser.');
    await act(async () => finish(SNAPSHOT));
    expect(screen.queryByRole('img')).not.toBeInTheDocument();
  });

  it('never treats an arbitrary image URL or mismatched session as a snapshot', async () => {
    mockWsClient.call.mockImplementation(async (method: string) => method === 'computer_sessions.status' ? ACTIVE : { ...SNAPSHOT, png_base64: 'https://outside.test/track', session_id: 'other-session' });
    render(panel());
    expect(await screen.findByText('The masked snapshot is temporarily unavailable.')).toBeInTheDocument();
    expect(screen.queryByRole('img')).not.toBeInTheDocument();
  });

  it('uses the existing direct session panel for direct access', async () => {
    mockWsClient.call.mockResolvedValue({ ok: true, active: false });
    render(panel());
    expect(await screen.findByText('Direct session panel')).toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('computer_sessions.snapshot', expect.anything());
  });

  it('stops polling in a hidden tab and rechecks permission when it becomes visible', async () => {
    vi.useFakeTimers();
    const visibility = vi.spyOn(document, 'visibilityState', 'get');
    try {
      visibility.mockReturnValue('visible');
      await act(async () => { render(panel()); });
      expect(mockWsClient.call).toHaveBeenCalledTimes(2);
      visibility.mockReturnValue('hidden');
      await act(async () => { document.dispatchEvent(new Event('visibilitychange')); await vi.advanceTimersByTimeAsync(6000); });
      expect(mockWsClient.call).toHaveBeenCalledTimes(2);
      visibility.mockReturnValue('visible');
      await act(async () => { document.dispatchEvent(new Event('visibilitychange')); });
      expect(mockWsClient.call).toHaveBeenCalledTimes(4);
    } finally { visibility.mockRestore(); vi.useRealTimers(); }
  });
});
