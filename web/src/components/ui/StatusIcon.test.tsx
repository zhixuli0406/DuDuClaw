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

    // Only the four statuses the gateway can persist (WP-A §2-6).
    const items = screen.getAllByRole('menuitemradio');
    expect(items).toHaveLength(4);
    expect(items.map((i) => i.textContent)).toEqual(['To do', 'In progress', 'Done', 'Stuck']);

    fireEvent.click(screen.getByRole('menuitemradio', { name: 'Done' }));
    expect(onChange).toHaveBeenCalledWith('done');
  });

  it('never offers a status the gateway cannot store (no silent dead clicks)', () => {
    renderWithProviders(<StatusIcon status="todo" onChange={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'To do' }));
    for (const dead of ['Backlog', 'In review', 'Cancelled']) {
      expect(screen.queryByRole('menuitemradio', { name: dead })).toBeNull();
    }
  });

  it('still renders a display-only glyph for statuses the picker no longer offers', () => {
    renderWithProviders(<StatusIcon status="cancelled" />);
    expect(screen.getByRole('img', { name: 'Cancelled' })).toBeInTheDocument();
  });
});

describe('<StatusIcon> unknown key (L2 round 4)', () => {
  it('falls back to a safe glyph + label instead of rendering an undefined component', () => {
    renderWithProviders(<StatusIcon status={'pending_approval' as never} />);
    expect(screen.getByRole('img', { name: 'To do' })).toBeInTheDocument();
  });
});

describe('<StatusIcon> null / undefined at runtime (L2 round 7)', () => {
  it('renders the safe glyph for both', () => {
    const { unmount } = renderWithProviders(<StatusIcon status={null as never} />);
    expect(screen.getByRole('img', { name: 'To do' })).toBeInTheDocument();
    unmount();
    renderWithProviders(<StatusIcon status={undefined as never} />);
    expect(screen.getByRole('img', { name: 'To do' })).toBeInTheDocument();
  });
});
