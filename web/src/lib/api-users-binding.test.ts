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


describe('users avatar API', () => {
  it('sends the target member and nullable avatar without changing the primary API', async () => {
    await api.users.setAvatar('member', 'avatar-ai');
    expect(mockWsClient.call).toHaveBeenCalledWith('users.set_avatar', { user_id: 'member', agent_id: 'avatar-ai' });
    await api.users.setAvatar('member', null);
    expect(mockWsClient.call).toHaveBeenCalledWith('users.set_avatar', { user_id: 'member', agent_id: null });
    await api.users.avatarConflicts();
    expect(mockWsClient.call).toHaveBeenCalledWith('users.avatar_conflicts');
  });
});
