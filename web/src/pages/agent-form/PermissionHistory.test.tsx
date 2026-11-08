import { describe, it, expect, vi, beforeEach, type MockInstance } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import { api, type PermissionHistoryEvent } from '@/lib/api';
import { PermissionHistory, describeChange } from './PermissionHistory';

vi.mock('@/components/shared/ToolCatalogPicker', () => ({
  fetchCatalog: vi.fn().mockResolvedValue([
    { name: 'write_file', qualified: 'mcp__duduclaw__write_file', description: '', scope: '', category: 'office', kind: 'mcp' },
  ]),
}));

const ev = (over: Partial<PermissionHistoryEvent>): PermissionHistoryEvent => ({
  timestamp: '2026-11-08T10:00:00Z',
  agent_id: 'a1',
  actor: 'u1',
  actor_name: 'Amy',
  changes: [],
  ...over,
});

function renderHistory(isAdmin = true) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <PermissionHistory agentId="a1" isAdmin={isAdmin} />
    </IntlProvider>,
  );
}

let spy: MockInstance<typeof api.agents.permissionHistory>;
beforeEach(() => {
  spy = vi.spyOn(api.agents, 'permissionHistory');
  spy.mockReset(); // drop call history and queued results from earlier tests
});

describe('describeChange', () => {
  it('turns list diffs into per-tool state lines and keeps wildcards literal', () => {
    expect(
      describeChange({ key: 'capabilities.denied_tools', before: ['a'], after: ['mcp__duduclaw__*'] }),
    ).toEqual([
      { kind: 'tool', entry: 'mcp__duduclaw__*', from: 'allowed', to: 'denied' },
      { kind: 'tool', entry: 'a', from: 'denied', to: 'allowed' },
    ]);
    expect(describeChange({ key: 'capabilities.approval_required_tools', before: [], after: ['x'] })).toEqual([
      { kind: 'tool', entry: 'x', from: 'allowed', to: 'approval' },
    ]);
    expect(describeChange({ key: 'capabilities.allowed_tools', before: [], after: ['x', 'y'] })).toEqual([
      { kind: 'allowlistOn', count: 2 },
    ]);
    expect(describeChange({ key: 'capabilities.action_rules', before: [], after: [{}] })).toEqual([
      { kind: 'other', key: 'action_rules' },
    ]);
  });
});

describe('PermissionHistory', () => {
  it('renders time, actor and old → new, marks changes not made from the table', async () => {
    spy.mockResolvedValue({
      events: [
        ev({ changes: [{ key: 'capabilities.denied_tools', before: [], after: ['mcp__duduclaw__write_file'] }] }),
        ev({
          timestamp: '2026-11-07T09:00:00Z',
          actor: 'u2',
          actor_name: null,
          changes: [{ key: 'capabilities.approval_required_tools', before: ['x'], after: [] }],
        }),
      ],
      next_cursor: null,
    });
    renderHistory();
    expect(await screen.findByText('Amy')).toBeInTheDocument();
    expect(await screen.findByText('write_file')).toBeInTheDocument(); // friendly name from the catalog
    expect(screen.getAllByText('Can use')[0].tagName).toBe('S'); // old value struck through
    expect(screen.getByText('Cannot use')).toBeInTheDocument();
    expect(screen.getByText('u2')).toBeInTheDocument(); // falls back to the user id
    // History shows change content only — no origin badge.
    expect(screen.queryByText('Changed in Advanced settings')).not.toBeInTheDocument();
    expect(spy).toHaveBeenCalledWith('a1', { limit: 20 });
  });

  it('loads more with the cursor', async () => {
    const user = userEvent.setup();
    spy
      .mockResolvedValueOnce({ events: [ev({ changes: [{ key: 'capabilities.denied_tools', before: [], after: ['a'] }] })], next_cursor: 'c1' })
      .mockResolvedValueOnce({ events: [ev({ timestamp: '2026-11-01T00:00:00Z', actor_name: 'Bob', changes: [{ key: 'capabilities.denied_tools', before: ['b'], after: [] }] })], next_cursor: null });
    renderHistory();
    await user.click(await screen.findByRole('button', { name: 'Load more' }));
    expect(await screen.findByText('Bob')).toBeInTheDocument();
    expect(spy).toHaveBeenLastCalledWith('a1', { limit: 20, cursor: 'c1' });
    expect(screen.queryByRole('button', { name: 'Load more' })).not.toBeInTheDocument();
  });

  it('shows the empty state', async () => {
    spy.mockResolvedValue({ events: [], next_cursor: null });
    renderHistory();
    expect(await screen.findByText('No permission changes yet')).toBeInTheDocument();
  });

  it('shows a retryable error state', async () => {
    const user = userEvent.setup();
    spy.mockRejectedValueOnce(new Error('boom')).mockResolvedValueOnce({ events: [], next_cursor: null });
    renderHistory();
    expect(await screen.findByText('Can’t load the permission history right now')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Retry' }));
    expect(await screen.findByText('No permission changes yet')).toBeInTheDocument();
  });

  it('shows a loading state first', () => {
    spy.mockReturnValue(new Promise(() => {}));
    renderHistory();
    expect(screen.getByText('Loading the permission history…')).toBeInTheDocument();
  });

  it('non-admin: explains it is admin-only and never calls the RPC', () => {
    renderHistory(false);
    expect(screen.getByText('Only an administrator can view the permission history.')).toBeInTheDocument();
    expect(spy).not.toHaveBeenCalled();
  });
});
