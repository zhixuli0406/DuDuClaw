import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { Separator } from '../separator';

describe('<Separator>', () => {
  it('renders a horizontal divider by default', () => {
    renderWithProviders(<Separator data-testid="sep" />);
    const sep = screen.getByTestId('sep');
    expect(sep).toHaveClass('h-px', 'w-full', 'bg-border');
  });

  it('supports vertical orientation', () => {
    renderWithProviders(<Separator orientation="vertical" data-testid="sep" />);
    expect(screen.getByTestId('sep')).toHaveClass('w-px', 'h-full');
  });
});
