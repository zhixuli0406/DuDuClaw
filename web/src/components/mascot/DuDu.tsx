import { useId } from 'react';
import { cn } from '@/lib/utils';
import { presetFor, restMouthPath, type ArmPose, type DuduFace } from './faces';
import { useDuduClock } from './useDuduClock';
import { visemePath, VISEMES, type VisemeShape } from './visemes';

/**
 * DuDu — the DuDuClaw mascot (§7). A rounded amber paw-creature (the 🐾 brand
 * made flesh): amber body with a warm stone outline, stubby arms + legs, and a
 * little paw print on its chest. Pure SVG React — no Rive/Lottie/GIF dependency,
 * so the whole character is version-controllable and diff-able.
 *
 * One `face` prop selects a preset (eyes + brows + mouth + arm pose). Blink is
 * driven by a RAF clock (slower while `thinking`); `speaking` drives the mouth
 * from the `viseme` prop instead (callers feed the rhythm). Everything freezes
 * to a static resting frame under `prefers-reduced-motion` (via `useDuduClock`).
 */

export type DuduSize = 'sm' | 'md' | 'lg' | number;

export interface DuDuProps {
  /** Which expression to wear. Defaults to `idle`. */
  face?: DuduFace;
  /** Active mouth shape while `speaking`; ignored otherwise. */
  viseme?: VisemeShape;
  /** `sm` (48) / `md` (96) / `lg` (160), or an explicit pixel size. */
  size?: DuduSize;
  /** Unique-ish id prefix for the gradient defs (auto-generated otherwise). */
  idPrefix?: string;
  /** Drive blink / bob / wave. Default true; forced off under reduced-motion. */
  animated?: boolean;
  /** Accessible label; defaults to a generic "DuDu, <face>". */
  label?: string;
  className?: string;
}

const SIZE_MAP: Record<Exclude<DuduSize, number>, number> = { sm: 48, md: 96, lg: 160 };
const VIEW = 100;

interface Arm {
  hnd: { x: number; y: number };
  wave?: boolean;
}

const L_SHOULDER = { x: 24, y: 53 };
const R_SHOULDER = { x: 76, y: 53 };

