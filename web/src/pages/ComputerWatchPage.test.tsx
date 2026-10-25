import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Route, Routes } from 'react-router';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { ComputerWatchPage } from './ComputerWatchPage';

vi.mock('@/components/agent/ComputerSessionPanel', () => ({ ComputerSessionPanel: () => <p>Direct session panel</p> }));
const PNG = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=';
const ACTIVE = { ok: true, active: true, view_mode: 'masked', session_id: 'session-a', state: 'running' };
const IMAGE = { ok: true, session_id: 'session-a', png_base64: PNG, captured_at: 1700000000000, stale: false, fully_masked: false };
function page() {
  return <IntlProvider locale="en" messages={en}><MemoryRouter initialEntries={['/computer/alice?l=signed-context']}><Routes><Route path="/computer/:agentId" element={<ComputerWatchPage />} /></Routes></MemoryRouter></IntlProvider>;
}

beforeEach(() => {
  mockWsClient.call.mockReset();
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'computer_sessions.open_link') return { ok: true, agent_id: 'alice', session_id: 'session-a' };
    if (method === 'computer_sessions.status') return ACTIVE;
    return IMAGE;
  });
});

describe('session-bound chat navigation', () => {
  it('validates navigation context before requesting any browser content', async () => {
    let finish!: (value: unknown) => void;
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'computer_sessions.open_link') return new Promise((resolve) => { finish = resolve; });
      return method === 'computer_sessions.status' ? ACTIVE : IMAGE;
    });
    render(page());
    await waitFor(() => expect(finish).toBeTypeOf('function'));
    expect(mockWsClient.call).toHaveBeenCalledTimes(1);
    expect(mockWsClient.call).toHaveBeenCalledWith('computer_sessions.open_link', { token: 'signed-context' });
    await act(async () => finish({ ok: true, agent_id: 'alice', session_id: 'session-a' }));
    await screen.findByRole('img');
    expect(mockWsClient.call).toHaveBeenCalledWith('computer_sessions.snapshot', { agent_id: 'alice', session_id: 'session-a' });
  });

  it('keeps an expired link closed until the person explicitly chooses current status', async () => {
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'computer_sessions.open_link') throw new Error('expired');
      return method === 'computer_sessions.status' ? ACTIVE : IMAGE;
    });
    render(page());
    expect(await screen.findByRole('alert')).toHaveTextContent('This link has expired.');
    expect(mockWsClient.call).not.toHaveBeenCalledWith('computer_sessions.status', expect.anything());
    expect(screen.queryByRole('img')).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'View current status' }));
    await screen.findByRole('img');
  });

  it('refuses a valid context for a different route employee', async () => {
    mockWsClient.call.mockImplementation(async () => ({ ok: true, agent_id: 'bob', session_id: 'session-a' }));
    render(page());
    expect(await screen.findByRole('alert')).toHaveTextContent('This link has expired.');
    expect(mockWsClient.call).toHaveBeenCalledTimes(1);
  });

  it('never silently switches a once-valid link to a replacement session', async () => {
    render(page());
    await screen.findByRole('img');
    mockWsClient.call.mockImplementation(async (method: string) => method === 'computer_sessions.status' ? { ...ACTIVE, session_id: 'session-b' } : { ...IMAGE, session_id: 'session-b' });
    act(() => document.dispatchEvent(new Event('visibilitychange')));
    expect(await screen.findByRole('alert')).toHaveTextContent('This link has expired.');
    expect(screen.queryByRole('img')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Stop browser' })).not.toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('computer_sessions.snapshot', { agent_id: 'alice', session_id: 'session-b' });
  });

  it('keeps direct viewers on the same masked session when opening a chat link', async () => {
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'computer_sessions.open_link') return { ok: true, agent_id: 'alice', session_id: 'session-a' };
      return method === 'computer_sessions.status' ? { ...ACTIVE, view_mode: undefined } : IMAGE;
    });
    render(page());
    await screen.findByRole('img');
    expect(screen.queryByText('Direct session panel')).not.toBeInTheDocument();
  });
});
