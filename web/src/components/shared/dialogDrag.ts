export interface Size {
  width: number;
  height: number;
}

export interface Offset {
  x: number;
  y: number;
}

export interface ClampOpts {
  /** Height of the draggable header that must stay fully on-screen. */
  headerHeight?: number;
  /** Minimum px of the dialog kept visible on each horizontal edge. */
  edge?: number;
}

function clamp(v: number, min: number, max: number): number {
  if (max < min) return min;
  return Math.min(Math.max(v, min), max);
}

/**
 * Clamp a drag offset so a centered <dialog> stays reachable inside the viewport.
 *
 * The dialog's untransformed position is the centered box (`margin: auto`). Given
 * a desired translate offset, this returns the offset adjusted so that:
 *  - at least `edge` px of the dialog remain visible on the left/right, and
 *  - the full header band ([top, top + headerHeight]) stays on-screen so the drag
 *    handle and close button are always clickable.
 *
 * Pure + deterministic — unit tested; safe when the dialog is larger than the
 * viewport (bounds stay valid).
 */
export function clampDialogOffset(
  offset: Offset,
  dialog: Size,
  viewport: Size,
  opts: ClampOpts = {},
): Offset {
  const headerHeight = opts.headerHeight ?? 48;
  const edge = opts.edge ?? 24;

  const baseLeft = (viewport.width - dialog.width) / 2;
  const baseTop = (viewport.height - dialog.height) / 2;

  // Visible left bounds: keep `edge` px on-screen on whichever side is dragged off.
  const minLeft = edge - dialog.width;
  const maxLeft = viewport.width - edge;
  const left = clamp(baseLeft + offset.x, minLeft, maxLeft);

  // Vertical: the header band must stay fully visible.
  const minTop = 0;
  const maxTop = Math.max(0, viewport.height - headerHeight);
  const top = clamp(baseTop + offset.y, minTop, maxTop);

  return { x: left - baseLeft, y: top - baseTop };
}
