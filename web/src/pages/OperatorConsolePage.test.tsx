import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { render } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import en from '@/i18n/en.json';
import { OperatorConsolePage } from './OperatorConsolePage';
import { useChatStore } from '@/stores/chat-store';
import { FIXTURE_DEVICE_STATUS, FIXTURE_CONFIRM_RESTART } from '@/components/console/fixtures';

beforeEach(() => {
  vi.clearAllMocks();
  useChatStore.setState({
    messages: [],
    steps: [],
    stepTree: [],
    isStreaming: false,
    phase: 'idle' as never,
    sessionId: null,
    sessionsRevision: 0,
    connectionState: 'connected' as never,
    agentName: 'DuDuClaw',
    agentIcon: '',
  });
});

describe('OperatorConsolePage — O-2 scaffold', () => {
  it('renders the composer', () => {
    renderWithProviders(<OperatorConsolePage />);
    expect(screen.getByPlaceholderText(en['console.placeholder'])).toBeInTheDocument();
  });

  it('shows the 對話先行 hero copy when there is no conversation yet', () => {
    renderWithProviders(<OperatorConsolePage />);
    expect(screen.getByText(en['console.hero.title'])).toBeInTheDocument();
    expect(screen.getByText(en['console.hero.subtitle'])).toBeInTheDocument();
  });

  it('clicking a hero suggestion fills the composer instead of sending immediately', () => {
    renderWithProviders(<OperatorConsolePage />);
    const suggestion = screen.getByText(en['console.hero.suggest.1']);
    fireEvent.click(suggestion);
    const input = screen.getByPlaceholderText(en['console.placeholder']) as HTMLTextAreaElement;
    expect(input).toHaveValue(en['console.hero.suggest.1']);
  });

  it('displays messages from the shared chat store', () => {
    useChatStore.setState({
      messages: [
        { id: '1', role: 'user', content: 'Restart the box', timestamp: Date.now() },
        { id: '2', role: 'assistant', content: 'On it.', timestamp: Date.now() },
      ],
    });
    renderWithProviders(<OperatorConsolePage />);
    expect(screen.getByText('Restart the box')).toBeInTheDocument();
    expect(screen.getByText('On it.')).toBeInTheDocument();
    // Hero copy only renders for an empty conversation.
    expect(screen.queryByText(en['console.hero.title'])).not.toBeInTheDocument();
  });

  it('does not send on Enter while a CJK IME is composing, but sends on a plain Enter', () => {
    const send = vi.fn();
    useChatStore.setState({ send: send as never, isStreaming: false, connectionState: 'connected' as never });

    renderWithProviders(<OperatorConsolePage />);
    const input = screen.getByPlaceholderText(en['console.placeholder']) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: '重開機' } });

    fireEvent.keyDown(input, { key: 'Enter', isComposing: true });
    expect(send).not.toHaveBeenCalled();

    fireEvent.keyDown(input, { key: 'Enter' });
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith('重開機', []);
  });

  it('O-3: renders a ChatArtifactCard below an assistant message that carries one', () => {
    useChatStore.setState({
      messages: [
        {
          id: '1',
          role: 'assistant',
          content: 'Here is the current status.',
          timestamp: Date.now(),
          artifact: FIXTURE_DEVICE_STATUS,
        },
      ],
    });
    renderWithProviders(<OperatorConsolePage />);
    expect(screen.getByText('Here is the current status.')).toBeInTheDocument();
    // The artifact card renders alongside the bubble, not instead of it.
    expect(screen.getByText(en['console.artifact.deviceStatus.title'])).toBeInTheDocument();
  });

  it('O-4: renders a confirm_action card for a pending restart — the real backend-populated artifact', () => {
    // FIXTURE_CONFIRM_RESTART mirrors exactly what `os_operator::marker_to_artifact`
    // + `parseChatArtifact` produce for an `os_power` restart marker — see
    // `crates/duduclaw-gateway/src/os_operator.rs` and `chat-store.ts`.
    useChatStore.setState({
      messages: [
        {
          id: '1',
          role: 'assistant',
          content: '「電源操作」會變更這台機器的狀態，請先確認：要執行嗎？',
          timestamp: Date.now(),
          artifact: FIXTURE_CONFIRM_RESTART,
        },
      ],
    });
    renderWithProviders(<OperatorConsolePage />);
    expect(screen.getByText('「電源操作」會變更這台機器的狀態，請先確認：要執行嗎？')).toBeInTheDocument();
    expect(screen.getAllByText('Restart').length).toBeGreaterThanOrEqual(1);
  });

  it('a message without an artifact renders no card (today\'s real path — nothing populates it yet)', () => {
    useChatStore.setState({
      messages: [
        { id: '1', role: 'assistant', content: 'Plain reply, no artifact yet.', timestamp: Date.now() },
      ],
    });
    renderWithProviders(<OperatorConsolePage />);
    expect(screen.getByText('Plain reply, no artifact yet.')).toBeInTheDocument();
    expect(screen.queryByText(en['console.artifact.deviceStatus.title'])).not.toBeInTheDocument();
  });

  it('the 進階 button navigates to the app grid (LauncherPage) without touching any other route', async () => {
    const user = userEvent.setup();
    render(
      <IntlProvider messages={en} locale="en" defaultLocale="en">
        <MemoryRouter initialEntries={['/console']}>
          <Routes>
            <Route path="/console" element={<OperatorConsolePage />} />
            <Route path="/launcher" element={<div>launcher-page-probe</div>} />
          </Routes>
        </MemoryRouter>
      </IntlProvider>,
    );
    await user.click(screen.getByRole('button', { name: en['console.advancedAria'] }));
    expect(await screen.findByText('launcher-page-probe')).toBeInTheDocument();
  });
});
