import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { PresetsPage } from './PresetsPage';
import { api } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';

const AGENTS = [
  { name: 'sales', display_name: '業務小美', role: 'staff', status: 'active' },
  { name: 'hr', display_name: 'HR 小安', role: 'staff', status: 'active' },
];

beforeEach(() => {
  vi.restoreAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never });
  useAgentsStore.setState({ agents: [], loading: false, loaded: false } as never);
  vi.spyOn(api.agents, 'list').mockResolvedValue({ agents: AGENTS } as never);
});

describe('<PresetsPage> — agent preset P1 read-only dashboard card', () => {
  it('renders the available preset catalog', async () => {
    vi.spyOn(api.presets, 'list').mockResolvedValue({
      presets: [
        { id: 'sales-followup', version: '1.0.0', label: '業務跟進部門', description: '共用部門組合' },
      ],
    } as never);
    vi.spyOn(api.presets, 'status').mockResolvedValue({
      agent_id: 'sales',
      resolution: { state: 'unbound' },
    } as never);

    renderWithProviders(<PresetsPage />);

    expect(await screen.findByText('sales-followup')).toBeInTheDocument();
    expect(screen.getByText('業務跟進部門')).toBeInTheDocument();
    expect(screen.getByText('v1.0.0')).toBeInTheDocument();
  });

  it('shows a broken preset with its parse error instead of dropping it', async () => {
    vi.spyOn(api.presets, 'list').mockResolvedValue({
      presets: [{ id: 'broken-kit', error: 'invalid TOML at line 4' }],
    } as never);
    vi.spyOn(api.presets, 'status').mockResolvedValue({
      agent_id: 'sales',
      resolution: { state: 'unbound' },
    } as never);

    renderWithProviders(<PresetsPage />);

    expect(await screen.findByText('broken-kit')).toBeInTheDocument();
    expect(screen.getByText('invalid TOML at line 4')).toBeInTheDocument();
  });

  it('shows each AI staff member\'s binding: applied with overridden fields, and unresolved with a reason', async () => {
    vi.spyOn(api.presets, 'list').mockResolvedValue({ presets: [] } as never);
    vi.spyOn(api.presets, 'status').mockImplementation((agentId: string) => {
      if (agentId === 'sales') {
        return Promise.resolve({
          agent_id: 'sales',
          resolution: {
            state: 'applied',
            preset_id: 'sales-followup',
            version: '1.0.0',
            label: '業務跟進部門',
            changed_fields: ['capabilities.allowed_tools', 'model.preferred'],
          },
        }) as never;
      }
      return Promise.resolve({
        agent_id: 'hr',
        resolution: { state: 'unresolved', preset_id: 'hr-onboarding', version: '2.0.0', reason: 'content hash mismatch' },
      }) as never;
    });
    useAgentsStore.setState({ agents: AGENTS as never, loading: false, loaded: true } as never);

    renderWithProviders(<PresetsPage />);

    await waitFor(() => expect(screen.getByText('業務跟進部門')).toBeInTheDocument());
    expect(screen.getByText('Applied')).toBeInTheDocument();
    expect(screen.getByText('capabilities.allowed_tools')).toBeInTheDocument();
    expect(screen.getByText('model.preferred')).toBeInTheDocument();

    await waitFor(() => expect(screen.getByText('Resolution failed')).toBeInTheDocument());
    expect(screen.getByText('Reason: content hash mismatch')).toBeInTheDocument();
  });

  it('shows the empty catalog state when no presets exist yet', async () => {
    vi.spyOn(api.presets, 'list').mockResolvedValue({ presets: [] } as never);
    vi.spyOn(api.presets, 'status').mockResolvedValue({
      agent_id: 'sales',
      resolution: { state: 'unbound' },
    } as never);

    renderWithProviders(<PresetsPage />);

    expect(await screen.findByText('No job presets yet')).toBeInTheDocument();
  });
});
