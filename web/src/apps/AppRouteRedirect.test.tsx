import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MemoryRouter, Routes, Route, useLocation } from 'react-router';
import { AppRouteRedirect, LegacyRouteRedirect } from './AppRouteRedirect';

/**
 * `LegacyRouteRedirect` (N-3, `DESIGN-agent-os-native-apps-2026-08.md` §5 WP
 * N-3) — the reverse-direction counterpart to `AppRouteRedirect`: an OLD
 * route that has actually relocated forwards to its NEW canonical path,
 * query string preserved. Every migrated 系統設定 route (`/manage/accounts`,
 * `/device`, …) uses this in `App.tsx`, so a bug here breaks every bookmark
 * to the six pages at once.
 */
function DestinationProbe() {
  const location = useLocation();
  return <div>new canonical page{location.search}</div>;
}

function renderAt(initialPath: string) {
  return render(
    <MemoryRouter initialEntries={[initialPath]}>
      <Routes>
        <Route path="/old/path" element={<LegacyRouteRedirect to="/new/path" />} />
        <Route path="/new/path" element={<DestinationProbe />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe('LegacyRouteRedirect', () => {
  it('forwards to the new canonical path', () => {
    renderAt('/old/path');
    expect(screen.getByText('new canonical page')).toBeInTheDocument();
  });

  it('preserves the query string across the redirect', () => {
    renderAt('/old/path?tab=account&foo=bar');
    expect(screen.getByText('new canonical page?tab=account&foo=bar')).toBeInTheDocument();
  });

  it('carries no query string when the old URL had none', () => {
    renderAt('/old/path');
    expect(screen.getByText('new canonical page')).toBeInTheDocument();
  });
});

/**
 * Route-ranking sanity check (N-3): `App.tsx` mounts BOTH the N-1 wildcard
 * seam (`app/:appId/*`, catches every app's deep link) and, now, N-3's
 * explicit `app/system/device` leaf as siblings under the same `<Routes>`
 * tree. React Router must resolve a concrete URL like `/app/system/device`
 * to the fully-static leaf, not the wildcard — otherwise every migrated page
 * would silently fall back through `AppRouteRedirect` instead of rendering
 * directly. This reproduces just that shape (not the whole App.tsx tree,
 * which needs the full store/provider stack) to pin the behaviour the entire
 * migration depends on.
 */
describe('route ranking — explicit /app/system/device beats the app/:appId/* wildcard seam', () => {
  function renderSiblingTree(initialPath: string) {
    return render(
      <MemoryRouter initialEntries={[initialPath]}>
        <Routes>
          <Route path="app/:appId/*" element={<AppRouteRedirect />} />
          <Route path="app/system/device" element={<div>device canonical page</div>} />
          {/* Destination the wildcard seam redirects `/app/workbench/anything`
              to, so the second test below can observe it actually fired. */}
          <Route path="anything" element={<div>workbench-page-probe</div>} />
        </Routes>
      </MemoryRouter>,
    );
  }

  it('an explicit static leaf wins over the wildcard seam for the same URL', () => {
    renderSiblingTree('/app/system/device');
    expect(screen.getByText('device canonical page')).toBeInTheDocument();
    expect(screen.queryByText('workbench-page-probe')).not.toBeInTheDocument();
  });

  it("the wildcard seam still catches every other app's deep link, unaffected by the new static route", () => {
    renderSiblingTree('/app/workbench/anything');
    expect(screen.getByText('workbench-page-probe')).toBeInTheDocument();
  });
});
