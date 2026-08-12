/**
 * thinking-orb-engine — pure rendering math for `mds/thinking-orb.tsx`'s
 * dotted "thought orb" animations: camera projection, per-mode dot layout,
 * and the size/count tuning tables. No React/DOM here by design (see
 * `coding-style.md` "many small files" — this half of the component is a
 * plain data pipeline and is kept testable/readable independent of the
 * canvas lifecycle in `thinking-orb.tsx`).
 *
 * This is NOT the `thinking-orbs` npm package — DuDuClaw does not add new
 * runtime dependencies for UI chrome. It is a from-scratch TypeScript
 * rewrite produced by reading the published source of `thinking-orbs`
 * v0.2.0 (https://github.com/Jakubantalik/thinking-orbs — `dist/index.es.js`
 * / `dist/index.d.ts`), reproducing its particle-system math with
 * DuDuClaw-native theming (see `paintDots` below) swapped in.
 *
 * Upstream license (MIT), reproduced per its terms since this file is a
 * derivative of its algorithms:
 *
 *   Copyright (c) Jakub Antalik
 *
 *   Permission is hereby granted, free of charge, to any person obtaining a
 *   copy of this software and associated documentation files (the
 *   "Software"), to deal in the Software without restriction, including
 *   without limitation the rights to use, copy, modify, merge, publish,
 *   distribute, sublicense, and/or sell copies of the Software, and to
 *   permit persons to whom the Software is furnished to do so, subject to
 *   the following conditions: the above copyright notice and this
 *   permission notice shall be included in all copies or substantial
 *   portions of the Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT
 *   WARRANTY OF ANY KIND. Full text:
 *   https://github.com/Jakubantalik/thinking-orbs/blob/main/LICENSE
 *
 * Upstream ships nine hand-tuned states at two sizes (64/20 CSS px). This
 * port covers the six DuDuClaw actually consumes today — working /
 * searching / solving / listening / composing / shaping — via
 * `components/chat/ThinkingOrbIndicator.tsx`'s state mapping. `connecting`
 * ("web"), `weaving` ("braid") and `breathing` ("ring") were deliberately
 * NOT ported: no call site references them today. Add them here, following
 * the same read-upstream-then-port pattern, if a call site ever needs them.
 *
 * Theming: `paintDots` takes a live-resolved ink color string (the mds
 * `--foreground` CSS custom property — see `thinking-orb.tsx`, never a
 * hardcoded hex) instead of upstream's manual dark/light detection. Per-dot
 * depth shading is collapsed from upstream's black/white channel flip into
 * a single alpha ramp against that one ink color — a deliberate
 * simplification to satisfy "no hardcoded hex, read the token" while
 * preserving the layered depth-fade look.
 */

export type ThinkingOrbSize = 64 | 20;

export type ThinkingOrbState =
  | 'working'
  | 'searching'
  | 'solving'
  | 'listening'
  | 'composing'
  | 'shaping';

// ─────────────────────────────────────────────────────────────────────────
// Shared math (ported from upstream's noise / projection helpers)
// ─────────────────────────────────────────────────────────────────────────

/** Deterministic pseudo-random hash in [0,1), sine-based. */
function hash2(x: number, y: number): number {
  const s = Math.sin(x * 12.9898 + y * 78.233) * 43758.5453;
  return s - Math.floor(s);
}

/** i-th of n points evenly spread on a unit sphere (golden-angle spiral). */
function fibonacciSpherePoint(i: number, n: number): [number, number, number] {
  const goldenAngle = Math.PI * (3 - Math.sqrt(5));
  const y = 1 - (2 * (i + 0.5)) / n;
  const r = Math.sqrt(Math.max(0, 1 - y * y));
  const theta = i * goldenAngle;
  return [r * Math.cos(theta), y, r * Math.sin(theta)];
}

/** Shortest signed angular distance a-b, wrapped to [-pi, pi]. */
function angleDiff(a: number, b: number): number {
  return Math.atan2(Math.sin(a - b), Math.cos(a - b));
}

type Projector = (x: number, y: number, z: number) => [number, number, number];

/**
 * Builds a fixed-camera (yaw, pitch) orthographic projector: 3D coords ->
 * [screenX, screenY, depth] centered at (cx, cy), scaled by `radius`.
 */
