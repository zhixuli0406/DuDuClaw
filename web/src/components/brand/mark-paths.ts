/**
 * DuDuClaw C1 mark geometry (poised sitting cat + amber tail), copied verbatim
 * from the brand master `duduclaw-mark.svg` (potrace output, IoU >= 0.9975 vs
 * the 1254 px raster master). Do not hand-edit the path data: re-export it from
 * the brand kit instead.
 *
 * Coordinates are potrace units. Wrap the paths in `MARK_POTRACE_TRANSFORM` to
 * land them in the 1254 x 1254 master pixel space (y down); `MARK_VIEWBOX` is
 * the master's tight square viewBox in that space.
 */

/** Master's square viewBox (~2% padding) in 1254 px master space. */
export const MARK_VIEWBOX = '23 38 1231 1231';

/** Maps potrace units into 1254 px master space. */
export const MARK_POTRACE_TRANSFORM = 'translate(0,1254) scale(0.1,-0.1)';

/** Ink layer, part 1: ears, head, chest (the right-hand navy piece). */
export const MARK_INK_HEAD_PATH =
  'M6520 11193 c-28 -26 -62 -82 -251 -416 -43 -76 -94 -164 -113 -195 -18 -31 -62 -106 -96 -167 -35 -60 -164 -285 -288 -500 -496 -862 -577 -1033 -672 -1425 -58 -240 -62 -295 -67 -801 -6 -620 10 -770 116 -1039 153 -394 445 -688 1155 -1170 27 -18 195 -137 336 -239 58 -41 175 -125 261 -185 86 -61 279 -201 428 -311 149 -110 281 -207 293 -215 21 -14 21 -14 14 70 -3 47 -13 308 -21 580 -8 272 -20 615 -25 761 -25 668 -6 871 104 1087 147 290 361 460 776 616 339 128 442 186 593 334 186 184 271 354 272 542 0 128 -32 207 -112 281 -27 24 -238 227 -470 452 l-423 410 -2 612 -3 613 -24 26 c-27 28 -76 42 -113 31 -12 -4 -233 -217 -490 -473 l-467 -466 -49 105 c-27 57 -122 262 -212 454 -90 193 -166 355 -170 360 -4 6 -34 68 -65 138 -62 139 -80 157 -149 157 -25 0 -45 -8 -66 -27z';

/** Ink layer, part 2: back and haunch (the left-hand navy piece). */
export const MARK_INK_BODY_PATH =
  'M4392 7085 c-968 -643 -1509 -1112 -2023 -1755 -705 -882 -1024 -1836 -924 -2770 93 -880 722 -1648 1455 -1776 88 -15 328 -16 3100 -9 2134 5 3007 10 3012 18 26 42 21 611 -7 787 -120 760 -636 1571 -1476 2319 -449 399 -724 612 -1499 1156 -639 449 -895 679 -1093 985 -191 295 -274 590 -293 1040 -4 88 -10 160 -13 160 -3 0 -111 -70 -239 -155z';

/** Accent layer: the amber tail. */
export const MARK_ACCENT_PATH =
  'M11255 7273 c-201 -28 -596 -153 -893 -282 -1455 -636 -2239 -1930 -1835 -3031 69 -189 217 -461 390 -716 256 -378 431 -747 517 -1089 84 -336 88 -792 11 -1206 -34 -183 -36 -172 28 -164 607 75 1114 503 1312 1110 59 178 76 286 82 501 9 310 -26 500 -191 1029 -232 741 -289 1069 -256 1465 44 539 223 936 815 1805 38 55 105 154 151 221 104 153 126 196 127 250 1 94 -87 131 -258 107z';

/**
 * Landmarks in 1254 px master space, measured from a 1:1 render of the master.
 * Used by the mascot to place its face and paws on the silhouette.
 */
export const MARK_LANDMARKS = {
  /** Tight bounding box of the whole mark. */
  bbox: { x0: 143, y0: 132, x1: 1150, y1: 1176 },
  /** Pivot at the base of the tail (where it meets the ground). */
  tailBase: { x: 965, y: 1172 },
} as const;
