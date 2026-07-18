import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { Button, buttonVariants } from '../button';

describe('<Button>', () => {
  it('renders a button with default variant + size classes', () => {
    renderWithProviders(<Button>Go</Button>);
    const btn = screen.getByRole('button', { name: 'Go' });
    expect(btn).toHaveClass('bg-primary', 'h-8', 'rounded-lg');
    expect(btn).toHaveAttribute('type', 'button');
  });

  it('applies variant + size via CVA', () => {
    renderWithProviders(
      <Button variant="brand" size="sm">
        Ship
      </Button>
    );
    const btn = screen.getByRole('button', { name: 'Ship' });
    expect(btn).toHaveClass('bg-brand', 'h-7');
  });

  it('exposes a buttonVariants helper for render composition', () => {
    expect(buttonVariants({ variant: 'ghost' })).toContain('hover:bg-muted');
    expect(buttonVariants({ size: 'icon' })).toContain('size-8');
  });

  it('fires onClick and respects disabled', () => {
    const onClick = vi.fn();
    const { rerender } = renderWithProviders(
      <Button onClick={onClick}>Tap</Button>
    );
    fireEvent.click(screen.getByRole('button', { name: 'Tap' }));
    expect(onClick).toHaveBeenCalledTimes(1);

    rerender(
      <Button onClick={onClick} disabled>
        Tap
      </Button>
    );
    fireEvent.click(screen.getByRole('button', { name: 'Tap' }));
    expect(onClick).toHaveBeenCalledTimes(1);
  });
});