function makeProjector(yaw: number, pitch: number, cx: number, cy: number, radius: number): Projector {
  const sinYaw = Math.sin(yaw);
  const cosYaw = Math.cos(yaw);
  const sinPitch = Math.sin(pitch);
  const cosPitch = Math.cos(pitch);
  return (x, y, z) => {
    const rx = x * cosYaw + z * sinYaw;
    const rzYaw = -x * sinYaw + z * cosYaw;
    const ry = y * cosPitch - rzYaw * sinPitch;
    const rz = y * sinPitch + rzYaw * cosPitch;
    return [cx + rx * radius, cy - ry * radius, rz];
  };
}

/** `(size / 300) ** exponent` — the density-vs-size power curve upstream tunes per mode. */
function densityPower(size: number, exponent: number): number {
  return (size / 300) ** exponent;
}

// ─────────────────────────────────────────────────────────────────────────
// Dot painter
// ─────────────────────────────────────────────────────────────────────────

interface Dot {
  x: number;
  y: number;
  /** Camera-space depth, used both for painter's-algorithm sort order and glow terms. */
  z: number;
  r: number;
  /** 0 = near/opaque, 1 = far/faded (upstream's "white" channel, ported as an alpha ramp — see file header). */
  white: number;
  /** Extra opacity multiplier (ghost trails, scan glow, etc). @default 1 */
  a?: number;
}

/** `color-mix()`-based translucency against a single themed ink color — works
 *  with any resolved CSS color format (oklch/hsl/rgb) without string parsing. */
function mixInk(inkColor: string, alpha: number): string {
  const pct = Math.round(Math.min(1, Math.max(0, alpha)) * 1000) / 10;
  return `color-mix(in srgb, ${inkColor} ${pct}%, transparent)`;
}

function paintDots(ctx: CanvasRenderingContext2D, dots: Dot[], inkColor: string, rMin = 0.3): void {
  dots.sort((p, q) => p.z - q.z);
  for (const dot of dots) {
    const baseAlpha = dot.a ?? 1;
    if (baseAlpha < 0.02) continue;
    const white = Math.min(1, Math.max(0, dot.white));
    const alpha = Math.min(1, baseAlpha * (1 - white));
    if (alpha < 0.02) continue;
    ctx.fillStyle = mixInk(inkColor, alpha);
    ctx.beginPath();
    ctx.arc(dot.x, dot.y, Math.max(rMin, dot.r), 0, Math.PI * 2);
    ctx.fill();
  }
}

// ─────────────────────────────────────────────────────────────────────────
// Per-mode draw functions
// ─────────────────────────────────────────────────────────────────────────

export interface ModeOpts {
  [key: string]: number | undefined;
}

export type DrawFn = (ctx: CanvasRenderingContext2D, size: number, t: number, ink: string, opts: ModeOpts) => void;

/** working — particles on tilted orbits. */
function drawOrbits(ctx: CanvasRenderingContext2D, size: number, t: number, ink: string, opts: ModeOpts) {
  const cx = size / 2;
  const cy = size / 2;
  const sphereR = (size / 2) * 0.82;
  const project = makeProjector(t * 0.12, 0.3, cx, cy, 1);
  const scale = densityPower(size, opts.rsPow ?? 0.6);
  const orbitCount = opts.orbitN ?? 12;
  const ghostCount = opts.ghostN ?? 40;
  const particleCount = opts.particles ?? 3;
  const dots: Dot[] = [];

  for (let h = 0; h < orbitCount; h++) {
    const rSeed = hash2(h, 1.7);
    const phaseSeed = hash2(h, 5.2);
    const dirSeed = hash2(h, 8.9);
    const orbitRadius = sphereR * (0.45 + 0.52 * rSeed);
    const azimuth = rSeed * 2 * Math.PI;
    const polar = Math.acos(2 * phaseSeed - 1);
    const axisX = Math.sin(polar) * Math.cos(azimuth);
    const axisY = Math.cos(polar);
    const axisZ = Math.sin(polar) * Math.sin(azimuth);

    // Two basis vectors spanning the orbit's tilted plane, orthogonal to the axis.
    let bx = -axisY;
    let by = axisX;
    const bz = 0;
    const bLen = Math.max(1e-6, Math.sqrt(bx * bx + by * by));
    bx /= bLen;
    by /= bLen;
    const cx2 = axisY * bz - axisZ * by;
    const cy2 = axisZ * bx - axisX * bz;
    const cz2 = axisX * by - axisY * bx;
    const spin = (0.25 + 0.55 * dirSeed) * (dirSeed > 0.5 ? 1 : -1);

    for (let g = 0; g < ghostCount; g++) {
      const angle = (g / ghostCount) * 2 * Math.PI;
      const [px, py, pz] = project(
        (bx * Math.cos(angle) + cx2 * Math.sin(angle)) * orbitRadius,
        (by * Math.cos(angle) + cy2 * Math.sin(angle)) * orbitRadius,
        (bz * Math.cos(angle) + cz2 * Math.sin(angle)) * orbitRadius,
      );
      const depth = (pz / orbitRadius + 1) / 2;
      dots.push({
        x: px,
        y: py,
        z: pz,
        r: (opts.ghostR ?? 0.9) * scale,
        white: 0.72,
        a: (opts.ghostA ?? 0.5) * (0.4 + 0.6 * depth),
      });
    }

    for (let p = 0; p < particleCount; p++) {
      const angle = t * spin + (p / particleCount) * 2 * Math.PI + phaseSeed * 6;
      const [px, py, pz] = project(
        (bx * Math.cos(angle) + cx2 * Math.sin(angle)) * orbitRadius,
        (by * Math.cos(angle) + cy2 * Math.sin(angle)) * orbitRadius,
        (bz * Math.cos(angle) + cz2 * Math.sin(angle)) * orbitRadius,
      );
      const depth = (pz / orbitRadius + 1) / 2;
      dots.push({
        x: px,
        y: py,
        z: pz,
        r: ((opts.partR ?? 1.2) + (opts.partRDepth ?? 1.6) * depth) * scale,
        white: 0.3 - 0.22 * depth,
      });
    }
  }

  paintDots(ctx, dots, ink, opts.rMin);
}

