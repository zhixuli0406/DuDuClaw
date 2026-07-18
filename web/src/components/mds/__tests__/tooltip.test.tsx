import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import {
  Tooltip,
  TooltipProvider,
  TooltipTrigger,
  TooltipContent,
} from '../tooltip';

describe('<Tooltip>', () => {
  it('reveals its lightweight popup on focus', async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <TooltipProvider delay={0}>
        <Tooltip>
          <TooltipTrigger>Info</TooltipTrigger>
          <TooltipContent>Helpful hint</TooltipContent>
        </Tooltip>
      </TooltipProvider>
    );

    expect(screen.queryByText('Helpful hint')).not.toBeInTheDocument();
    await user.tab();
    const tip = await screen.findByText('Helpful hint');
    expect(tip.closest('[data-slot=tooltip-content]')).toHaveClass(
      'border-border',
      'bg-popover'
    );
  });
});
