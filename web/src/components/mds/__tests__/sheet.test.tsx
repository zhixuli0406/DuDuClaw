import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import {
  Sheet,
  SheetTrigger,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from '../sheet';

function Fixture({ side }: { side?: 'top' | 'right' | 'bottom' | 'left' }) {
  return (
    <Sheet>
      <SheetTrigger>Open panel</SheetTrigger>
      <SheetContent side={side}>
        <SheetHeader>
          <SheetTitle>Details</SheetTitle>
        </SheetHeader>
      </SheetContent>
    </Sheet>
  );
}

describe('<Sheet>', () => {
  it('opens from the trigger and anchors to the requested side', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Fixture side="left" />);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Open panel' }));
    const content = await screen.findByRole('dialog');
    expect(content).toHaveAttribute('data-side', 'left');
    expect(content).toHaveClass('left-0', 'ring-1');
    expect(screen.getByText('Details')).toBeInTheDocument();
  });
});
