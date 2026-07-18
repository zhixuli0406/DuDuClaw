import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { Checkbox } from '../checkbox';

describe('<Checkbox>', () => {
  it('renders an unchecked box with base styling', () => {
    renderWithProviders(<Checkbox aria-label="agree" />);
    const cb = screen.getByRole('checkbox', { name: 'agree' });
    expect(cb).toHaveClass('size-4', 'border-input');
    expect(cb).toHaveAttribute('aria-checked', 'false');
  });

  it('checks and reports the new state', async () => {
    const user = userEvent.setup();
    const onCheckedChange = vi.fn();
    renderWithProviders(
      <Checkbox aria-label="agree" onCheckedChange={onCheckedChange} />
    );
    const cb = screen.getByRole('checkbox', { name: 'agree' });
    await user.click(cb);
    expect(onCheckedChange).toHaveBeenCalledWith(true, expect.anything());
    expect(cb).toHaveAttribute('aria-checked', 'true');
  });
});