/** searching — a scan meridian sweeps a dotted globe. */
function drawGlobe(ctx: CanvasRenderingContext2D, size: number, t: number, ink: string, opts: ModeOpts) {
  const cx = size / 2;
  const cy = size / 2;
  const sphereR = (size / 2) * 0.82;
  const pitch = 0.4 + 0.06 * Math.sin(t * 0.35);
  const project = makeProjector(t * 0.5, pitch, cx, cy, sphereR);
  const scanAngle = t * (0.5 + 1.2 * (opts.scanMul ?? 1));
  const scale = densityPower(size, opts.rsPow ?? 0.6);
  const dimBase = opts.dimBase ?? 1;
  const latRings = opts.latRings ?? 17;
  const lonDensity = opts.lonDensity ?? 44;
  const dots: Dot[] = [];

  for (let ring = 0; ring <= latRings; ring++) {
    const lat = -Math.PI / 2 + (ring / latRings) * Math.PI;
    const cosLat = Math.cos(lat);
    const sinLat = Math.sin(lat);
    const lonCount = Math.max(1, Math.round(Math.abs(cosLat) * lonDensity));
    for (let lonIdx = 0; lonIdx < lonCount; lonIdx++) {
      const lon = (lonIdx / lonCount) * 2 * Math.PI;
      const [px, py, pz] = project(cosLat * Math.cos(lon), sinLat, cosLat * Math.sin(lon));
      const depth = (pz + 1) / 2;
      const angleToScan = angleDiff(lon + t * 0.5, scanAngle);
      const scanGlow = Math.exp(-(angleToScan * angleToScan) / 0.18) * Math.max(0, pz);
      dots.push({
        x: px,
        y: py,
        z: pz,
        r: ((opts.rBase ?? 0.6) + (opts.rDepth ?? 1.7) * depth + (opts.rBoost ?? 1) * scanGlow) * scale,
        white: (opts.inkFar ?? 0.62) - (opts.inkSpan ?? 0.54) * depth,
        a: dimBase + (1 - dimBase) * Math.min(1, scanGlow),
      });
    }
  }
  paintDots(ctx, dots, ink, opts.rMin);
}

/** One rubik "slice": axis-aligned band that rotates by `ang` when active. */
interface RubikSlice {
  axis: 0 | 1 | 2;
  lo: number;
  hi: number;
  ang: number;
}

function makeRubikSlices(count: number): RubikSlice[] {
  const slices: RubikSlice[] = [];
  for (let s = 0; s < count; s++) {
    const axis = Math.min(2, Math.floor(hash2(s, 2.3) * 3)) as 0 | 1 | 2;
    const lo = -1 + 0.5 * Math.min(3, Math.floor(hash2(s, 5.9) * 4));
    const sign = hash2(s, 7.7) < 0.5 ? 1 : -1;
    slices.push({ axis, lo, hi: lo + 0.5, ang: (sign * Math.PI) / 2 });
  }
  return slices;
}

