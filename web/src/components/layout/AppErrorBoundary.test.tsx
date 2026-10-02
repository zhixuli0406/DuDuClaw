import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import { AppErrorBoundary } from './AppErrorBoundary';

function Boom(): never {
  throw new Error('sidebar exploded');
}

describe('<AppErrorBoundary> (L2 round 5 — root safety net)', () => {
  afterEach(() => vi.restoreAllMocks());

  it('renders the translated fallback instead of an empty document', () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const { container } = render(
      <IntlProvider messages={en} locale="en" defaultLocale="en">
        <AppErrorBoundary>
          <Boom />
        </AppErrorBoundary>
      </IntlProvider>,
    );
    expect(container).not.toBeEmptyDOMElement();
    expect(screen.getByText("This page couldn't be shown")).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Reload' })).toBeInTheDocument();
  });

  it('still renders plain fallback text without any i18n context', () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    render(
      <AppErrorBoundary>
        <Boom />
      </AppErrorBoundary>,
    );
    expect(screen.getByRole('button', { name: /Reload/ })).toBeInTheDocument();
    expect(screen.getByRole('alert')).toBeInTheDocument();
  });

  it('passes children through when nothing throws', () => {
    render(
      <AppErrorBoundary>
        <p>ok</p>
      </AppErrorBoundary>,
    );
    expect(screen.getByText('ok')).toBeInTheDocument();
  });
});
