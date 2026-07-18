import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
} from '../dropdown-menu';

describe('<DropdownMenu>', () => {
  it('opens on trigger and invokes the item action', async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    renderWithProviders(
      <DropdownMenu>
        <DropdownMenuTrigger>Menu</DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuLabel>Actions</DropdownMenuLabel>
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={onSelect}>Edit</DropdownMenuItem>
          <DropdownMenuItem variant="destructive">Delete</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    );

    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Menu' }));

    const menu = await screen.findByRole('menu');
    expect(menu).toHaveClass('rounded-lg', 'bg-surface-raised');
    expect(
      screen.getByRole('menuitem', { name: 'Delete' })
    ).toHaveAttribute('data-variant', 'destructive');

    await user.click(screen.getByRole('menuitem', { name: 'Edit' }));
    expect(onSelect).toHaveBeenCalledTimes(1);
  });
});