interface RubikTimeline {
  amount: number[];
  active: number;
}

/** Scramble-then-solve timeline: slices twist in one at a time, hold, then untwist in reverse. */
function rubikTimeline(t: number, count: number, stepSeconds: number, pauseSeconds: number): RubikTimeline {
  const cycle = 2 * count * stepSeconds + pauseSeconds;
  const pos = t % cycle;
  const amount = new Array(count).fill(0);
  let active = -1;
  if (pos < 2 * count * stepSeconds) {
    const step = Math.floor(pos / stepSeconds);
    const progress = (pos - step * stepSeconds) / stepSeconds;
    const eased = 1 - (1 - Math.min(1, progress / 0.7)) ** 3;
    if (step < count) {
      for (let i = 0; i < step; i++) amount[i] = 1;
      amount[step] = eased;
      active = step;
    } else {
      const idx = 2 * count - 1 - step;
      for (let i = 0; i < idx; i++) amount[i] = 1;
      amount[idx] = 1 - eased;
      active = idx;
    }
  }
  return { amount, active };
}

/** Applies all active rubik slices to a point; reports whether the currently-animating slice touched it. */
function applyRubikSlices(
  point: [number, number, number],
  slices: RubikSlice[],
  timeline: RubikTimeline,
): [number, number, number, boolean] {
  let [x, y, z] = point;
  let touchedActive = false;
  for (let i = 0; i < slices.length; i++) {
    if (timeline.amount[i] <= 0) continue;
    const slice = slices[i];
    const coord = slice.axis === 0 ? x : slice.axis === 1 ? y : z;
    if (coord < slice.lo || coord >= slice.hi) continue;
    if (i === timeline.active) touchedActive = true;
    const angle = slice.ang * timeline.amount[i];
    const c = Math.cos(angle);
    const s = Math.sin(angle);
    if (slice.axis === 0) {
      const ny = y * c - z * s;
      z = y * s + z * c;
      y = ny;
    } else if (slice.axis === 1) {
      const nx = x * c + z * s;
      z = -x * s + z * c;
      x = nx;
    } else {
      const nx = x * c - y * s;
      y = x * s + y * c;
      x = nx;
    }
  }
  return [x, y, z, touchedActive];
}

/** solving — bands scramble in quarter turns, then click back solved. */
function drawRubik(ctx: CanvasRenderingContext2D, size: number, t: number, ink: string, opts: ModeOpts) {
  const cx = size / 2;
  const cy = size / 2;
  const sphereR = (size / 2) * 0.82;
  const project = makeProjector(t * 0.55, 0.35 + 0.1 * Math.sin(t * 0.9), cx, cy, sphereR);
  const scale = densityPower(size, opts.rsPow ?? 0.6);
  const moveCount = opts.moveCount ?? 14;
  const slices = makeRubikSlices(moveCount);
  const timeline = rubikTimeline(t, moveCount, 0.42, 1.2);
  const latRings = opts.latRings ?? 15;
  const lonDensity = opts.lonDensity ?? 40;
  const dots: Dot[] = [];

  for (let ring = 0; ring <= latRings; ring++) {
    const lat = -Math.PI / 2 + (ring / latRings) * Math.PI;
    const cosLat = Math.cos(lat);
    const sinLat = Math.sin(lat);
    const lonCount = Math.max(1, Math.round(Math.abs(cosLat) * lonDensity));
    for (let lonIdx = 0; lonIdx < lonCount; lonIdx++) {
      const lon = (lonIdx / lonCount) * 2 * Math.PI;
      const [rx, ry, rz, isActive] = applyRubikSlices(
        [cosLat * Math.cos(lon), sinLat, cosLat * Math.sin(lon)],
        slices,
        timeline,
      );
      const [px, py, pz] = project(rx, ry, rz);
      const depth = (pz + 1) / 2;
      dots.push({
        x: px,
        y: py,
        z: pz,
        r: ((opts.rBase ?? 0.6) + (opts.rDepth ?? 1.7) * depth + (isActive ? (opts.rActive ?? 0.3) : 0)) * scale,
        white: (opts.inkFar ?? 0.62) - (opts.inkSpan ?? 0.54) * depth - (isActive ? 0.14 : 0),
      });
    }
  }
  paintDots(ctx, dots, ink, opts.rMin);
}

