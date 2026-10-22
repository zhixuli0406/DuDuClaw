import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { callsOf, mockRpc } from '@/pages/knowledge/test-fixtures';
import { KnowledgeBindingSection, bindingFromSaved, bindingToWrite } from './KnowledgeBindingSection';

const GRANTED = [
  { id: 'col_1', slug: 'hr', name: 'HR handbook', description: '', sensitivity: 'internal', source_count: 2 },
  { id: 'col_2', slug: 'faq', name: 'Product FAQ', description: 'Public answers', sensitivity: 'public', source_count: 5 },
];

beforeEach(() => vi.clearAllMocks());

describe('KnowledgeBindingSection', () => {
  it('starts on "every shared collection" and narrows to ticked ones', async () => {
    const user = userEvent.setup();
    mockRpc({
      'knowledge.agent.collections': () => ({ granted: GRANTED, bound: null, unreadable: false }),
      'agents.update': () => ({}),
    });
    renderWithProviders(<KnowledgeBindingSection agentId="a1" />);
    const all = await screen.findByRole('radio', { name: /Use every collection shared with it/ });
    expect(all).toBeChecked();
    expect(screen.getByRole('checkbox', { name: /HR handbook/ })).toHaveAttribute('aria-disabled', 'true');

    await user.click(screen.getByRole('radio', { name: /Only the ones ticked below/ }));
    await waitFor(() => expect(callsOf('agents.update').at(-1)).toEqual({ agent_id: 'a1', knowledge: { collections: [] } }));
    await user.click(screen.getByRole('checkbox', { name: /Product FAQ/ }));
    await waitFor(() => expect(callsOf('agents.update').at(-1)).toEqual({ agent_id: 'a1', knowledge: { collections: ['faq'] } }));
    expect(await screen.findByText('Saved')).toBeInTheDocument();

    await user.click(screen.getByRole('radio', { name: /Don't use the document library/ }));
    await waitFor(() => expect(callsOf('agents.update').at(-1)).toEqual({ agent_id: 'a1', knowledge: { collections: [] } }));
    await user.click(screen.getByRole('radio', { name: /Use every collection shared with it/ }));
    await waitFor(() => expect(callsOf('agents.update').at(-1)).toEqual({ agent_id: 'a1', knowledge: { collections: null } }));
  });

  it('shows a saved selection and drops collections no longer shared', async () => {
    mockRpc({ 'knowledge.agent.collections': () => ({ granted: GRANTED, bound: ['hr', 'gone'], unreadable: false }) });
    renderWithProviders(<KnowledgeBindingSection agentId="a1" />);
    expect(await screen.findByRole('checkbox', { name: /HR handbook/ })).toBeChecked();
    expect(screen.getByRole('checkbox', { name: /Product FAQ/ })).not.toBeChecked();
    expect(screen.getByText(/removed on the next save: gone/)).toBeInTheDocument();
  });

  it('reports a refused save', async () => {
    const user = userEvent.setup();
    mockRpc({
      'knowledge.agent.collections': () => ({ granted: GRANTED, bound: null, unreadable: false }),
      'agents.update': () => {
        throw new Error('集合「faq」沒有授權給這位 AI 員工或其部門。');
      },
    });
    renderWithProviders(<KnowledgeBindingSection agentId="a1" />);
    await user.click(await screen.findByRole('radio', { name: /Don't use/ }));
    expect(await screen.findByText(/沒有授權給這位 AI 員工/)).toBeInTheDocument();
  });

  it('explains how to share when nothing is shared yet', async () => {
    mockRpc({ 'knowledge.agent.collections': () => ({ granted: [], bound: null, unreadable: false }) });
    renderWithProviders(<KnowledgeBindingSection agentId="a1" />);
    expect(await screen.findByText(/No collection is shared with this AI agent/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Manage sharing in the document library/ })).toBeInTheDocument();
  });

  it('stays hidden when the document library is unavailable', async () => {
    mockRpc({});
    const { container } = renderWithProviders(<KnowledgeBindingSection agentId="a1" />);
    await waitFor(() => expect(callsOf('knowledge.agent.collections')).toHaveLength(1));
    expect(container).toBeEmptyDOMElement();
  });

  it('mapping helpers', () => {
    expect(bindingFromSaved(null)).toEqual({ mode: 'all', selected: [] });
    expect(bindingFromSaved([])).toEqual({ mode: 'off', selected: [] });
    expect(bindingFromSaved(['a'])).toEqual({ mode: 'selected', selected: ['a'] });
    expect(bindingToWrite('all', ['a'], ['a'])).toBeNull();
    expect(bindingToWrite('off', ['a'], ['a'])).toEqual([]);
    expect(bindingToWrite('selected', ['a', 'gone'], ['a'])).toEqual(['a']);
  });
});
