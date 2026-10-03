import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useRestartRequiredStore } from '@/stores/restart-required-store';

const configMock = vi.fn();
const updateConfigMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    system: {
      config: () => configMock(),
      updateConfig: (f: Record<string, unknown>) => updateConfigMock(f),
    },
  },
}));

const toastInfo = vi.fn();
const toastError = vi.fn();
vi.mock('@/lib/toast', async (orig) => {
  const real = await orig<typeof import('@/lib/toast')>();
  return { ...real, toast: { ...real.toast, info: (m: string) => toastInfo(m), error: (m: string) => toastError(m) } };
});

import { WebChatWidgetCard, generateWidgetKey } from './WebChatWidgetCard';
import { reportChannelStart } from './AddChannelDialog';
import { createIntl } from 'react-intl';
import en from '@/i18n/en.json';

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useRestartRequiredStore.getState().clear();
  updateConfigMock.mockResolvedValue({ success: true, changes: [], restart_required: [] });
});

describe('<WebChatWidgetCard>', () => {
  it('clears a stored key with an explicit action, sending ""', async () => {
    configMock.mockResolvedValue({ config: '[webchat]\npublic_widget = false\nwidget_key = "********"\n' });
    renderWithProviders(<WebChatWidgetCard />);
    fireEvent.click(await screen.findByRole('button', { name: 'Clear key' }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({ webchat: { widget_key: '' } });
    await waitFor(() => expect(screen.getByTestId('widget-key-state').textContent).toBe('Not set'));
  });

  it('shows a stored key only as set, and never re-sends it', async () => {
    configMock.mockResolvedValue({ config: '[webchat]\npublic_widget = false\nwidget_key = "********"\n' });
    renderWithProviders(<WebChatWidgetCard />);
    await waitFor(() => expect(screen.getByTestId('widget-key-state').textContent).toBe('Set'));
    fireEvent.click(screen.getByRole('switch', { name: 'Open the website chat widget' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({ webchat: { public_widget: true } });
  });

  it('requires a key of at least 16 characters before opening the widget', async () => {
    configMock.mockResolvedValue({ config: '' });
    renderWithProviders(<WebChatWidgetCard />);
    await waitFor(() => expect(screen.getByTestId('widget-key-state').textContent).toBe('Not set'));
    fireEvent.click(screen.getByRole('switch', { name: 'Open the website chat widget' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByText('Set a key before opening the widget.')).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText('Widget key'), { target: { value: 'short' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByText('The key must be at least 16 characters.')).toBeInTheDocument();
    expect(updateConfigMock).not.toHaveBeenCalled();
    const key = generateWidgetKey();
    expect(key.length).toBeGreaterThanOrEqual(16);
    fireEvent.change(screen.getByLabelText('Widget key'), { target: { value: key } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({ webchat: { public_widget: true, widget_key: key } });
  });
});

describe('reportChannelStart (channels.add hot_started)', () => {
  const intl = createIntl({ locale: 'en', messages: en });

  it('flags a restart when the gateway says the endpoint needs one', () => {
    expect(reportChannelStart('whatsapp', { hot_started: false, restart_required: true }, intl)).toBe('restart');
    expect(toastInfo.mock.calls[0][0]).toContain('only starts receiving messages after the gateway restarts');
    expect(useRestartRequiredStore.getState().keys).toEqual(['channels.whatsapp']);
  });

  it('points at the credentials when a v1.68 gateway could not start it', () => {
    expect(
      reportChannelStart('telegram', { hot_started: false, restart_required: false, not_started_reason: 'check_credentials' }, intl),
    ).toBe('not_started');
    expect(toastError.mock.calls[0][0]).toContain('Check the token or secret');
    expect(useRestartRequiredStore.getState().keys).toEqual([]);
  });

  it('shows a v1.68 answer without a reason as an error, not a restart', () => {
    expect(reportChannelStart('slack', { hot_started: false, restart_required: false, not_started_reason: null }, intl)).toBe('not_started');
    expect(toastError.mock.calls[0][0]).toContain('gave no reason');
    expect(useRestartRequiredStore.getState().keys).toEqual([]);
  });

  it('treats a bare hot_started:false (older gateway) as restart-only', () => {
    expect(reportChannelStart('feishu', { hot_started: false }, intl)).toBe('not_started');
    expect(useRestartRequiredStore.getState().keys).toEqual(['channels.feishu']);
  });

  it('stays quiet when the channel started', () => {
    expect(reportChannelStart('discord', { hot_started: true, restart_required: false }, intl)).toBe('started');
    expect(toastInfo).not.toHaveBeenCalled();
    expect(toastError).not.toHaveBeenCalled();
  });
});