/** listening — a waveform rolls through latitude rings. */
function drawWave(ctx: CanvasRenderingContext2D, size: number, t: number, ink: string, opts: ModeOpts) {
  const cx = size / 2;
  const cy = size / 2;
  const baseR = (size / 2) * 0.874;
  const project = makeProjector(t * 0.18, 0.38, cx, cy, 1);
  const scale = densityPower(size, opts.rsPow ?? 0.6);
  const ringCount = opts.rings ?? 15;
  const lonDensity = opts.lonDensity ?? 40;
  const dots: Dot[] = [];

  for (let ring = 0; ring <= ringCount; ring++) {
    const lat = -Math.PI / 2 + (ring / ringCount) * Math.PI;
    const radial = Math.cos(lat);
    const height = Math.sin(lat);
    const wave = 0.62 * Math.sin(t * 2.1 - ring * 0.52) + 0.38 * Math.sin(t * 1.27 + ring * 0.83);
    const ringRadius = baseR * (0.88 + 0.105 * wave);
    const lonCount = Math.max(1, Math.round(Math.abs(radial) * lonDensity));
    for (let lonIdx = 0; lonIdx < lonCount; lonIdx++) {
      const lon = (lonIdx / lonCount) * 2 * Math.PI;
      const [px, py, pz] = project(
        radial * Math.cos(lon) * ringRadius,
        height * ringRadius,
        radial * Math.sin(lon) * ringRadius,
      );
      const depth = (pz / baseR + 1) / 2;
      const waveBoost = Math.max(0, wave);
      dots.push({
        x: px,
        y: py,
        z: pz,
        r: ((opts.rBase ?? 0.6) + (opts.rDepth ?? 1.7) * depth) * (1 + 0.4 * waveBoost) * scale,
        white: 0.66 - 0.56 * depth - 0.1 * waveBoost,
      });
    }
  }
  paintDots(ctx, dots, ink, opts.rMin);
}

/** composing — an undulating multi-band sash. */
function drawRibbon(ctx: CanvasRenderingContext2D, size: number, t: number, ink: string, opts: ModeOpts) {
  const cx = size / 2;
  const cy = size / 2;
  const sphereR = (size / 2) * 0.78;
  const spin = opts.spin ?? 1;
  const project = makeProjector(t * 0.1 * spin, 0.3, cx, cy, 1);
  const scale = densityPower(size, opts.rsPow ?? 0.6);
  const dots: Dot[] = [];

  // Faint ghost sphere behind the ribbon bands.
  const ghostCount = opts.ghostN ?? 150;
  for (let i = 0; i < ghostCount; i++) {
    const [sx, sy, sz] = fibonacciSpherePoint(i, ghostCount);
    const [px, py, pz] = project(sx * sphereR, sy * sphereR, sz * sphereR);
    const depth = (pz / sphereR + 1) / 2;
    dots.push({ x: px, y: py, z: pz, r: 0.8 * scale, white: 0.78, a: 0.1 + 0.22 * depth });
  }

  // Ribbon orientation basis: axis1/axis2 span the band's ring plane (tumble
  // over time via `spin`), axis3 is the band's lateral (width) direction.
  // Upstream carries this through a symmetric 3-axis Euler construction with
  // one axis pinned to a literal 0; algebraically collapsed here (verified
  // orthonormal) rather than ported as permanently-dead branches.
  const h = t * 0.24 * spin;
  const roll = 0.55 + 0.3 * Math.sin(t * 0.18) * spin;
  const cosH = Math.cos(h);
  const sinH = Math.sin(h);
  const cosRoll = Math.cos(roll);
  const sinRoll = Math.sin(roll);
  const axis1: [number, number, number] = [cosH, 0, sinH];
  const axis2: [number, number, number] = [-sinH * sinRoll, cosRoll, cosH * sinRoll];
  const axis3: [number, number, number] = [-sinH * cosRoll, -sinRoll, cosH * cosRoll];

  const wobMul = opts.wobMul ?? 1;
  const lanes = opts.lanes ?? 5;
  const segs = opts.segs ?? 88;
  const bandCount = Math.max(1, Math.round(lanes * (opts.bandMul ?? 1)));

  for (let band = 0; band < bandCount; band++) {
    const laneOffset = (band - (bandCount - 1) / 2) * 0.075;
    const laneEdge = Math.abs(band - (bandCount - 1) / 2) / Math.max(1, (bandCount - 1) / 2);
    for (let seg = 0; seg < segs; seg++) {
      const z = (seg / segs) * 2 * Math.PI;
      const wobble =
        (0.16 * Math.sin(z * 3 - t * 1.7 + band * 0.22) + 0.07 * Math.sin(z * 5 + t * 1.1)) * wobMul;
      const lateral = laneOffset + wobble;
      const wx = axis1[0] * Math.cos(z) + axis2[0] * Math.sin(z) + axis3[0] * lateral;
      const wy = axis1[1] * Math.cos(z) + axis2[1] * Math.sin(z) + axis3[1] * lateral;
      const wz = axis1[2] * Math.cos(z) + axis2[2] * Math.sin(z) + axis3[2] * lateral;
      const mag = Math.sqrt(wx * wx + wy * wy + wz * wz) || 1e-6;
      const [px, py, pz] = project((wx / mag) * sphereR, (wy / mag) * sphereR, (wz / mag) * sphereR);
      const depth = (pz / sphereR + 1) / 2;
      dots.push({
        x: px,
        y: py,
        z: pz,
        r: ((opts.rBase ?? 1.1) + (opts.rDepth ?? 1.7) * depth) * (1 - 0.25 * laneEdge) * scale,
        white: 0.52 - 0.44 * depth + 0.18 * laneEdge,
        a: 0.4 + 0.6 * depth,
      });
    }
  }
  paintDots(ctx, dots, ink, opts.rMin);
}

