import { useId } from 'react';
import { cn } from '@/lib/utils';
import {
  MARK_ACCENT_PATH,
  MARK_INK_BODY_PATH,
  MARK_INK_HEAD_PATH,
  MARK_LANDMARKS,
  MARK_POTRACE_TRANSFORM,
} from '@/components/brand/mark-paths';
import { presetFor, restMouthPath, type ArmPose, type DuduFace } from './faces';
import { useDuduClock } from './useDuduClock';
import { visemePath, VISEMES, type VisemeShape } from './visemes';

/**
 * DuDu — the DuDuClaw mascot (§7), drawn on the C1 brand mark: the poised
 * sitting cat (ink layer) with its amber tail (accent layer). The silhouette is
 * the mark's own path data; the mascot only adds a small face (eyes, brows,
 * mouth, cheeks) and, for some presets, a front paw or a prop. Pure SVG React,
 * no Rive/Lottie/GIF dependency.
 *
 * Colours follow the logo rules: the body uses `--logo-ink` (Navy on light,
 * Cream on dark), the tail `--logo-accent` (Amber, never recoloured), and the
 * face `--logo-face` (the inverse of the ink, so it always reads on the body).
 *
 * One `face` prop selects a preset (eyes + brows + mouth + paw pose). Blink and
 * tail sway run on a RAF clock (slower while `thinking`); `speaking` drives the
 * mouth from the `viseme` prop. Everything freezes to a static resting frame
 * under `prefers-reduced-motion` (via `useDuduClock`).
 */

export type DuduSize = 'sm' | 'md' | 'lg' | number;

export interface DuDuProps {
  /** Which expression to wear. Defaults to `idle`. */
  face?: DuduFace;
  /** Active mouth shape while `speaking`; ignored otherwise. */
  viseme?: VisemeShape;
  /** `sm` (48) / `md` (96) / `lg` (160), or an explicit pixel size. */
  size?: DuduSize;
  /** Unique-ish id prefix for SVG defs (auto-generated otherwise). */
  idPrefix?: string;
  /** Drive blink / bob / tail / wave. Default true; forced off under reduced-motion. */
  animated?: boolean;
  /** Accessible label; defaults to a generic "DuDu, <face>". */
  label?: string;
  className?: string;
}

const SIZE_MAP: Record<Exclude<DuduSize, number>, number> = { sm: 48, md: 96, lg: 160 };
const VIEW = 100;

// ── Master-space layout ─────────────────────────────────────────────────────
// Everything below is in the mark's 1254 px master space (y down). One group
// transform fits the mark's bounding box into the 100-unit view, leaving room
// at the bottom for the ground shadow.
const { bbox, tailBase } = MARK_LANDMARKS;
const FIT = 88 / (bbox.y1 - bbox.y0);
const FIT_CX = (bbox.x0 + bbox.x1) / 2;
const FIT_TRANSFORM = `translate(50 4) scale(${FIT}) translate(${-FIT_CX} ${-bbox.y0})`;

/** Face-space (the original 100-unit face grid, eyes at y=46) → head. The
 *  three-quarter head faces right; the eye pair sits mid-head. */
const EYES_TRANSFORM = 'translate(735 395) scale(7.5) translate(-50 -46)';
/** Mouth sits under the snout, a little smaller than the eye grid. */
const MOUTH_TRANSFORM = 'translate(815 462) scale(6.5) translate(-50 -59)';

/** Front paw anchor on the chest, in master space. */
const SHOULDER = { x: 700, y: 880 };
const PAW_WIDTH = 104;
const PAW_R = 64;

interface Paw {
  hnd: { x: number; y: number };
  wave?: boolean;
}

