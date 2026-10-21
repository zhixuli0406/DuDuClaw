import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { createIntl } from 'react-intl';
import '@/test/mocks';
import en from '@/i18n/en.json';
import { AccessLogTab, requesterText } from './AccessLogTab';
import { callsOf, mockRpc, renderAt } from './test-fixtures';

function row(id: number, over: Record<string, unknown> = {}) {
  return {
    id,
    at: '2026-10-21T02:00:00.000Z',
    op: 'search',
    agent_id: null,
    requester_kind: 'dashboard_user',
    requester_id: 'u2',
    channel: null,
    query: '特休幾天',
    result_count: 3,
    mode: 'keyword_only',
    outcome: 'ok',
    ...over,
  };
}

const PRINCIPALS = { edition: 'enterprise', kinds: [], users: [{ id: 'u2', display_name: 'Amy Chen', department: null }], departments: [], agents: [{ id: 'helper', display_name: 'Helper', department: null }], channels: [] };

beforeEach(() => vi.clearAllMocks());

describe('AccessLogTab', () => {
  it('shows who asked what and how many passages came back, with names', async () => {
    mockRpc({
      'knowledge.access_log.list': () => ({
        entries: [row(2), row(1, { op: 'get_passage', agent_id: 'helper', requester_kind: 'channel_unbound', requester_id: null, channel: 'line', query: null, outcome: 'denied', result_count: 0 })],
      }),
      'knowledge.principals.search': () => PRINCIPALS,
    });
    renderAt(<AccessLogTab collectionId="col_1" />);
    expect(await screen.findByText('特休幾天')).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText('Amy Chen')).toBeInTheDocument());
    expect(screen.getByText('Helper (for Someone on LINE without an account)')).toBeInTheDocument();
    expect(screen.getByText('Refused')).toBeInTheDocument();
    expect(screen.getByText('Read a passage')).toBeInTheDocument();
  });

  it('pages back with the oldest id', async () => {
    const user = userEvent.setup();
    const page = Array.from({ length: 50 }, (_, i) => row(100 - i));
    mockRpc({
      'knowledge.access_log.list': (p) => ({ entries: p.before_id ? [row(10)] : page }),
      'knowledge.principals.search': () => PRINCIPALS,
    });
    renderAt(<AccessLogTab collectionId="col_1" />);
    await user.click(await screen.findByRole('button', { name: 'Load earlier entries' }));
    await waitFor(() => expect(callsOf('knowledge.access_log.list')[1]).toMatchObject({ before_id: 51 }));
    expect(screen.queryByRole('button', { name: 'Load earlier entries' })).toBeNull();
  });

  it('empty state', async () => {
    mockRpc({ 'knowledge.access_log.list': () => ({ entries: [] }), 'knowledge.principals.search': () => PRINCIPALS });
    renderAt(<AccessLogTab collectionId="col_1" />);
    expect(await screen.findByText('Nobody has read this collection yet')).toBeInTheDocument();
  });

  it('requesterText falls back to a short id and names scheduled work', () => {
    const intl = createIntl({ locale: 'en', messages: en });
    expect(requesterText(intl, { requester_kind: 'dashboard_user', requester_id: 'abcdef123456', agent_id: null, channel: null })).toBe('User abcdef12');
    expect(requesterText(intl, { requester_kind: 'system', requester_id: null, agent_id: null, channel: null })).toBe('Scheduled work');
  });

  it('shows only a code for a question the viewer may not read, and names unknown senders', async () => {
    mockRpc({
      'knowledge.access_log.list': () => ({
        entries: [row(3, { query: null, query_hash: 'a1b2c3', requester_kind: 'unknown', requester_id: null })],
      }),
      'knowledge.principals.search': () => PRINCIPALS,
    });
    renderAt(<AccessLogTab collectionId="col_1" />);
    expect(await screen.findByText('#a1b2c3')).toBeInTheDocument();
    expect(screen.getByText('A request whose sender could not be confirmed')).toBeInTheDocument();
  });
});
