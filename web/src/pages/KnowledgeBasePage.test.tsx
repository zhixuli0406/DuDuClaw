import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { useAuthStore } from '@/stores/auth-store';
import { KnowledgeBasePage } from './KnowledgeBasePage';
import { callsOf, collection, mockRpc, renderAt } from './knowledge/test-fixtures';

const STATUS = { provider: 'none', available: false, mode: 'keyword_only', reason: 'off', coverage: 0 };

function setRole(role: 'admin' | 'manager' | 'employee') {
  useAuthStore.setState({ user: { id: 'u1', display_name: 'Me', role } } as never);
}

function renderPage() {
  return renderAt(<KnowledgeBasePage />, '/knowledge-base', '/knowledge-base', ['/knowledge-base/:id']);
}

beforeEach(() => {
  vi.clearAllMocks();
  setRole('employee');
});

describe('KnowledgeBasePage', () => {
  it('lists shared collections with sensitivity, role and document counts', async () => {
    mockRpc({
      'knowledge.collections.list': () => ({
        collections: [collection(), collection({ id: 'col_2', name: 'Product FAQ', sensitivity: 'public', my_role: 'viewer', source_count: 0, ready_count: 0 })],
        can_create: false,
      }),
      'knowledge.embedding.status': () => STATUS,
    });
    renderPage();
    expect(await screen.findByRole('button', { name: 'HR handbook' })).toBeInTheDocument();
    expect(screen.getByText('Product FAQ')).toBeInTheDocument();
    expect(screen.getByText('Public')).toBeInTheDocument();
    expect(screen.getByText('Owner')).toBeInTheDocument();
    expect(screen.getByText('2 documents')).toBeInTheDocument();
    expect(screen.getByText('· 1 in progress')).toBeInTheDocument();
    // No create permission: no create button; not an admin: no refused-reads list.
    expect(screen.queryByRole('button', { name: 'New collection' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Refused reads' })).toBeNull();
    expect(await screen.findByRole('note')).toBeInTheDocument();
  });

  it('opens a collection when its card is activated from the keyboard', async () => {
    const user = userEvent.setup();
    mockRpc({
      'knowledge.collections.list': () => ({ collections: [collection()], can_create: false }),
      'knowledge.embedding.status': () => STATUS,
    });
    renderPage();
    (await screen.findByRole('button', { name: 'HR handbook' })).focus();
    await user.keyboard('{Enter}');
    expect(await screen.findByText('probe:/knowledge-base/:id')).toBeInTheDocument();
  });

  it('creates a collection and lands on it; an invalid code blocks the button', async () => {
    const user = userEvent.setup();
    setRole('manager');
    mockRpc({
      'knowledge.collections.list': () => ({ collections: [], can_create: true }),
      'knowledge.embedding.status': () => STATUS,
      'knowledge.collections.create': (p) => ({ collection: collection({ id: 'col_new', name: String(p.name) }) }),
    });
    renderPage();
    expect(await screen.findByText('No collections yet')).toBeInTheDocument();
    await user.click(screen.getAllByRole('button', { name: 'New collection' })[0]);
    await user.type(screen.getByLabelText('Name'), 'Price list');
    await user.type(screen.getByLabelText('Short code (optional)'), 'bad code!');
    const create = screen.getByRole('button', { name: 'Create' });
    expect(create).toBeDisabled();
    await user.clear(screen.getByLabelText('Short code (optional)'));
    await user.type(screen.getByLabelText('Short code (optional)'), 'price-list');
    await user.selectOptions(screen.getByLabelText('Sensitivity'), 'public');
    await user.click(create);
    expect(await screen.findByText('probe:/knowledge-base/:id')).toBeInTheDocument();
    expect(callsOf('knowledge.collections.create')[0]).toMatchObject({ name: 'Price list', slug: 'price-list', sensitivity: 'public' });
  });

  it('shows a retryable error when the list cannot load', async () => {
    const user = userEvent.setup();
    let fail = true;
    mockRpc({
      'knowledge.collections.list': () => {
        if (fail) throw new Error('Request timeout: knowledge.collections.list');
        return { collections: [collection()], can_create: false };
      },
      'knowledge.embedding.status': () => STATUS,
    });
    renderPage();
    const retry = await screen.findByRole('button', { name: /retry|try again/i });
    fail = false;
    await user.click(retry);
    expect(await screen.findByRole('button', { name: 'HR handbook' })).toBeInTheDocument();
  });

  it('admins can open the refused-reads list across collections', async () => {
    const user = userEvent.setup();
    setRole('admin');
    mockRpc({
      'knowledge.collections.list': () => ({ collections: [], can_create: true }),
      'knowledge.embedding.status': () => STATUS,
      'knowledge.principals.search': () => ({ edition: 'enterprise', kinds: [], users: [{ id: 'u9', display_name: 'Bob', department: null }], departments: [], agents: [], channels: [] }),
      'knowledge.access_log.denied': () => ({
        entries: [
          { id: 3, at: '2026-10-21T01:00:00.000Z', op: 'get_passage', collection_id: 'col_1', collection_name: 'HR handbook', agent_id: null, requester_kind: 'dashboard_user', requester_id: 'u9', channel: null, query: null },
        ],
      }),
    });
    renderPage();
    await user.click(await screen.findByRole('button', { name: 'Refused reads' }));
    expect(await screen.findByText('Bob')).toBeInTheDocument();
    expect(screen.getByText(/HR handbook/)).toBeInTheDocument();
    await waitFor(() => expect(callsOf('knowledge.access_log.denied')).toHaveLength(1));
  });
});
