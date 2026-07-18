import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { Badge } from '../badge';

describe('<Badge>', () => {
  it('renders default pill styling', () => {
    renderWithProviders(<Badge>New</Badge>);
    const badge = screen.getByText('New');
    expect(badge).toHaveClass('rounded-4xl', 'h-5', 'bg-primary');
  });

  it('applies destructive + outline variants', () => {
    const { rerender } = renderWithProviders(
      <Badge variant="destructive">Err</Badge>
    );
    expect(screen.getByText('Err')).toHaveClass('bg-destructive/10', 'text-destructive');
    rerender(<Badge variant="outline">Out</Badge>);
    expect(screen.getByText('Out')).toHaveClass('border-border', 'text-foreground');
  });
});
