import { describe, it, expect, vi, afterEach } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { RouteErrorBoundary } from './RouteErrorBoundary';

function Boom(): never {
  throw new Error('widget exploded');
}

describe('<RouteErrorBoundary> (L2 round 4)', () => {
  afterEach(() => vi.restoreAllMocks());

  it('catches a throwing child and shows a recoverable message with a reload action', () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    renderWithProviders(
      <RouteErrorBoundary>
        <Boom />
      </RouteErrorBoundary>,
    );
    expect(screen.getByText("This page couldn't be shown")).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Reload' })).toBeInTheDocument();
  });

  it('renders children untouched when nothing throws', () => {
    renderWithProviders(
      <RouteErrorBoundary>
        <p>fine</p>
      </RouteErrorBoundary>,
    );
    expect(screen.getByText('fine')).toBeInTheDocument();
  });
});

// ── L2 round 7: recovery after navigation + the reload action ──
import { fireEvent, render } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import { MemoryRouter, Routes, Route, Link, Outlet, useLocation } from 'react-router';

function Shell() {
  const location = useLocation();
  return (
    <div>
      <Link to="/ok">go ok</Link>
      <RouteErrorBoundary resetKey={location.pathname}>
        <Outlet />
      </RouteErrorBoundary>
    </div>
  );
}

describe('<RouteErrorBoundary> recovery (L2 round 7)', () => {
  it('resets when the route changes', () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    render(
      <IntlProvider messages={en} locale="en" defaultLocale="en">
        <MemoryRouter initialEntries={['/boom']}>
          <Routes>
            <Route element={<Shell />}>
              <Route path="boom" element={<Boom />} />
              <Route path="ok" element={<p>healthy page</p>} />
            </Route>
          </Routes>
        </MemoryRouter>
      </IntlProvider>,
    );
    expect(screen.getByText("This page couldn't be shown")).toBeInTheDocument();
    fireEvent.click(screen.getByText('go ok'));
    expect(screen.getByText('healthy page')).toBeInTheDocument();
    expect(screen.queryByText("This page couldn't be shown")).toBeNull();
  });

  it('the reload action reloads the page', () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const reload = vi.fn();
    const original = window.location;
    Object.defineProperty(window, 'location', { configurable: true, value: { ...original, reload } });
    try {
      renderWithProviders(
        <RouteErrorBoundary>
          <Boom />
        </RouteErrorBoundary>,
      );
      fireEvent.click(screen.getByRole('button', { name: 'Reload' }));
      expect(reload).toHaveBeenCalledTimes(1);
    } finally {
      Object.defineProperty(window, 'location', { configurable: true, value: original });
    }
  });
});
