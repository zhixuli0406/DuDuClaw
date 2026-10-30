import { describe, it, expect, beforeEach } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import { api } from './api';

beforeEach(() => mockWsClient.call.mockClear());

describe('users.bindAgent authorization intent', () => {
  it.each(['owner', 'operator', 'viewer'] as const)('sends the explicitly selected %s access', async (access) => {
    await api.users.bindAgent('user', 'sales', access);
    expect(mockWsClient.call).toHaveBeenCalledWith('users.bind_agent', {
      user_id: 'user', agent_name: 'sales', access_level: access,
    });
  });
});
