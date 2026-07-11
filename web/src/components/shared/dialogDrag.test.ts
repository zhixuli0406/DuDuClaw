import { describe, it, expect } from 'vitest';
import { clampDialogOffset } from './dialogDrag';

const DIALOG = { width: 400, height: 300 };
const VIEWPORT = { width: 1000, height: 800 };

describe('clampDialogOffset', () => {
  it('leaves a centered dialog unmoved at zero offset', () => {
    expect(clampDialogOffset({ x: 0, y: 0 }, DIALOG, VIEWPORT)).toEqual({ x: 0, y: 0 });
  });

  it('passes through a small in-bounds drag untouched', () => {
    expect(clampDialogOffset({ x: 50, y: -30 }, DIALOG, VIEWPORT)).toEqual({ x: 50, y: -30 });
  });

  it('clamps a far-right drag so `edge` px stay visible on the left', () => {
    const baseLeft = (VIEWPORT.width - DIALOG.width) / 2; // 300
    const out = clampDialogOffset({ x: 10_000, y: 0 }, DIALOG, VIEWPORT, { edge: 24 });
    // visible left = baseLeft + out.x must equal viewport.width - edge
    expect(baseLeft + out.x).toBe(VIEWPORT.width - 24);
  });

  it('clamps a far-left drag so `edge` px of the right edge stay visible', () => {
    const baseLeft = (VIEWPORT.width - DIALOG.width) / 2; // 300
    const out = clampDialogOffset({ x: -10_000, y: 0 }, DIALOG, VIEWPORT, { edge: 24 });
    const visibleLeft = baseLeft + out.x;
    // right edge = visibleLeft + width must equal `edge`
    expect(visibleLeft + DIALOG.width).toBe(24);
  });

  it('pins the header to the top when dragged up past the viewport', () => {
    const baseTop = (VIEWPORT.height - DIALOG.height) / 2; // 250
    const out = clampDialogOffset({ x: 0, y: -10_000 }, DIALOG, VIEWPORT);
    expect(baseTop + out.y).toBe(0);
  });

  it('keeps the full header band on-screen when dragged down', () => {
    const baseTop = (VIEWPORT.height - DIALOG.height) / 2; // 250
    const out = clampDialogOffset({ x: 0, y: 10_000 }, DIALOG, VIEWPORT, { headerHeight: 48 });
    // header top must sit at viewport.height - headerHeight
    expect(baseTop + out.y).toBe(VIEWPORT.height - 48);
  });

  it('produces valid bounds when the dialog is larger than the viewport', () => {
    const tall = { width: 1200, height: 1000 };
    const small = { width: 800, height: 600 };
    const out = clampDialogOffset({ x: 0, y: 0 }, tall, small, { headerHeight: 48 });
    const baseTop = (small.height - tall.height) / 2; // -200
    // header pinned to top (0) since centered position would hide it
    expect(baseTop + out.y).toBe(0);
    expect(Number.isFinite(out.x)).toBe(true);
    expect(Number.isFinite(out.y)).toBe(true);
  });
});
