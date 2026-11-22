import { beforeEach, expect, it, vi } from 'vitest';
import { screen, within, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { mockCallableGrants } from '@/test/callable-grants';
import { renderWithProviders } from '@/test/render';
import { KnowledgeCuration } from './KnowledgeCuration';
import type { AutoWikiPage } from '@/lib/api';

vi.mock('@/components/MemoryGraph', () => ({ MemoryGraph: () => null }));

const page = (state: AutoWikiPage['state']): AutoWikiPage => ({
  path: 'auto/sop/process.md', title: 'Process', updated: '2026-11-21T00:00:00Z',
  doc_type: 'sop', doc_type_label: 'SOP', sources: [], revision_count: 1, trust: 0.3,
  state,
});
let pages: AutoWikiPage[];

beforeEach(() => {
  pages = [page('needs_confirmation')];
  mockCallableGrants(['alice']);
  mockWsClient.call.mockReset();
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'wiki.auto_pages') return { pages: [...pages], exists: true };
    if (method === 'wiki.promote') {
      pages = [page('confirmed')];
      return { promoted: true };
    }
    if (method === 'wiki.archive') {
      pages = [];
      return { archived: true };
    }
    return { shared: true, nodes: [], edges: [] };
  });
});

async function openTab() {
  renderWithProviders(<KnowledgeCuration agentId="alice" />);
  await userEvent.click(screen.getByRole('tab', { name: 'Auto-filed' }));
  await screen.findByText('Process');
}

it('keeps a promoted row visible and offers sharing only after confirmation', async () => {
  await openTab();
  expect(screen.getByText('Needs confirmation')).toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Share to shared knowledge base' })).not.toBeInTheDocument();
  expect(screen.getByText('Confirm this page before sharing.')).toBeInTheDocument();
  await userEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  await userEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Confirm' }));
  const share = await screen.findByRole('button', { name: 'Share to shared knowledge base' });
  expect(screen.getByText('Process')).toBeInTheDocument();
  expect(screen.getByText('Confirmed')).toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Confirm' })).not.toBeInTheDocument();
  await userEvent.click(share);
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('wiki.share', {
    agent_id: 'alice', page_path: 'auto/sop/process.md',
  }));
  expect(mockWsClient.call).toHaveBeenCalledWith('wiki.promote', {
    agent_id: 'alice', page_path: 'auto/sop/process.md',
  });
  expect(await screen.findByText('Shared. Your other AI staff can see this page now.')).toBeInTheDocument();
});

it.each(['needs_confirmation', 'confirmed'] as const)('can archive a %s row', async (state) => {
  pages = [page(state)];
  await openTab();
  await userEvent.click(screen.getByRole('button', { name: 'Remove' }));
  await userEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Remove' }));
  await waitFor(() => expect(screen.queryByText('Process')).not.toBeInTheDocument());
  expect(mockWsClient.call).toHaveBeenCalledWith('wiki.archive', {
    agent_id: 'alice', page_path: 'auto/sop/process.md',
  });
});

it('requires confirmation when an older gateway omits the state', async () => {
  const legacy = page('needs_confirmation');
  delete legacy.state;
  pages = [legacy];
  await openTab();
  expect(screen.getByRole('button', { name: 'Confirm' })).toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Share to shared knowledge base' })).not.toBeInTheDocument();
});
