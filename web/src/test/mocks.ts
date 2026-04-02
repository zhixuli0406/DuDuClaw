import { vi } from 'vitest';

// Mock ws-client — must be hoisted before store imports
export const mockWsClient = {
  call: vi.fn().mockResolvedValue(null),
  subscribe: vi.fn().mockReturnValue(vi.fn()),
  connect: vi.fn().mockResolvedValue(undefined),
  disconnect: vi.fn(),
};

vi.mock('@/lib/ws-client', () => ({
  client: mockWsClient,
  DuDuClawClient: vi.fn(() => mockWsClient),
}));
