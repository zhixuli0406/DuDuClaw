import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, cleanup } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import { UpdateTab } from './UpdateTab';
import zh from '@/i18n/zh-TW.json';
import en from '@/i18n/en.json';
import ja from '@/i18n/ja-JP.json';

const mocks = vi.hoisted(() => ({
  check: vi.fn(), apply: vi.fn(), handlers: new Map<string, (payload: unknown) => void>(),
}));
vi.mock('@/lib/api', () => ({ api: { system: {
  version: vi.fn().mockResolvedValue({}), checkUpdate: mocks.check, applyUpdate: mocks.apply,
} } }));
vi.mock('@/lib/ws-client', () => ({ client: { subscribe: (event: string, handler: (payload: unknown) => void) => {
  mocks.handlers.set(event, handler);
  return () => mocks.handlers.delete(event);
} } }));
vi.mock('@/lib/desktop-update', () => ({ desktopUpdateInfo: vi.fn().mockResolvedValue(null) }));

beforeEach(() => {
  cleanup();
  mocks.check.mockResolvedValue({ available: true, current_version: '1.79.2', latest_version: '1.79.3',
    download_url: 'https://example.test/release.tar.gz', install_method: 'binary' });
});

async function install(locale: string, messages: Record<string, string>) {
  render(<IntlProvider locale={locale} messages={messages}><UpdateTab /></IntlProvider>);
  fireEvent.click(screen.getByRole('button', { name: messages['settings.update.check'] }));
  fireEvent.click(await screen.findByRole('button', { name: messages['settings.update.install'] }));
}

describe('UpdateTab failure reasons', () => {
  it.each([['zh-TW', zh], ['en', en], ['ja-JP', ja]] as const)('shows a localized size reason for old RPC strings in %s', async (locale, messages) => {
    const reason = 'Extracted binary exceeds size limit (possible zip bomb)';
    mocks.apply.mockRejectedValue(reason);
    await install(locale, messages);
    expect(await screen.findByText(messages['settings.update.failure.size'])).toBeInTheDocument();
    expect(screen.getByText(reason)).toBeInTheDocument();
    expect(screen.getByText(reason).closest('details')).not.toHaveAttribute('open');
    fireEvent.click(screen.getByText(messages['settings.update.failure.details']));
    expect(screen.getByText(reason).closest('details')).toHaveAttribute('open');
  });
  it('shows the provider reason for a non-throwing unsuccessful result', async () => {
    mocks.apply.mockResolvedValue({ success: false, message: 'Failed to sync binary: disk full' });
    await install('zh-TW', zh);
    expect(await screen.findByText(zh['settings.update.failure.write'])).toBeInTheDocument();
  });
  it('never leaves an empty failure invisible', async () => {
    mocks.apply.mockRejectedValue(undefined);
    await install('zh-TW', zh);
    expect(await screen.findByText(zh['settings.update.failure.unknown'])).toBeInTheDocument();
  });
  it('shows background failure events', async () => {
    render(<IntlProvider locale="en" messages={en}><UpdateTab /></IntlProvider>);
    await waitFor(() => expect(mocks.handlers.has('system.update_failed')).toBe(true));
    const { act } = await import('@testing-library/react');
    act(() => mocks.handlers.get('system.update_failed')?.({ kind: 'rollback', detail: 'rollback failed: denied' }));
    expect(await screen.findByText(en['settings.update.failure.rollback'])).toBeInTheDocument();
  });
});
