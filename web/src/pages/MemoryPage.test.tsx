import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useSystemStore } from '@/stores/system-store';
import { MemoryPage } from './MemoryPage';

/** Point the edition gate at one profile for the duration of a test. */
function setEdition(profile: 'personal' | 'enterprise') {
  useSystemStore.setState({
    status: { edition_profile: profile },
  } as Parameters<typeof useSystemStore.setState>[0]);
}

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
  setEdition('enterprise');
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('MemoryPage', () => {
  it('renders the collection header', () => {
    renderWithProviders(<MemoryPage />);
    expect(screen.getByRole('heading', { name: 'Memory' })).toBeInTheDocument();
  });

  it('renders memory, knowledge and learning segments (enterprise)', () => {
    renderWithProviders(<MemoryPage />);
    expect(screen.getByRole('radio', { name: 'Memories' })).toBeInTheDocument();
    // The knowledge base merged in here (2026-07-30 client feedback).
    expect(screen.getByRole('radio', { name: 'Personal' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Shared' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Key Insights' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Self-Improvement' })).toBeInTheDocument();
  });

  it('collapses the two knowledge bases into one tab on Personal', () => {
    setEdition('personal');
    renderWithProviders(<MemoryPage />);
    expect(screen.getByRole('radio', { name: 'Knowledge base' })).toBeInTheDocument();
    expect(screen.queryByRole('radio', { name: 'Shared' })).not.toBeInTheDocument();
    expect(screen.queryByRole('radio', { name: 'Personal' })).not.toBeInTheDocument();
  });

  // Evolution v3 dashboard convergence: stagnation watch, rejection
  // distribution, and the Playbook section only render once an agent is
  // selected (the picker defaults to the first agent from `agents.list`).
  it('evolution tab surfaces stagnation health and the playbook section for the selected agent', async () => {
    mockWsClient.call.mockImplementation((method: unknown) => {
      switch (method) {
        case 'agents.list':
          return Promise.resolve({ agents: [{ name: 'agent-a', display_name: 'Agent A' }] });
        case 'evolution.status':
          return Promise.resolve({
            enabled: true,
            mode: 'prediction_driven',
            total_agents: 1,
            gvu_enabled_count: 1,
            total_versions: 0,
            last_applied_at: null,
            agents: [{
              agent_id: 'agent-a',
              gvu_enabled: true,
              cognitive_memory: true,
              skill_auto_activate: true,
              skill_security_scan: true,
              max_silence_hours: 12,
              max_gvu_generations: 3,
              observation_period_hours: 24,
            }],
          });
        case 'evolution.versions':
          return Promise.resolve({ versions: [] });
        case 'evolution.stagnation':
          return Promise.resolve({
            snapshots: [{ agent_id: 'agent-a', is_stagnant: false, signals: [], summary: null, checked_at: '' }],
          });
        case 'evolution.telemetry':
          return Promise.resolve({ agent_id: 'agent-a', days: 7, total: 0, by_stage_layer: {} });
        case 'evolution.consolidations':
          return Promise.resolve({ consolidations: [] });
        case 'playbook.list':
          return Promise.resolve({ agent_id: 'agent-a', entries: [] });
        default:
          return Promise.resolve({});
      }
    });

    const user = userEvent.setup();
    renderWithProviders(<MemoryPage />);
    await user.click(screen.getByRole('radio', { name: 'Self-Improvement' }));

    await waitFor(() => {
      expect(screen.getByText('Evolution loop health')).toBeInTheDocument();
    });
    expect(await screen.findByText('Running normally — no stagnation signal')).toBeInTheDocument();
    expect(await screen.findByText('Rejection distribution')).toBeInTheDocument();
    expect(await screen.findByText('Playbook')).toBeInTheDocument();
    expect(screen.getByText("This AI staff member hasn't built up any playbook entries yet")).toBeInTheDocument();
  });
});
