import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { GroupHeader } from './GroupHeader';

describe('<GroupHeader>', () => {
  it('shows a label + count and toggles', () => {
    const onToggle = vi.fn();
    renderWithProviders(<GroupHeader label="要我拍板" count={3} collapsed={false} onToggle={onToggle} />);
    expect(screen.getByText('要我拍板')).toBeInTheDocument();
    expect(screen.getByText('3')).toBeInTheDocument();
    const btn = screen.getByRole('button', { expanded: true });
    fireEvent.click(btn);
    expect(onToggle).toHaveBeenCalled();
  });
});
