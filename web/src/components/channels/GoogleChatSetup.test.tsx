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
  it('quotes shell arguments and includes the HTTP endpoint', () => {
    expect(shellQuote("a'$(touch /tmp/pwn)")).toBe("'a'\"'\"'$(touch /tmp/pwn)'");
    expect(bootstrapCommand('https://chat.example/webhook/googlechat', 'sales')).toContain("--agent 'sales'");
  });
  it('uses only an HTTPS public base without embedded credentials', () => {
    expect(publicEndpoint('https://chat.example/root/', '/webhook/googlechat')).toBe('https://chat.example/root/webhook/googlechat');
    for (const value of ['http://chat.example', 'https://a:b@chat.example', 'https://chat.example?token=x', 'garbage']) {
      expect(publicEndpoint(value, '/webhook/googlechat')).toBe('');
    }
  });
  it('loads the configured public URL and shows the final configuration after import', async () => {
    mockRpc({ 'system.config': () => ({ config: '[dashboard]\npublic_url = "https://public.example"\n[channels]\ngooglechat_project_number = "123456789"' }) });
    renderWithProviders(<GoogleChatSetup />);
    expect(await screen.findByRole('link', { name: 'Open Google Chat app configuration' })).toHaveAttribute('href', expect.stringContaining('project=123456789'));
    await waitFor(() => expect(screen.getByLabelText('Public HTTPS endpoint')).toHaveValue('https://public.example/webhook/googlechat'));
    expect(screen.getByText(/Authentication Audience: Project number/)).toBeInTheDocument();
  });
  it.each([['en', en], ['zh-TW', zhTW], ['ja-JP', jaJP]] as const)(
    'shows and copies the add-on opt-out instruction in %s', async (locale, messages) => {
      mockRpc({ 'system.config': () => ({ config: '' }) });
      const user = userEvent.setup();
      const clipboard = vi.spyOn(navigator.clipboard, 'writeText');
      render(<IntlProvider locale={locale} messages={messages}>
        <GoogleChatSetup agentId="a-very-long-employee-name-that-exceeds-the-app-limit" projectNumber="123456789" />
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
