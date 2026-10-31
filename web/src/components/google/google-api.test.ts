import { expect, it } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import { api } from '@/lib/api';

it('allows bounded per-service Google probes to finish beyond the default RPC timeout', async () => {
  mockWsClient.call.mockResolvedValueOnce({ ok: true, services: [] });
  await api.googleCredentials.test();
  expect(mockWsClient.call).toHaveBeenCalledWith('google.credentials.test', {}, false, 90000);
});
