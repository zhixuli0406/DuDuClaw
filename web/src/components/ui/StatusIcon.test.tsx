import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { StatusIcon } from './StatusIcon';

describe('<StatusIcon>', () => {
  it('renders a read-only labelled glyph when no onChange', () => {
    renderWithProviders(<StatusIcon status="in_progress" />);
    expect(screen.getByRole('img', { name: 'In progress' })).toBeInTheDocument();
  });

  it('opens a status picker and reports the chosen status', () => {
    const onChange = vi.fn();
    renderWithProviders(<StatusIcon status="todo" onChange={onChange} />);

    const trigger = screen.getByRole('button', { name: 'To do' });
    expect(trigger).toHaveAttribute('aria-expanded', 'false');
    fireEvent.click(trigger);
    expect(trigger).toHaveAttribute('aria-expanded', 'true');

    // 7 status options in the menu.
    const items = screen.getAllByRole('menuitemradio');
    expect(items).toHaveLength(7);

    fireEvent.click(screen.getByRole('menuitemradio', { name: 'Done' }));
    expect(onChange).toHaveBeenCalledWith('done');
  });
});