/** Hand endpoints for each arm attitude (left, right). */
function armsFor(pose: ArmPose): [Arm, Arm] {
  switch (pose) {
    case 'wave':
      return [{ hnd: { x: 19, y: 67 } }, { hnd: { x: 88, y: 30 }, wave: true }];
    case 'cheer':
      return [{ hnd: { x: 14, y: 31 } }, { hnd: { x: 86, y: 31 } }];
    case 'think':
      return [{ hnd: { x: 19, y: 67 } }, { hnd: { x: 58, y: 57 } }];
    case 'work':
      return [{ hnd: { x: 38, y: 71 } }, { hnd: { x: 62, y: 71 } }];
    case 'read':
      return [{ hnd: { x: 40, y: 64 } }, { hnd: { x: 60, y: 64 } }];
    default: // rest
      return [{ hnd: { x: 18, y: 67 } }, { hnd: { x: 82, y: 67 } }];
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
  const bob = Math.sin(t * Math.PI * 1.2) * 1.8;

  // Blink ~0.18s; slower while thinking so the squint reads as a held pose.
  const blinkMs = face === 'thinking' ? 4200 : 2600;
  const tMs = t * 1000;
  const inBlink = t > 0 && (tMs + blinkMs / 2) % blinkMs < 180;
  const blinkScale = inBlink ? 0.12 : 1;

  const waveDeg = animated ? Math.sin(t * Math.PI * 2.4) * 14 : 0;

  const id = (k: string) => `${prefix}-${k}`;
  const bodyFill = `url(#${id('body')})`;
  const stroke = 'var(--character-ink-soft)';
  const ink = 'var(--character-ink)';
  const cream = 'var(--character-bubble)';
  const blush = 'var(--agent-2b)';

  const [lArm, rArm] = armsFor(preset.arm);

  // Eye geometry.
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
        aria-hidden="true"
        className="overflow-visible"
      >
        <defs>
          <linearGradient id={id('body')} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0" stopColor="var(--agent-1a)" />
            <stop offset="1" stopColor="var(--agent-1b)" />
          </linearGradient>
        </defs>

        {/* Ground shadow — squashes as the body bobs up. */}
        <ellipse cx={50} cy={95} rx={26 - bob * 1.2} ry={3.6} fill={ink} opacity={0.1} />

        <g transform={`translate(0, ${bob})`}>
          {/* Ears (tucked behind the body). */}
          {[
            { cx: 30, cy: 20, rot: -18 },
            { cx: 70, cy: 20, rot: 18 },
          ].map((ear) => (
            <g key={ear.cx} transform={`rotate(${ear.rot} ${ear.cx} ${ear.cy})`}>
              <ellipse cx={ear.cx} cy={ear.cy} rx={8.5} ry={11.5} fill={bodyFill} stroke={stroke} strokeWidth={1.4} />
              <ellipse cx={ear.cx} cy={ear.cy + 1} rx={4} ry={6} fill={blush} opacity={0.5} />
            </g>
          ))}

          {/* Legs (behind the body). */}
          {[40, 60].map((cx) => (
            <ellipse key={cx} cx={cx} cy={87} rx={8} ry={7.5} fill={bodyFill} stroke={stroke} strokeWidth={1.4} />
          ))}

          {/* Body. */}
          <ellipse cx={50} cy={53} rx={34} ry={36} fill={bodyFill} stroke={stroke} strokeWidth={1.6} />

          {/* Arms + paw hands, rendered over the body edge. */}
          {([
            { s: L_SHOULDER, a: lArm },
            { s: R_SHOULDER, a: rArm },
          ] as const).map(({ s, a }, i) => (
            <g
              key={i}
              transform={a.wave ? `rotate(${waveDeg} ${s.x} ${s.y})` : undefined}
              style={a.wave ? { transformOrigin: `${s.x}px ${s.y}px` } : undefined}
            >
              <line
                x1={s.x}
                y1={s.y}
                x2={a.hnd.x}
                y2={a.hnd.y}
                stroke={bodyFill}
                strokeWidth={7}
                strokeLinecap="round"
              />
              <circle cx={a.hnd.x} cy={a.hnd.y} r={4.6} fill={bodyFill} stroke={stroke} strokeWidth={1.2} />
            </g>
          ))}

          {/* Book prop for the reading pose. */}
          {preset.arm === 'read' && (
            <g>
              <rect x={41} y={60} width={18} height={12} rx={2} fill={cream} stroke={stroke} strokeWidth={1.2} />
              <line x1={50} y1={61} x2={50} y2={71} stroke={stroke} strokeWidth={1} opacity={0.6} />
            </g>
          )}

          {/* Belly patch + chest paw print. */}
          <ellipse cx={50} cy={64} rx={17} ry={19} fill={cream} opacity={0.9} />
          {preset.arm !== 'read' && preset.arm !== 'work' && (
            <g fill={stroke} opacity={0.4}>
              <ellipse cx={50} cy={70} rx={6.5} ry={5.5} />
              <circle cx={44} cy={63} r={2.4} />
              <circle cx={50} cy={61} r={2.6} />
              <circle cx={56} cy={63} r={2.4} />
            </g>
          )}

          {/* Cheeks. */}
          <ellipse cx={31} cy={53} rx={5} ry={3} fill={blush} opacity={0.85 * preset.blushOpacity} />
          <ellipse cx={69} cy={53} rx={5} ry={3} fill={blush} opacity={0.85 * preset.blushOpacity} />

          {/* Brows. */}
          {preset.showBrows && (
            <g fill={ink} data-face-brows={face}>
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

          {/* Eyes. */}
          <g>
            <ellipse cx={50 - eyeDx} cy={eyeY} rx={eyeRx} ry={eyeRy} fill={ink} />
            <ellipse cx={50 + eyeDx} cy={eyeY} rx={eyeRx} ry={eyeRy} fill={ink} />
            {!inBlink && face !== 'sleep' && (
              <>
                <circle cx={50 - eyeDx + 1.4} cy={eyeY - 1.6} r={1.3} fill={cream} />
                <circle cx={50 + eyeDx + 1.4} cy={eyeY - 1.6} r={1.3} fill={cream} />
              </>
            )}
          </g>

          {/* Mouth — viseme-driven while speaking, otherwise the rest shape. */}
          <path
            d={face === 'speaking' ? visemePath(viseme ?? VISEMES.REST) : restMouthPath(face)}
            fill={ink}
            data-face={face}
          />
        </g>
      </svg>
    </span>
  );
}
