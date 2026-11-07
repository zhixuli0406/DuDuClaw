import { beforeEach, expect, it, vi } from 'vitest';
import { act, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { mockCallableGrants } from '@/test/callable-grants';
import { renderWithProviders } from '@/test/render';
import { KnowledgeCuration } from './KnowledgeCuration';

vi.mock('@/components/MemoryGraph', () => ({ MemoryGraph: () => null }));
beforeEach(() => {
  mockWsClient.call.mockReset();
  mockWsClient.call.mockResolvedValue({ pages: [], nodes: [], edges: [] });
  mockCallableGrants(['alice']);
});

it('clears a purge confirmation and fences its captured agent after revocation', async () => {
  renderWithProviders(<KnowledgeCuration agentId="alice" />);
  await userEvent.click(screen.getByRole('tab', { name: 'Auto-filed' }));
  await userEvent.click(await screen.findByRole('button', { name: 'Clear everything collected automatically' }));
  const confirm = within(await screen.findByRole('dialog')).getByRole('button', { name: 'Clear all' });
  const count = mockWsClient.call.mock.calls.length;
  await act(async () => { mockCallableGrants([]); confirm.click(); });
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  expect(mockWsClient.call.mock.calls.slice(count)).toEqual([]);
});