/** Paw endpoint for each arm attitude, or `null` when the paw stays tucked. */
function pawFor(pose: ArmPose): Paw | null {
  switch (pose) {
    case 'wave':
      return { hnd: { x: 900, y: 660 }, wave: true };
    case 'cheer':
      return { hnd: { x: 930, y: 590 } };
    case 'think':
      return { hnd: { x: 830, y: 560 } };
    case 'work':
      return { hnd: { x: 900, y: 985 } };
    case 'read':
      return { hnd: { x: 830, y: 880 } };
    default: // rest — sitting upright, paws tucked like the mark
      return null;
  }
}

/** Tail sway amplitude (degrees) per face: livelier when pleased. */
function tailAmplitude(face: DuduFace): number {
  switch (face) {
    case 'sleep':
      return 0;
    case 'happy':
    case 'proud':
    case 'celebrating':
    case 'waving':
      return 6;
    case 'concerned':
      return 1.5;
    default:
      return 3;
  }
}

export function DuDu({
  face = 'idle',
  viseme,
  size = 'md',
  idPrefix,
  animated = true,
  label,
  className,
}: DuDuProps) {
  const autoId = useId().replace(/[:]/g, '');
  const prefix = idPrefix ?? `dudu-${autoId}`;
  const px = typeof size === 'number' ? size : SIZE_MAP[size];
  const preset = presetFor(face);
  const t = useDuduClock(animated);

  // Gentle whole-body bob (subtle in a 100-unit box).
  const bob = Math.sin(t * Math.PI * 1.2) * 1.4;

  // Blink ~0.18s; slower while thinking so the squint reads as a held pose.
  const blinkMs = face === 'thinking' ? 4200 : 2600;
  const tMs = t * 1000;
  const inBlink = t > 0 && (tMs + blinkMs / 2) % blinkMs < 180;
  const blinkScale = inBlink ? 0.12 : 1;

  const waveDeg = animated ? Math.sin(t * Math.PI * 2.4) * 10 : 0;
  const tailSpeed = face === 'thinking' ? 0.5 : 0.9;
  const tailDeg = Math.sin(t * Math.PI * tailSpeed) * tailAmplitude(face);

  const ink = 'var(--logo-ink)';
  const accent = 'var(--logo-accent)';
  const faceInk = 'var(--logo-face)';

  const paw = pawFor(preset.arm);

  // Eye geometry (face-space units, scaled onto the head by EYES_TRANSFORM).
  const eyeY = 46;
  const eyeDx = 10;
  const eyeRx = 4.2 * preset.eyeScaleX;
  const eyeRy = 5.2 * preset.eyeScaleY * blinkScale;

  return (
    <span
      role="img"
      aria-label={label ?? `DuDu, ${face}`}
      className={cn('relative inline-block align-middle', className)}
      style={{ width: px, height: px }}
    >
      <svg
        viewBox={`0 0 ${VIEW} ${VIEW}`}
        width={px}
        height={px}
        data-face={face}
        data-dudu={prefix}
        aria-hidden="true"
        className="overflow-visible"
      >
        {/* Ground shadow — squashes as the body bobs up. */}
        {/* Ground shadow stays black: the ink flips to Cream in dark mode, which would read as a glow. */}
        <ellipse cx={50} cy={95} rx={30 - bob * 1.2} ry={3} fill="#000" opacity={0.12} />

        <g transform={`translate(0, ${bob})`}>
          <g transform={FIT_TRANSFORM}>
            {/* Tail (accent layer), swaying from its base. */}
            <g transform={`rotate(${tailDeg} ${tailBase.x} ${tailBase.y})`}>
              <g transform={MARK_POTRACE_TRANSFORM} style={{ fill: accent }}>
                <path d={MARK_ACCENT_PATH} />
              </g>
            </g>

            {/* Body (ink layer): back + haunch, then head + chest. */}
            <g transform={MARK_POTRACE_TRANSFORM} style={{ fill: ink }}>
              <path d={MARK_INK_BODY_PATH} />
              <path d={MARK_INK_HEAD_PATH} />
            </g>

            {/* Props sit in front of the body, under the paw. */}
            {preset.arm === 'read' && (
              <g>
                <rect x={770} y={790} width={270} height={190} rx={20} fill={faceInk} stroke={ink} strokeWidth={16} />
                <line x1={905} y1={804} x2={905} y2={966} stroke={ink} strokeWidth={12} opacity={0.6} />
              </g>
            )}
            {preset.arm === 'work' && (
              <rect x={820} y={1030} width={300} height={56} rx={16} fill={faceInk} stroke={ink} strokeWidth={16} />
            )}

            {/* Front paw: an ink capsule outlined in the face colour so it
                separates from the body it overlaps. */}
            {paw && (
              <g
                transform={paw.wave ? `rotate(${waveDeg} ${SHOULDER.x} ${SHOULDER.y})` : undefined}
                data-paw={preset.arm}
              >
                <line
                  x1={SHOULDER.x}
                  y1={SHOULDER.y}
                  x2={paw.hnd.x}
                  y2={paw.hnd.y}
                  stroke={faceInk}
                  strokeWidth={PAW_WIDTH + 28}
                  strokeLinecap="round"
                />
                <line
                  x1={SHOULDER.x}
                  y1={SHOULDER.y}
                  x2={paw.hnd.x}
                  y2={paw.hnd.y}
                  stroke={ink}
                  strokeWidth={PAW_WIDTH}
                  strokeLinecap="round"
                />
                <circle cx={paw.hnd.x} cy={paw.hnd.y} r={PAW_R} fill={ink} stroke={faceInk} strokeWidth={14} />
                {/* Toe beans hint. */}
                <g fill={faceInk} opacity={0.8}>
                  <circle cx={paw.hnd.x - 26} cy={paw.hnd.y - 18} r={11} />
                  <circle cx={paw.hnd.x} cy={paw.hnd.y - 30} r={11} />
                  <circle cx={paw.hnd.x + 26} cy={paw.hnd.y - 18} r={11} />
                </g>
              </g>
            )}

            {/* Face, drawn on the original 100-unit face grid. */}
            <g transform={EYES_TRANSFORM}>
              {/* Near cheek (amber accent); the far cheek hides behind the snout. */}
              <ellipse cx={35} cy={54} rx={4.2} ry={2.4} fill={accent} opacity={0.6 * preset.blushOpacity} />

              {/* Brows. */}
              {preset.showBrows && (
                <g fill={faceInk} data-face-brows={face}>
                  <rect
                    x={34}
                    y={37 + preset.browDy}
                    width={9}
                    height={1.8}
                    rx={0.9}
                    transform={`rotate(${-preset.browTilt} 38.5 ${38 + preset.browDy})`}
                  />
                  <rect
                    x={57}
                    y={37 + preset.browDy}
                    width={9}
                    height={1.8}
                    rx={0.9}
                    transform={`rotate(${preset.browTilt} 61.5 ${38 + preset.browDy})`}
                  />
                </g>
              )}

              {/* Eyes: face-colour almonds with ink pupils glancing forward. */}
              <g>
                <ellipse cx={50 - eyeDx} cy={eyeY} rx={eyeRx} ry={eyeRy} fill={faceInk} />
                <ellipse cx={50 + eyeDx} cy={eyeY} rx={eyeRx} ry={eyeRy} fill={faceInk} />
                {!inBlink && face !== 'sleep' && (
                  <>
                    <circle cx={50 - eyeDx + 1.4} cy={eyeY + 0.2} r={1.3} fill={ink} />
                    <circle cx={50 + eyeDx + 1.4} cy={eyeY + 0.2} r={1.3} fill={ink} />
                  </>
                )}
              </g>
            </g>

            {/* Mouth — viseme-driven while speaking, otherwise the rest shape. */}
            <g transform={MOUTH_TRANSFORM}>
              <path
                d={face === 'speaking' ? visemePath(viseme ?? VISEMES.REST) : restMouthPath(face)}
                fill={faceInk}
                data-face={face}
              />
            </g>
          </g>
        </g>
      </svg>
    </span>
  );
}
