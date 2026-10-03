import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent, waitFor, within } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { MarketplacePage } from '@/pages/MarketplacePage';
import { missingRequiredEnv, requiredEnvPayload, unmaskedEnv } from './RequiredEnvFields';

describe('required env helpers', () => {
  it('never echoes the masked status words back', () => {
    expect(unmaskedEnv({ A: 'set', B: 'not_set', C: 'reference', D: '/srv/data' })).toEqual({ D: '/srv/data' });
  });
  it('lists missing names and trims the payload', () => {
    expect(missingRequiredEnv(['K1', 'K2'], { K1: ' x ', K2: '  ' })).toEqual(['K2']);
    expect(requiredEnvPayload(['K1'], { K1: ' x ', OTHER: 'y' })).toEqual({ K1: 'x' });
  });
});

describe('MarketplacePage install with required env', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useConnectionStore.setState({ state: 'authenticated' as never, error: null });
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'agents.list') return { agents: [{ name: 'sam', display_name: 'Sam' }] };
      if (method === 'marketplace.list') {
        return {
          servers: [{
            id: 'browserbase', name: 'Browserbase', description: 'Remote browser', category: 'browser',
            author: 'x', tags: [], featured: true, requires_oauth: false,
            required_env: ['BROWSERBASE_API_KEY'], installed_by: [],
          }],
        };
      }
      return { success: true, agent_id: 'sam' };
    });
  });

  it('asks for the key and sends it as env, blocking install until filled', async () => {
    renderWithProviders(<MarketplacePage />);
    const installButtons = await screen.findAllByRole('button', { name: 'Install' });
    fireEvent.click(installButtons[0]);
    const dialog = await screen.findByRole('dialog');
    const confirm = within(dialog).getByRole('button', { name: 'Install' });
    expect(confirm).toBeDisabled();
    const field = within(dialog).getByLabelText('BROWSERBASE_API_KEY') as HTMLInputElement;
    expect(field.type).toBe('password');
    fireEvent.change(field, { target: { value: 'bb-secret' } });
    fireEvent.click(confirm);
    await waitFor(() =>
      expect(mockWsClient.call.mock.calls.some(([m]) => m === 'marketplace.install')).toBe(true),
    );
    const params = mockWsClient.call.mock.calls.find(([m]) => m === 'marketplace.install')?.[1];
    expect(params).toEqual({ id: 'browserbase', agent_id: 'sam', env: { BROWSERBASE_API_KEY: 'bb-secret' } });
  });
});
