import { beforeEach, describe, expect, it } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import { api } from './api';

beforeEach(() => {
  mockWsClient.call.mockReset();
  mockWsClient.call.mockResolvedValue({});
});

describe('approval decision RPC params', () => {
  it.each([true, false])('sends expected_digest when approve=%s', async (approve) => {
    const digest = 'a'.repeat(64);
    await api.approvals.decide('apr-1', approve, 'reviewed', digest);
    expect(mockWsClient.call).toHaveBeenCalledWith('approvals.decide', {
      id: 'apr-1', approve, reason: 'reviewed', expected_digest: digest,
    });
  });

  it('omits expected_digest for gateways without approval digests', async () => {
    await api.approvals.decide('apr-1', true);
    expect(mockWsClient.call).toHaveBeenCalledWith('approvals.decide', {
      id: 'apr-1', approve: true,
    });
  });

  it('keeps install request decisions on their separate RPC contract', async () => {
    await api.installRequests.decide('install-1', false, 'declined');
    expect(mockWsClient.call).toHaveBeenCalledWith('install_requests.decide', {
      id: 'install-1', approve: false, reason: 'declined',
    });
  });
});
