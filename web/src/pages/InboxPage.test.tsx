import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { InboxPage } from './InboxPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
});

describe('InboxPage (Multica list+detail split)', () => {
  it('renders the list column header + scope tabs', () => {
    renderWithProviders(<InboxPage />);
    // Page title in the left list header.
    expect(screen.getByRole('heading', { name: 'Inbox' })).toBeInTheDocument();
    // The five scope tabs.
    expect(screen.getByRole('button', { name: /Mine/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^All/ })).toBeInTheDocument();
  });

  it('shows the empty detail placeholder when nothing is selected', () => {
    renderWithProviders(<InboxPage />);
    expect(screen.getByText('Select an item to see its details')).toBeInTheDocument();
  });
});

/** Renders InboxPage at an explicit path (`renderWithProviders`'s router has
 *  no way to seed a starting location/query string). */
function renderAt(path: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <Routes>
          <Route path="/inbox" element={<InboxPage />} />
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}

const APPROVAL_FIXTURE = {
  agent_id: 'agent-a',
  kind: 'tool_call',
  payload: {},
  created_at: '2026-07-11T00:00:00Z',
  ttl_seconds: 3600,
};

describe('InboxPage deep link (W2-5 H5, ?item=<id>)', () => {
  beforeEach(() => {
    useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  });

  it('opens the linked approval from a bare gateway-style id (no type prefix)', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'approvals.list') {
        return Promise.resolve({
          count: 1,
          approvals: [{ ...APPROVAL_FIXTURE, id: 'apr-1', summary: 'Run a shell command' }],
        });
      }
      return Promise.resolve({});
    });

    // `deep_link.rs` (gateway) only ever has the bare broker id — never the
    // frontend's `approval:<id>` prefix. The suffix-match fallback must
    // still resolve it.
    renderAt('/inbox?item=apr-1');

    await waitFor(() => {
      expect(screen.queryByText('Select an item to see its details')).toBeNull();
    });
    expect(screen.getAllByText('Run a shell command').length).toBeGreaterThan(0);
  });

  it('opens the linked item from an already-prefixed id (dashboard-originated link)', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'approvals.list') {
        return Promise.resolve({
          count: 1,
          approvals: [{ ...APPROVAL_FIXTURE, id: 'apr-2', summary: 'Send a refund' }],
        });
      }
      return Promise.resolve({});
    });

    renderAt(`/inbox?item=${encodeURIComponent('approval:apr-2')}`);

    await waitFor(() => {
      expect(screen.queryByText('Select an item to see its details')).toBeNull();
    });
    expect(screen.getAllByText('Send a refund').length).toBeGreaterThan(0);
  });

  it('leaves the empty-state placeholder when the linked id matches nothing loaded', async () => {
    mockWsClient.call.mockResolvedValue({});
    renderAt('/inbox?item=does-not-exist');
    await waitFor(() => {
      expect(screen.getByText('Select an item to see its details')).toBeInTheDocument();
    });
  });
});
