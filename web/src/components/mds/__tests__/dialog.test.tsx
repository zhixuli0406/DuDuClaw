import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import {
  Dialog,
  DialogTrigger,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '../dialog';

function Fixture() {
  return (
    <Dialog>
      <DialogTrigger>Open</DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Confirm</DialogTitle>
          <DialogDescription>Are you sure?</DialogDescription>
        </DialogHeader>
      </DialogContent>
    </Dialog>
  );
}

describe('<Dialog>', () => {
  it('is closed initially, opens on trigger, and closes on Escape', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Fixture />);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Open' }));
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveClass('rounded-xl', 'bg-surface-raised');
    expect(screen.getByText('Confirm')).toHaveClass('text-base', 'leading-none');

    await user.keyboard('{Escape}');
    await screen.findByRole('button', { name: 'Open' });
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('renders a close button inside the content', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Fixture />);
    await user.click(screen.getByRole('button', { name: 'Open' }));
    expect(await screen.findByRole('button', { name: 'Close' })).toBeInTheDocument();
  });
});