// ── shaping (morph) helpers: flat 2D dotted-outline icon interpolation ────

function smoothstep01(x: number): number {
  return x * x * (3 - 2 * x);
}

type ShapeSampler = (t: number) => [number, number];

function circleSampler(t: number): [number, number] {
  const a = -Math.PI / 2 + t * 2 * Math.PI;
  return [Math.cos(a) * 0.24, Math.sin(a) * 0.24];
}

/** Arc-length-parameterized sampler around a closed polygon. */
function makePolygonSampler(points: ReadonlyArray<[number, number]>): ShapeSampler {
  const n = points.length;
  const segLens: number[] = [];
  let total = 0;
  for (let i = 0; i < n; i++) {
    const p = points[i];
    const q = points[(i + 1) % n];
    const len = Math.hypot(q[0] - p[0], q[1] - p[1]);
    segLens.push(len);
    total += len;
  }
  return (t: number) => {
    let target = t * total;
    let i = 0;
    while (target > segLens[i] && i < n - 1) {
      target -= segLens[i];
      i++;
    }
    const p = points[i];
    const q = points[(i + 1) % n];
    const localT = segLens[i] ? Math.min(1, target / segLens[i]) : 0;
    return [p[0] + (q[0] - p[0]) * localT, p[1] + (q[1] - p[1]) * localT];
  };
}

const TRIANGLE_SAMPLER = makePolygonSampler([
  [0, -0.26],
  [0.24, 0.16],
  [-0.24, 0.16],
]);
const SQUARE_SAMPLER = makePolygonSampler([
  [0, -0.2],
  [0.2, -0.2],
  [0.2, 0.2],
  [-0.2, 0.2],
  [-0.2, -0.2],
]);
const MORPH_SHAPES: ShapeSampler[] = [circleSampler, TRIANGLE_SAMPLER, SQUARE_SAMPLER];

const MORPH_HOLD_SECONDS = 1.4;
const MORPH_TRANSITION_SECONDS = 0.9;
const MORPH_CYCLE_SECONDS = MORPH_HOLD_SECONDS + MORPH_TRANSITION_SECONDS;
const MORPH_OUTLINE_SAMPLES = 160;

function morphDotCount(spread: number): number {
  return Math.max(6, Math.round(34 * spread));
}

