import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { InboxIcon } from 'lucide-react';
import { renderWithProviders } from '@/test/render';
import { Empty } from '../empty';
import { Button } from '../button';

describe('<Empty>', () => {
  it('renders icon, title, description and action', () => {
    renderWithProviders(
      <Empty
        icon={InboxIcon}
        title="Nothing here"
        description="Create your first item"
        action={<Button>New</Button>}
      />
    );
    expect(screen.getByText('Nothing here')).toBeInTheDocument();
    expect(screen.getByText('Create your first item')).toHaveClass('max-w-md');
    expect(screen.getByRole('button', { name: 'New' })).toBeInTheDocument();
  });

  it('applies destructive tone', () => {
    renderWithProviders(
      <Empty title="Failed" tone="destructive" data-testid="e" />
    );
    const root = screen.getByText('Failed').closest('[data-slot=empty]')!;
    expect(root).toHaveAttribute('data-tone', 'destructive');
    expect(screen.getByText('Failed')).toHaveClass('text-destructive');
  });

  it('applies dashed variant container', () => {
    renderWithProviders(<Empty title="Empty" variant="dashed" />);
    const root = screen.getByText('Empty').closest('[data-slot=empty]')!;
    expect(root).toHaveClass('border-dashed', 'rounded-lg');
  });
});
