import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { InlineEditor } from './InlineEditor';

describe('<InlineEditor>', () => {
  it('commits a trimmed edit on Enter (single-line)', () => {
    const onCommit = vi.fn();
    renderWithProviders(
      <InlineEditor value="舊標題" onCommit={onCommit} ariaLabel="標題" />,
    );
    fireEvent.click(screen.getByRole('button', { name: '標題' }));
    const input = screen.getByRole('textbox');
    fireEvent.change(input, { target: { value: '  新標題  ' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(onCommit).toHaveBeenCalledWith('新標題');
  });

  it('cancels on Escape without committing', () => {
    const onCommit = vi.fn();
    renderWithProviders(<InlineEditor value="原值" onCommit={onCommit} ariaLabel="欄位" />);
    fireEvent.click(screen.getByRole('button', { name: '欄位' }));
    const input = screen.getByRole('textbox');
    fireEvent.change(input, { target: { value: '亂改' } });
    fireEvent.keyDown(input, { key: 'Escape' });
    expect(onCommit).not.toHaveBeenCalled();
    // Reverts to resting text button.
    expect(screen.getByRole('button', { name: '欄位' })).toBeInTheDocument();
  });

  it('rejects an empty commit', () => {
    const onCommit = vi.fn();
    renderWithProviders(<InlineEditor value="有值" onCommit={onCommit} ariaLabel="欄位" />);
    fireEvent.click(screen.getByRole('button', { name: '欄位' }));
    const input = screen.getByRole('textbox');
    fireEvent.change(input, { target: { value: '   ' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(onCommit).not.toHaveBeenCalled();
  });
});
