import { describe, it, expect, vi, beforeEach } from 'vitest';
import '@/test/mocks';
import { useAgentsStore } from './agents-store';

vi.mock('@/lib/api', async (importOriginal) => {
  const original = await importOriginal<typeof import('@/lib/api')>();
  return {
    ...original,
    api: {
      ...original.api,
      agents: {
        list: vi.fn(),
        pause: vi.fn(),
        resume: vi.fn(),
        update: vi.fn(),
        remove: vi.fn(),
      },
    },
  };
});

// Must import after mocking
const { api } = await import('@/lib/api');

beforeEach(() => {
  vi.clearAllMocks();
  useAgentsStore.setState({ agents: [], loading: false, error: null, selectedAgentId: null });
});

describe('agents-store', () => {
  it('starts with empty state', () => {
    const state = useAgentsStore.getState();
    expect(state.agents).toEqual([]);
    expect(state.loading).toBe(false);
    expect(state.error).toBeNull();
  });

  it('fetchAgents populates agents list', async () => {
    const mockAgents = [
      { name: 'bot-1', display_name: 'Bot 1', status: 'active' },
      { name: 'bot-2', display_name: 'Bot 2', status: 'paused' },
    ];
    vi.mocked(api.agents.list).mockResolvedValue({ agents: mockAgents } as never);

    await useAgentsStore.getState().fetchAgents();

    const state = useAgentsStore.getState();
    expect(state.agents).toEqual(mockAgents);
    expect(state.loading).toBe(false);
    expect(state.error).toBeNull();
  });

  it('fetchAgents sets error on failure', async () => {
    vi.mocked(api.agents.list).mockRejectedValue(new Error('Network error'));

    await useAgentsStore.getState().fetchAgents();

    const state = useAgentsStore.getState();
    expect(state.error).toContain('Network error');
    expect(state.loading).toBe(false);
  });

  it('pauseAgent updates agent status optimistically', async () => {
    useAgentsStore.setState({
      agents: [{ name: 'bot-1', status: 'active' }] as never[],
    });
    vi.mocked(api.agents.pause).mockResolvedValue(undefined as never);

    await useAgentsStore.getState().pauseAgent('bot-1');

    const agent = useAgentsStore.getState().agents.find((a) => a.name === 'bot-1');
    expect(agent?.status).toBe('paused');
  });

  it('resumeAgent updates agent status optimistically', async () => {
    useAgentsStore.setState({
      agents: [{ name: 'bot-1', status: 'paused' }] as never[],
    });
    vi.mocked(api.agents.resume).mockResolvedValue(undefined as never);

    await useAgentsStore.getState().resumeAgent('bot-1');

    const agent = useAgentsStore.getState().agents.find((a) => a.name === 'bot-1');
    expect(agent?.status).toBe('active');
  });

  it('removeAgent filters agent from list', async () => {
    useAgentsStore.setState({
      agents: [
        { name: 'bot-1', status: 'active' },
        { name: 'bot-2', status: 'active' },
      ] as never[],
    });
    vi.mocked(api.agents.remove).mockResolvedValue(undefined as never);

    await useAgentsStore.getState().removeAgent('bot-1');

    expect(useAgentsStore.getState().agents).toHaveLength(1);
    expect(useAgentsStore.getState().agents[0].name).toBe('bot-2');
  });

  it('selectAgent sets selectedAgentId', () => {
    useAgentsStore.getState().selectAgent('bot-1');
    expect(useAgentsStore.getState().selectedAgentId).toBe('bot-1');

    useAgentsStore.getState().selectAgent(null);
    expect(useAgentsStore.getState().selectedAgentId).toBeNull();
  });
});
