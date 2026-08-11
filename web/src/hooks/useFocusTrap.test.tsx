import { useRef } from 'react';
import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useFocusTrap } from './useFocusTrap';

function Harness({ active }: { readonly active: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  useFocusTrap(ref, active);
  return (
    <div>
      <button type="button">before</button>
      <div ref={ref} data-testid="trap">
        <button type="button">first</button>
        <button type="button">second</button>
        <button type="button">last</button>
      </div>
      <button type="button">after</button>
    </div>
  );
}

describe('useFocusTrap', () => {
  it('moves focus into the container on activation when focus was outside it', () => {
    render(<Harness active={true} />);
    expect(screen.getByText('first')).toHaveFocus();
  });

  it('does nothing when inactive — focus stays wherever it was', () => {
    render(<Harness active={false} />);
    expect(document.body).toHaveFocus();
  });

  it('Tab wraps from the last focusable element back to the first', async () => {
    const user = userEvent.setup();
    render(<Harness active={true} />);
    screen.getByText('last').focus();

    await user.tab();

    expect(screen.getByText('first')).toHaveFocus();
  });

  it('Shift+Tab wraps from the first focusable element back to the last', async () => {
    const user = userEvent.setup();
    render(<Harness active={true} />);
    screen.getByText('first').focus();

    await user.tab({ shift: true });

    expect(screen.getByText('last')).toHaveFocus();
  });

  it('never lets Tab reach an element outside the container', async () => {
    const user = userEvent.setup();
    render(<Harness active={true} />);

    for (let i = 0; i < 6; i++) {
      await user.tab();
      expect(screen.getByTestId('trap').contains(document.activeElement)).toBe(true);
    }
    expect(screen.queryByText('before')).not.toHaveFocus();
    expect(screen.queryByText('after')).not.toHaveFocus();
  });

  it('restores focus to the previously-focused element on deactivation', () => {
    const { rerender } = render(<Harness active={false} />);
    const before = screen.getByText('before');
    before.focus();
    expect(before).toHaveFocus();

    rerender(<Harness active={true} />);
    expect(screen.getByText('first')).toHaveFocus();

    rerender(<Harness active={false} />);
    expect(before).toHaveFocus();
  });
});
