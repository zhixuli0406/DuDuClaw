import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { SwipeToArchive } from './SwipeToArchive';

describe('<SwipeToArchive>', () => {
  it('fires onArchive after a left drag past the threshold', () => {
    const onArchive = vi.fn();
    renderWithProviders(
      <SwipeToArchive onArchive={onArchive} threshold={96}>
        <div>收件項目</div>
      </SwipeToArchive>,
    );
    const surface = screen.getByRole('presentation');
    fireEvent.pointerDown(surface, { clientX: 240, button: 0, pointerType: 'mouse', pointerId: 1 });
    fireEvent.pointerMove(surface, { clientX: 80, pointerId: 1 }); // dx = -160
    fireEvent.pointerUp(surface, { clientX: 80, pointerId: 1 });
    expect(onArchive).toHaveBeenCalledTimes(1);
  });

  it('springs back (no archive) on a short drag', () => {
    const onArchive = vi.fn();
    renderWithProviders(
      <SwipeToArchive onArchive={onArchive} threshold={96}>
        <div>收件項目</div>
      </SwipeToArchive>,
    );
    const surface = screen.getByRole('presentation');
    fireEvent.pointerDown(surface, { clientX: 200, button: 0, pointerType: 'mouse', pointerId: 1 });
    fireEvent.pointerMove(surface, { clientX: 180, pointerId: 1 }); // dx = -20
    fireEvent.pointerUp(surface, { clientX: 180, pointerId: 1 });
    expect(onArchive).not.toHaveBeenCalled();
  });
});