/** shaping — dotted outline morphs circle → triangle → square. */
function drawMorph(ctx: CanvasRenderingContext2D, size: number, t: number, ink: string, opts: ModeOpts) {
  const shapeCount = MORPH_SHAPES.length;
  const cyclePos = t % (MORPH_CYCLE_SECONDS * shapeCount);
  const shapeIdx = Math.floor(cyclePos / MORPH_CYCLE_SECONDS);
  const withinCycle = cyclePos - shapeIdx * MORPH_CYCLE_SECONDS;
  const morphProgress =
    withinCycle > MORPH_HOLD_SECONDS
      ? smoothstep01((withinCycle - MORPH_HOLD_SECONDS) / MORPH_TRANSITION_SECONDS)
      : 0;
  const spread = opts.spread ?? 1;
  const shapeFrom = MORPH_SHAPES[shapeIdx];
  const shapeTo = MORPH_SHAPES[(shapeIdx + 1) % shapeCount];

  const outline: [number, number][] = [];
  for (let i = 0; i < MORPH_OUTLINE_SAMPLES; i++) {
    const u = i / MORPH_OUTLINE_SAMPLES;
    const from = shapeFrom(u);
    const to = shapeTo(u);
    outline.push([
      (from[0] + (to[0] - from[0]) * morphProgress) * spread,
      (from[1] + (to[1] - from[1]) * morphProgress) * spread,
    ]);
  }

  const segLens: number[] = [];
  let totalLen = 0;
  for (let i = 0; i < MORPH_OUTLINE_SAMPLES; i++) {
    const p = outline[i];
    const q = outline[(i + 1) % MORPH_OUTLINE_SAMPLES];
    const len = Math.hypot(q[0] - p[0], q[1] - p[1]);
    segLens.push(len);
    totalLen += len;
  }

  const dotCount = morphDotCount(opts.iconD ?? 1);
  const dotRadius = (opts.rDot ?? 0.021) * 1.35 * spread;
  const pulse = 1 + 0.02 * Math.sin(withinCycle * 3.1);
  const center = size / 2;
  const dots: Dot[] = [];
  let segIdx = 0;
  let walked = 0;
  for (let i = 0; i < dotCount; i++) {
    const target = (i / dotCount) * totalLen;
    while (walked + segLens[segIdx] < target && segIdx < MORPH_OUTLINE_SAMPLES - 1) {
      walked += segLens[segIdx];
      segIdx++;
    }
    const p = outline[segIdx];
    const q = outline[(segIdx + 1) % MORPH_OUTLINE_SAMPLES];
    const localT = segLens[segIdx] ? Math.min(1, (target - walked) / segLens[segIdx]) : 0;
    const x = (p[0] + (q[0] - p[0]) * localT) * pulse;
    const y = (p[1] + (q[1] - p[1]) * localT) * pulse;
    dots.push({
      x: center + x * size,
      y: center + y * size,
      z: 0,
      r: Math.max(0.35, dotRadius * size),
      white: 0.1,
    });
  }
  paintDots(ctx, dots, ink, opts.rMin);
}

// ─────────────────────────────────────────────────────────────────────────
// Preset resolution (base opts × per-size count/radius tuning)
// ─────────────────────────────────────────────────────────────────────────

type PortedMode = 'orbits' | 'globe' | 'rubik' | 'wave' | 'ribbon' | 'morph';

const STATE_TO_MODE: Record<ThinkingOrbState, PortedMode> = {
  working: 'orbits',
  searching: 'globe',
  solving: 'rubik',
  listening: 'wave',
  composing: 'ribbon',
  shaping: 'morph',
};

const MODE_DRAWS: Record<PortedMode, DrawFn> = {
  orbits: drawOrbits,
  globe: drawGlobe,
  rubik: drawRubik,
  wave: drawWave,
  ribbon: drawRibbon,
  morph: drawMorph,
};

const BASE_OPTS: Record<PortedMode, ModeOpts> = {
  orbits: { orbitN: 12, ghostN: 40, ghostR: 0.9, ghostA: 0.5, particles: 3, partR: 1.2, partRDepth: 1.6, rsPow: 0.6, rMin: 0.3 },
  globe: { latRings: 17, lonDensity: 44, rBase: 0.6, rDepth: 1.7, rBoost: 1, inkFar: 0.62, inkSpan: 0.54, rsPow: 0.6, rMin: 0.3 },
  rubik: { latRings: 15, lonDensity: 40, moveCount: 14, rBase: 0.6, rDepth: 1.7, rActive: 0.3, inkFar: 0.62, inkSpan: 0.54, rsPow: 0.6, rMin: 0.3 },
  wave: { rings: 15, lonDensity: 40, rBase: 0.6, rDepth: 1.7, rsPow: 0.6, rMin: 0.3 },
  ribbon: { lanes: 5, segs: 88, ghostN: 150, rBase: 1.1, rDepth: 1.7, rsPow: 0.6, rMin: 0.3 },
  morph: { rDot: 0.021, iconD: 1, rMin: 0.25 },
};

interface SizeTuning {
  speed: number;
  count: number;
  size: number;
  extra?: ModeOpts;
}

