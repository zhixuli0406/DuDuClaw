import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { Skeleton } from '../skeleton';

describe('<Skeleton>', () => {
  it('renders a pulsing placeholder', () => {
    renderWithProviders(<Skeleton className="h-4 w-20" data-testid="sk" />);
    const sk = screen.getByTestId('sk');
    expect(sk).toHaveClass('animate-pulse', 'bg-muted', 'rounded-md', 'h-4', 'w-20');
  });
});
