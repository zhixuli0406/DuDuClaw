import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { ErrorState } from '../error-state';

describe('<ErrorState>', () => {
  it('renders a persistent alert with a plain-language default title', () => {
    renderWithProviders(<ErrorState />);
    const root = screen.getByRole('alert');
    expect(root).toHaveAttribute('data-slot', 'error-state');
    expect(screen.getByText("Couldn't load this")).toBeInTheDocument();
  });

  it('describes the failure from the classified reason, not the raw error', () => {
    renderWithProviders(<ErrorState error={new Error('Failed to fetch')} />);
    expect(
      screen.getByText(/Can't reach the backend service/)
    ).toBeInTheDocument();
    expect(screen.queryByText(/Failed to fetch/)).not.toBeInTheDocument();
  });

  it('renders a retry button only when onRetry is supplied', async () => {
    const onRetry = vi.fn();
    const { rerender } = renderWithProviders(<ErrorState />);
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();

    rerender(<ErrorState onRetry={onRetry} />);
    await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it('hides the sanitized detail behind a disclosure', async () => {
    renderWithProviders(
      <ErrorState error={new Error('rpc dispatch failed: token=abcdefgh1234')} />
    );
    const toggle = screen.getByRole('button', {
      name: 'Show technical details',
    });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByText(/rpc dispatch failed/)).toBeNull();

    await userEvent.click(toggle);
    const detail = screen.getByText(/rpc dispatch failed/);
    expect(detail).toHaveAttribute('data-slot', 'error-state-detail');
    // Sanitized: the credential never reaches the DOM.
    expect(detail.textContent).toContain('token=***');
    expect(detail.textContent).not.toContain('abcdefgh1234');
    expect(
      screen.getByRole('button', { name: 'Hide technical details' })
    ).toHaveAttribute('aria-expanded', 'true');
  });

  it('omits the disclosure when there is no detail to show', () => {
    renderWithProviders(<ErrorState />);
    expect(screen.queryByRole('button', { name: /technical details/ })).toBeNull();
  });

  it('honours explicit title, description and extra action', () => {
    renderWithProviders(
      <ErrorState
        title="Couldn't delete the plan"
        description="Nothing was removed."
        action={<button type="button">Go back</button>}
      />
    );
    expect(screen.getByText("Couldn't delete the plan")).toBeInTheDocument();
    expect(screen.getByText('Nothing was removed.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Go back' })).toBeInTheDocument();
  });

  it('renders the inline banner variant', () => {
    renderWithProviders(<ErrorState variant="inline" onRetry={() => {}} />);
    expect(screen.getByRole('alert')).toHaveAttribute('data-variant', 'inline');
    expect(
      screen.getByRole('button', { name: 'Try again' })
    ).toBeInTheDocument();
  });

  // WCAG AA: over the inline banner's tinted bg the shared --destructive /
  // --muted-foreground tokens land ~3.8:1 in light mode. The fix darkens the
  // text in light mode only (scoped OKLCH), restoring the tokens under `dark:`.
  it('darkens light-mode title/body text so the tinted banner clears AA', () => {
    renderWithProviders(
      <ErrorState variant="inline" title="Boom" description="Details here" />
    );
    const title = screen.getByText('Boom');
    const body = screen.getByText('Details here');
    expect(title.className).toContain('oklch(0.505_0.21_27.325)');
    expect(title.className).toContain('dark:text-destructive');
    expect(body.className).toContain('oklch(0.48_0.016_285.938)');
    expect(body.className).toContain('dark:text-muted-foreground');
  });
});
