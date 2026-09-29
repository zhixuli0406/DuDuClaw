import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route, useLocation } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { ProvenancePanel } from './KnowledgeCuration';
import type { MemoryGraphEdge } from '@/lib/api';

/**
 * ProvenancePanel's normal home is the graph tab's D3/SVG canvas
 * (`MemoryGraph`, edge selection via a d3 click handler) — not worth driving
 * from a DOM test. Exercising the panel directly, the same way `formatSource`
 * is tested as a pure unit below, keeps this test fast and not brittle to the
 * graph's rendering internals.
 */

const EDGE: MemoryGraphEdge = {
  subject: 'duduclaw',
  predicate: 'uses',
  object: 'sqlite',
  memory_id: 'mem-1',
  origin_trust: 0.8,
  quarantined: false,
};

function WikiTrustProbe() {
  // G2 (2026-09 feature audit): the wiki-trust page used to be a `?tab=`
  // panel inside the Governance shell. It is a route of its own now, so the
  // probe reports the path rather than a query param.
  const { pathname } = useLocation();
  return <div>wikitrust-probe:{pathname}</div>;
}

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ chain: [] });
});

describe('ProvenancePanel', () => {
  // X03 (UX audit §3.3): the trust % shown here is per-edge `origin_trust`;
  // the full page-level trust score audit lives on WikiTrustPage with no
  // route back until now.
  it('opens the wiki trust page from the "view page trust scores" CrossLink', async () => {
    const user = userEvent.setup();

    render(
      <IntlProvider messages={en} locale="en" defaultLocale="en">
        <MemoryRouter initialEntries={['/curate']}>
          <Routes>
            <Route
              path="/curate"
              element={
                <ProvenancePanel agentId="agent-1" edge={EDGE} onClose={vi.fn()} onOpenHistory={vi.fn()} />
              }
            />
            <Route path="/manage/wiki-trust" element={<WikiTrustProbe />} />
          </Routes>
        </MemoryRouter>
      </IntlProvider>,
    );

    const link = await screen.findByRole('button', { name: 'View page trust scores' });
    await user.click(link);

    await waitFor(() => {
      expect(screen.getByText('wikitrust-probe:/manage/wiki-trust')).toBeInTheDocument();
    });
  });
});
