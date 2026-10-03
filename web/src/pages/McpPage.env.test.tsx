import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';
import { McpPage } from './McpPage';

// v1.68 (W2): mcp.list answers env values masked (set / not_set /
// reference). Installing from the marketplace tab must never send those
// words back as values; required names are typed by the operator.
beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  useAgentsStore.setState({ fetchAgents: vi.fn() as never, agents: [{ name: 'sam', display_name: 'Sam' }] as never });
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'mcp.list') {
      return {
        agents: [],
        catalog: [{
          id: 'browserbase', name: 'Browserbase', description: 'Remote browser', category: 'browser',
          requires_oauth: false,
          default_def: { command: 'npx', args: ['-y', '@browserbasehq/mcp'], env: { BROWSERBASE_API_KEY: 'not_set' } },
          required_env: ['BROWSERBASE_API_KEY'],
        }],
      };
    }
    if (method === 'marketplace.install') return { success: true, agent_id: 'sam' };
    return { providers: [] };
  });
});

describe('McpPage marketplace install', () => {
  it('installs through marketplace.install with the typed key, never the masked word', async () => {
    const user = userEvent.setup();
    render(
      <IntlProvider messages={en} locale="en" defaultLocale="en">
        <MemoryRouter initialEntries={['/?mcp_tab=marketplace']}>
          <McpPage />
        </MemoryRouter>
      </IntlProvider>,
    );
    fireEvent.click(await screen.findByRole('button', { name: /Install to Agent/ }));
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('combobox'));
    await user.click(await screen.findByRole('option', { name: 'Sam' }));
    const confirm = within(dialog).getByRole('button', { name: 'Install to Agent' });
    expect(confirm).toBeDisabled();
    fireEvent.change(within(dialog).getByLabelText('BROWSERBASE_API_KEY'), { target: { value: 'bb-secret' } });
    fireEvent.click(confirm);
    await waitFor(() =>
      expect(mockWsClient.call.mock.calls.some(([m]) => m === 'marketplace.install')).toBe(true),
    );
    const params = mockWsClient.call.mock.calls.find(([m]) => m === 'marketplace.install')?.[1];
    expect(params).toEqual({ id: 'browserbase', agent_id: 'sam', env: { BROWSERBASE_API_KEY: 'bb-secret' } });
    expect(JSON.stringify(mockWsClient.call.mock.calls)).not.toContain('not_set"}}');
    expect(mockWsClient.call.mock.calls.some(([m]) => m === 'mcp.update')).toBe(false);
  });
});
