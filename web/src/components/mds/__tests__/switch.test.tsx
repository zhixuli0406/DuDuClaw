import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { Switch } from '../switch';

describe('<Switch>', () => {
  it('renders an unchecked switch with track styling', () => {
    renderWithProviders(<Switch aria-label="wifi" />);
    const sw = screen.getByRole('switch', { name: 'wifi' });
    expect(sw).toHaveClass('h-5', 'w-9', 'rounded-full');
    expect(sw).toHaveAttribute('aria-checked', 'false');
  });

  it('toggles and reports the new checked state', async () => {
    const user = userEvent.setup();
    const onCheckedChange = vi.fn();
    renderWithProviders(
      <Switch aria-label="wifi" onCheckedChange={onCheckedChange} />
    );
    const sw = screen.getByRole('switch', { name: 'wifi' });
    await user.click(sw);
    expect(onCheckedChange).toHaveBeenCalledWith(true, expect.anything());
    expect(sw).toHaveAttribute('aria-checked', 'true');
  });
});
