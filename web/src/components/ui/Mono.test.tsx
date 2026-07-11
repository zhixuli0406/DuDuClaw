import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { Mono } from './Mono';

describe('<Mono>', () => {
  it('renders machine values in the mono font with tabular numerals', () => {
    renderWithProviders(<Mono>$12.34</Mono>);
    const el = screen.getByText('$12.34');
    expect(el).toBeInTheDocument();
    expect(el.className).toContain('font-mono');
    expect(el.className).toContain('tabular-nums');
  });
});
