import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { RedactionTab } from './RedactionTab';

const src = (mode: string) => ({ mode, only_categories: [], exclude_categories: [] });

const config = {
  enabled: true,
  vault_ttl_hours: 24,
  purge_after_expire_days: 30,
  profiles: [],
  sources: {
    user_input: src('off'),
    tool_results: src('on'),
    system_prompt: src('off'),
    sub_agent: src('inherit'),
    cron_context: src('inherit'),
  },
  tool_egress: {},
  available_profiles: [],
  field_rules: [],
  data_sources: [],
  poisoned: null,
};

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'redaction.get') return Promise.resolve(config);
    if (method === 'redaction.update')
      return Promise.resolve({ success: true, changes: [], applied: true, warning: null });
    return Promise.resolve(null);
  });
});

describe('RedactionTab sub_agent source row', () => {
  it('renders the delegated-employee row and saves sources.sub_agent', async () => {
    const user = userEvent.setup();
    renderWithProviders(<RedactionTab />);

    expect(await screen.findByText('Delegated employee replies')).toBeInTheDocument();
    expect(screen.getByText(/does not cover the copy delivered to the person/)).toBeInTheDocument();

    await user.click(screen.getByRole('combobox', { name: 'Delegated employee replies' }));
    await user.click(await screen.findByRole('option', { name: 'Always redact' }));

    await user.click(screen.getByRole('button', { name: /^Save$/ }));

    await waitFor(() => {
      const call = mockWsClient.call.mock.calls.find((c) => c[0] === 'redaction.update');
      expect(call).toBeTruthy();
      expect(call![1].sources.sub_agent).toEqual({
        mode: 'on',
        only_categories: [],
        exclude_categories: [],
      });
    });
  });
});
