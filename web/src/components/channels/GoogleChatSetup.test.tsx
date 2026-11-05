import { describe, it, expect, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { mockRpc } from '@/pages/knowledge/test-fixtures';
import { GoogleChatSetup, bootstrapCommand, publicEndpoint, shellQuote } from './GoogleChatSetup';

describe('Google Chat setup', () => {
  it('quotes shell arguments and omits the endpoint for Pub/Sub', () => {
    expect(shellQuote("a'$(touch /tmp/pwn)")).toBe("'a'\"'\"'$(touch /tmp/pwn)'");
    expect(bootstrapCommand('http', 'https://chat.example/webhook/googlechat', 'sales')).toContain("--agent 'sales'");
    expect(bootstrapCommand('pubsub', 'https://chat.example')).not.toContain('--endpoint-url');
  });
  it('uses only an HTTPS public base without embedded credentials', () => {
    expect(publicEndpoint('https://chat.example/root/', '/webhook/googlechat')).toBe('https://chat.example/root/webhook/googlechat');
    for (const value of ['http://chat.example', 'https://a:b@chat.example', 'https://chat.example?token=x', 'garbage']) {
      expect(publicEndpoint(value, '/webhook/googlechat')).toBe('');
    }
  });
  it('loads the configured public URL and shows the final configuration after import', async () => {
    mockRpc({ 'system.config': () => ({ config: '[dashboard]\npublic_url = "https://public.example"\n[channels]\ngooglechat_project_number = "123456789"' }) });
    renderWithProviders(<GoogleChatSetup connection="http" onConnectionChange={vi.fn()} subscription="" onSubscriptionChange={vi.fn()} />);
    expect(await screen.findByRole('link', { name: 'Open Google Chat app configuration' })).toHaveAttribute('href', expect.stringContaining('project=123456789'));
    await waitFor(() => expect(screen.getByLabelText('Public HTTPS endpoint')).toHaveValue('https://public.example/webhook/googlechat'));
    expect(screen.getByText(/Authentication Audience: Project number/)).toBeInTheDocument();
  });
  it('does not invent the topic of a custom subscription', async () => {
    mockRpc({ 'system.config': () => ({ config: '' }) });
    renderWithProviders(<GoogleChatSetup connection="pubsub" onConnectionChange={vi.fn()} subscription="projects/my-project/subscriptions/custom" onSubscriptionChange={vi.fn()} agentId="sales" projectNumber="123456789" />);
    expect(screen.getByText(/Cloud Pub\/Sub topic: Use the topic name printed by the setup script/)).toBeInTheDocument();
  });
  it('exposes Pub/Sub without requiring a public URL and links Cloud Shell', async () => {
    mockRpc({ 'system.config': () => ({ config: '' }) });
    const user = userEvent.setup();
    const change = vi.fn();
    renderWithProviders(<GoogleChatSetup connection="pubsub" onConnectionChange={change} subscription="projects/my-project/subscriptions/duduclaw-chat-events-sub" onSubscriptionChange={vi.fn()} agentId="sales" projectNumber="123456789" />);
    expect(screen.queryByLabelText('Public HTTPS endpoint')).toBeNull();
    expect(screen.getByText(/Cloud Pub\/Sub topic: projects\/my-project\/topics\/duduclaw-chat-events/)).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Open in Cloud Shell' })).toHaveAttribute('href', expect.stringContaining('cloudshell_tutorial='));
    await user.selectOptions(screen.getByLabelText('Connection type'), 'http');
    expect(change).toHaveBeenCalledWith('http');
  });
  it.each([['en', en], ['zh-TW', zhTW], ['ja-JP', jaJP]] as const)(
    'shows and copies the add-on opt-out instruction in %s', async (locale, messages) => {
      mockRpc({ 'system.config': () => ({ config: '' }) });
      const user = userEvent.setup();
      const clipboard = vi.spyOn(navigator.clipboard, 'writeText');
      render(<IntlProvider locale={locale} messages={messages}>
        <GoogleChatSetup connection="pubsub" onConnectionChange={vi.fn()} subscription="projects/my-project/subscriptions/duduclaw-chat-events-sub" onSubscriptionChange={vi.fn()} agentId="a-very-long-employee-name-that-exceeds-the-app-limit" projectNumber="123456789" />
      </IntlProvider>);
      expect(screen.getByText(messages['googlechat.setup.disableAddon'])).toBeInTheDocument();
      expect(screen.getByText(messages['googlechat.setup.dedicatedProject'])).toBeInTheDocument();
      await user.click(screen.getByRole('button', { name: messages['googlechat.setup.copyValues'] }));
      const copied = clipboard.mock.calls.at(-1)?.[0] ?? '';
      expect(copied).toContain(messages['googlechat.setup.disableAddon']);
      expect(copied).toContain('Build this Chat app as a Google Workspace add-on');
      const appName = copied.split('\n').find(line => line.startsWith('App name: '))?.slice(10) ?? '';
      expect(Array.from(appName)).toHaveLength(25);
    },
  );

});
