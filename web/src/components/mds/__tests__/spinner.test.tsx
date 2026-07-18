import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { screen, act } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { Spinner } from '../spinner';

describe('<Spinner>', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('renders a labelled status glyph in a fixed-width mono cell', () => {
    renderWithProviders(<Spinner label="Working" />);
    const el = screen.getByRole('status', { name: 'Working' });
    expect(el).toHaveClass('font-mono', 'w-[1ch]', 'tabular-nums');
    expect(el.textContent).toBe('⠋');
  });

  it('advances braille frames on the interval', () => {
    renderWithProviders(<Spinner intervalMs={80} />);
    const el = screen.getByRole('status');
    expect(el.textContent).toBe('⠋');
    act(() => {
      vi.advanceTimersByTime(80);
    });
    expect(el.textContent).toBe('⠙');
  });
});