const SIZE_TUNING: Record<PortedMode, Record<ThinkingOrbSize, SizeTuning>> = {
  orbits: {
    64: { speed: 1.885, count: 1, size: 1 },
    20: { speed: 3.9, count: 0.238, size: 2.4 },
  },
  globe: {
    64: { speed: 2.015, count: 0.42, size: 1.15, extra: { scanMul: 4.08, dimBase: 0.45 } },
    20: { speed: 2.665, count: 0.105, size: 1.75, extra: { scanMul: 4.335, dimBase: 0.45 } },
  },
  rubik: {
    64: { speed: 1.82, count: 0.35, size: 1.05 },
    20: { speed: 1.95, count: 0.088, size: 1.9 },
  },
  wave: {
    64: { speed: 4.388, count: 0.341, size: 1 },
    20: { speed: 3.998, count: 0.105, size: 1.6 },
  },
  ribbon: {
    64: { speed: 2.34, count: 0.25, size: 0.85, extra: { spin: 0, bandMul: 3.9, wobMul: 1 } },
    20: { speed: 3.12, count: 0.051, size: 1.073, extra: { spin: 0, bandMul: 4.94, wobMul: 1 } },
  },
  morph: {
    64: { speed: 2.405, count: 0.702, size: 0.395, extra: { spread: 1.45 } },
    20: { speed: 2.08, count: 0.53, size: 1.011, extra: { spread: 1.45 } },
  },
};

// Density fields scaled together by sqrt(countMultiplier) — keeps dot
// density roughly proportional to surface area rather than linear count.
const PAIRED_DENSITY_FIELDS: [string, string][] = [
  ['latRings', 'lonDensity'],
  ['rings', 'lonDensity'],
  ['lanes', 'segs'],
];
const LINEAR_COUNT_FIELDS = ['orbitN', 'ghostN'];
const DIRECT_COUNT_FIELDS = ['iconD'];
const RADIUS_FIELDS = ['rBase', 'rDepth', 'rActive', 'rDot', 'ghostR', 'partR', 'partRDepth'];

function scaleCounts(opts: ModeOpts, countMul: number): ModeOpts {
  const result: ModeOpts = { ...opts };
  const touched = new Set<string>();
  const sqrtMul = Math.sqrt(countMul);
  for (const [a, b] of PAIRED_DENSITY_FIELDS) {
    const va = result[a];
    const vb = result[b];
    if (va != null && vb != null && !touched.has(a) && !touched.has(b)) {
      result[a] = Math.max(2, Math.round(va * sqrtMul));
      result[b] = Math.max(2, Math.round(vb * sqrtMul));
      touched.add(a);
      touched.add(b);
    }
  }
  for (const f of LINEAR_COUNT_FIELDS) {
    const v = result[f];
    if (v != null && v !== 0 && !touched.has(f)) result[f] = Math.max(1, Math.round(v * countMul));
  }
  for (const f of DIRECT_COUNT_FIELDS) {
    const v = result[f];
    if (v != null) result[f] = Math.max(0.02, v * countMul);
  }
  return result;
}

function scaleRadii(opts: ModeOpts, sizeMul: number): ModeOpts {
  const result: ModeOpts = { ...opts };
  for (const f of RADIUS_FIELDS) {
    const v = result[f];
    if (v != null) result[f] = v * sizeMul;
  }
  return result;
}

export interface ResolvedPreset {
  /** Frame painter for the resolved mode — the component just calls this. */
  draw: DrawFn;
  speed: number;
  opts: ModeOpts;
}

const presetCache = new Map<string, ResolvedPreset>();

/** Resolves a (state, size) pair to its draw function + fully-scaled options, memoized. */
export function resolvePreset(state: ThinkingOrbState, size: ThinkingOrbSize): ResolvedPreset {
  const key = `${state}-${size}`;
  const cached = presetCache.get(key);
  if (cached) return cached;
  const mode = STATE_TO_MODE[state];
  const tuning = SIZE_TUNING[mode][size];
  let opts: ModeOpts = { ...BASE_OPTS[mode] };
  if (tuning.count !== 1) opts = scaleCounts(opts, tuning.count);
  if (tuning.size !== 1) opts = scaleRadii(opts, tuning.size);
  if (tuning.extra) opts = { ...opts, ...tuning.extra };
  const resolved: ResolvedPreset = { draw: MODE_DRAWS[mode], speed: tuning.speed, opts };
  presetCache.set(key, resolved);
  return resolved;
}
