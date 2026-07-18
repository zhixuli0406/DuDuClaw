import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { Popover, PopoverTrigger, PopoverContent } from '../popover';

describe('<Popover>', () => {
  it('opens on trigger and shows the panel', async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <Popover>
        <PopoverTrigger>Display</PopoverTrigger>
        <PopoverContent>
          <span>Grouping options</span>
        </PopoverContent>
      </Popover>
    );

    expect(screen.queryByText('Grouping options')).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Display' }));

    const panel = await screen.findByText('Grouping options');
    expect(panel.closest('[data-slot=popover-content]')).toHaveClass(
      'bg-surface-raised',
      'rounded-lg'
    );
  });
});
