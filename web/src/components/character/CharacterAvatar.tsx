import { useId } from 'react';
import { cn } from '@/lib/utils';
import { characterFor } from '@/lib/character-gen';
import { StatusEmote, type StatusEmoteKind } from './StatusEmote';
import type { CharacterPose } from './poses';

/**
 * CharacterAvatar — the single visual identity of an AI staff member (§3.2).
 * One seed (agent id → `characterFor`) renders three ways: a small round-face
 * `avatar` for list rows, and a `bust` with shoulders + pose-driven arms for
 * cards / detail heros. Pure SVG + CSS, no dependency, theme-agnostic (the agent
 * gradient tokens read on both light and dark).
 *
 * Motion: eyes blink out of phase (delay from the generated blinkSeed) and the
 * working/waving poses add a micro-motion — all gated behind
 * `prefers-reduced-motion: no-preference` in index.css, so reduced-motion users
 * get the resting frame (eyes open, arms still).
 */

export type { CharacterPose } from './poses';

export interface CharacterAvatarProps {
  /** Agent id — the seed for tint + accessory + blink phase. */
  agentId: string;
  /** Human name for the accessible label; falls back to the id. */
  name?: string;
  /** Pixel box size. <64 renders the `avatar` face; ≥64 renders the `bust`. */
  size?: number;
  /** Force a variant regardless of size. */
  variant?: 'avatar' | 'bust';
  /** Whole-body attitude (bust reflects it fully; avatar only eyes/mouth). */
  pose?: CharacterPose;
  /** Head-top status bubble; omit for a calm face. */
  emote?: StatusEmoteKind | null;
  /** Show a live pulse dot at the corner (an agent mid-run). */
  live?: boolean;
  /** Disable blink / micro-motion even when motion is allowed. Default true. */
  animated?: boolean;
  className?: string;
}

interface Head {
  cx: number;
  cy: number;
  r: number;
}

const VIEW = 48;

function eyePositions(h: Head) {
  return {
    dx: h.r * 0.42,
    y: h.cy - h.r * 0.02,
    rx: h.r * 0.14,
    ry: h.r * 0.2,
  };
}

/** Mouth path for a pose (a small smile by default; open when celebrating). */
function Mouth({ h, pose }: { h: Head; pose: CharacterPose }) {
  const my = h.cy + h.r * 0.44;
  const w = h.r * 0.34;
  const ink = 'var(--character-ink)';
  if (pose === 'blocked') {
    // Flat, uncertain line.
    return <line x1={h.cx - w * 0.7} y1={my} x2={h.cx + w * 0.7} y2={my} stroke={ink} strokeWidth={h.r * 0.09} strokeLinecap="round" />;
  }
  if (pose === 'celebrating' || pose === 'waving') {
    // Open happy mouth.
    return <path d={`M ${h.cx - w} ${my - h.r * 0.05} Q ${h.cx} ${my + h.r * 0.4} ${h.cx + w} ${my - h.r * 0.05} Z`} fill={ink} />;
  }
  // Gentle smile.
  return (
    <path
      d={`M ${h.cx - w} ${my} Q ${h.cx} ${my + h.r * 0.3} ${h.cx + w} ${my}`}
      fill="none"
      stroke={ink}
      strokeWidth={h.r * 0.09}
      strokeLinecap="round"
    />
  );
}

function Eyes({ h, pose, animated, blinkSeedMs }: { h: Head; pose: CharacterPose; animated: boolean; blinkSeedMs: number }) {
  const e = eyePositions(h);
  const ink = 'var(--character-ink)';
  if (pose === 'sleeping') {
    // Closed, content arcs — no blink.
    const arc = (cx: number) => (
      <path
        key={cx}
        d={`M ${cx - e.rx * 1.3} ${e.y} Q ${cx} ${e.y + e.ry * 0.8} ${cx + e.rx * 1.3} ${e.y}`}
        fill="none"
        stroke={ink}
        strokeWidth={h.r * 0.08}
        strokeLinecap="round"
      />
    );
    return (
      <g>
        {arc(h.cx - e.dx)}
        {arc(h.cx + e.dx)}
      </g>
    );
  }
  return (
    <g
      className={animated ? 'character-eyes' : undefined}
      style={animated ? { animationDelay: `${blinkSeedMs}ms` } : undefined}
    >
      <ellipse cx={h.cx - e.dx} cy={e.y} rx={e.rx} ry={e.ry} fill={ink} />
      <ellipse cx={h.cx + e.dx} cy={e.y} rx={e.rx} ry={e.ry} fill={ink} />
      {/* Catchlights for life. */}
      <circle cx={h.cx - e.dx + e.rx * 0.4} cy={e.y - e.ry * 0.4} r={e.rx * 0.35} fill="var(--character-bubble)" />
      <circle cx={h.cx + e.dx + e.rx * 0.4} cy={e.y - e.ry * 0.4} r={e.rx * 0.35} fill="var(--character-bubble)" />
    </g>
  );
}

function Cheeks({ h }: { h: Head }) {
  const cy = h.cy + h.r * 0.28;
  const dx = h.r * 0.66;
  const rr = h.r * 0.13;
  return (
    <g opacity={0.5}>
      <circle cx={h.cx - dx} cy={cy} r={rr} fill="var(--agent-2b)" opacity={0.35} />
      <circle cx={h.cx + dx} cy={cy} r={rr} fill="var(--agent-2b)" opacity={0.35} />
    </g>
  );
}

function Accessory({ h, kind, gradId }: { h: Head; kind: string; gradId: string }) {
  const ink = 'var(--character-ink)';
  const topY = h.cy - h.r;
  const grad = `url(#${gradId})`;
  switch (kind) {
    case 'antenna':
      return (
        <g>
          <line x1={h.cx} y1={topY + h.r * 0.1} x2={h.cx} y2={topY - h.r * 0.5} stroke={ink} strokeWidth={h.r * 0.07} strokeLinecap="round" />
          <circle cx={h.cx} cy={topY - h.r * 0.55} r={h.r * 0.16} fill="var(--xp)" />
        </g>
      );
    case 'bow':
      return (
        <g transform={`translate(${h.cx + h.r * 0.5} ${topY + h.r * 0.12})`}>
          <path d={`M 0 0 L ${-h.r * 0.32} ${-h.r * 0.2} L ${-h.r * 0.32} ${h.r * 0.2} Z`} fill="var(--xp)" />
          <path d={`M 0 0 L ${h.r * 0.32} ${-h.r * 0.2} L ${h.r * 0.32} ${h.r * 0.2} Z`} fill="var(--xp)" />
          <circle cx={0} cy={0} r={h.r * 0.1} fill="var(--coin)" />
        </g>
      );
    case 'glasses': {
      const e = eyePositions(h);
      return (
        <g fill="none" stroke={ink} strokeWidth={h.r * 0.06}>
          <circle cx={h.cx - e.dx} cy={e.y} r={e.rx * 1.9} />
          <circle cx={h.cx + e.dx} cy={e.y} r={e.rx * 1.9} />
          <line x1={h.cx - e.dx + e.rx * 1.9} y1={e.y} x2={h.cx + e.dx - e.rx * 1.9} y2={e.y} />
        </g>
      );
    }
    case 'cap':
      return (
        <g>
          <path d={`M ${h.cx - h.r * 0.9} ${topY + h.r * 0.28} A ${h.r * 0.9} ${h.r * 0.9} 0 0 1 ${h.cx + h.r * 0.9} ${topY + h.r * 0.28} Z`} fill={grad} />
          <rect x={h.cx - h.r * 0.95} y={topY + h.r * 0.22} width={h.r * 1.9} height={h.r * 0.14} rx={h.r * 0.07} fill={grad} />
        </g>
      );
    case 'scarf': {
      const sy = h.cy + h.r * 0.82;
      return <rect x={h.cx - h.r * 0.72} y={sy} width={h.r * 1.44} height={h.r * 0.34} rx={h.r * 0.17} fill="var(--agent-2b)" />;
    }
    case 'flower':
      return (
        <g transform={`translate(${h.cx + h.r * 0.55} ${topY + h.r * 0.15})`}>
          {[0, 72, 144, 216, 288].map((deg) => {
            const rad = (deg * Math.PI) / 180;
            return <circle key={deg} cx={Math.cos(rad) * h.r * 0.16} cy={Math.sin(rad) * h.r * 0.16} r={h.r * 0.11} fill="var(--agent-6a)" />;
          })}
          <circle cx={0} cy={0} r={h.r * 0.1} fill="var(--xp)" />
        </g>
      );
    default:
      return null;
  }
}

/** Bust body + pose-driven arms. Rendered under the head so the head overlaps.
 *  Geometry is in fixed viewBox units (the head sits at a fixed bust position). */
function BustBody({ pose, gradId, animated }: { pose: CharacterPose; gradId: string; animated: boolean }) {
  const grad = `url(#${gradId})`;
  const armColor = 'var(--character-ink-soft)';
  const armW = 2.6;
  // Shoulder anchors on the mound.
  const lS = { x: 13, y: 35 };
  const rS = { x: 35, y: 35 };

  type Arm = { s: { x: number; y: number }; hnd: { x: number; y: number }; wave?: boolean };
  let arms: Arm[];
  switch (pose) {
    case 'working':
      arms = [
        { s: lS, hnd: { x: 19, y: 43 } },
        { s: rS, hnd: { x: 29, y: 43 } },
      ];
      break;
    case 'blocked':
      arms = [
        { s: lS, hnd: { x: 11, y: 44 } },
        { s: rS, hnd: { x: 40, y: 27 } },
      ];
      break;
    case 'waving':
      arms = [
        { s: lS, hnd: { x: 11, y: 44 } },
        { s: rS, hnd: { x: 41, y: 25 }, wave: true },
      ];
      break;
    case 'celebrating':
      arms = [
        { s: lS, hnd: { x: 9, y: 26 } },
        { s: rS, hnd: { x: 39, y: 26 } },
      ];
      break;
    default: // idle / sleeping
      arms = [
        { s: lS, hnd: { x: 10, y: 45 } },
        { s: rS, hnd: { x: 38, y: 45 } },
      ];
  }

  return (
    <g>
      {/* Shoulders / body mound */}
      <path d="M6 48 Q6 31 24 31 Q42 31 42 48 Z" fill={grad} />
      {/* Arms + paw hands */}
      {arms.map((a, i) => (
        <g
          key={i}
          className={a.wave && animated ? 'character-arm-wave' : undefined}
          style={a.wave && animated ? { transformOrigin: `${a.s.x}px ${a.s.y}px` } : undefined}
        >
          <line x1={a.s.x} y1={a.s.y} x2={a.hnd.x} y2={a.hnd.y} stroke={armColor} strokeWidth={armW} strokeLinecap="round" opacity={0.85} />
          <circle cx={a.hnd.x} cy={a.hnd.y} r={2.4} fill="var(--character-ink-soft)" />
        </g>
      ))}
    </g>
  );
}

export function CharacterAvatar({
  agentId,
  name,
  size = 32,
  variant,
  pose = 'idle',
  emote,
  live = false,
  animated = true,
  className,
}: CharacterAvatarProps) {
  const uid = useId().replace(/[:]/g, '');
  const gradId = `char-grad-${uid}`;
  const traits = characterFor(agentId);
  const isBust = variant ? variant === 'bust' : size >= 64;
  const label = name ?? agentId;

  const head: Head = isBust ? { cx: 24, cy: 17, r: 12.5 } : { cx: 24, cy: 24, r: 18 };
  const aVar = `var(--agent-${traits.tintIndex}a)`;
  const bVar = `var(--agent-${traits.tintIndex}b)`;
  const canBlink = animated && pose !== 'sleeping';

  return (
    <span
      role="img"
      aria-label={label}
      className={cn('relative inline-block align-middle', className)}
      style={{ width: size, height: size }}
    >
      <svg viewBox={`0 0 ${VIEW} ${VIEW}`} width={size} height={size} aria-hidden="true" className="overflow-visible">
        <defs>
          <linearGradient id={gradId} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0" stopColor={aVar} />
            <stop offset="1" stopColor={bVar} />
          </linearGradient>
        </defs>

        {isBust && <BustBody pose={pose} gradId={gradId} animated={animated} />}

        {/* Head */}
        <circle
          cx={head.cx}
          cy={head.cy}
          r={head.r}
          fill={`url(#${gradId})`}
          stroke="var(--character-ink-soft)"
          strokeOpacity={0.18}
          strokeWidth={0.6}
        />
        <Cheeks h={head} />
        <Eyes h={head} pose={pose} animated={canBlink} blinkSeedMs={traits.blinkSeedMs} />
        <Mouth h={head} pose={pose} />
        <Accessory h={head} kind={traits.accessory} gradId={gradId} />
      </svg>

      {emote && (
        <span className="absolute -right-1 -top-1">
          <StatusEmote kind={emote} size={Math.max(14, Math.round(size * 0.5))} />
        </span>
      )}

      {live && (
        <span className="absolute bottom-0 right-0 flex h-2.5 w-2.5" aria-hidden="true">
          <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400 opacity-70" />
          <span className="relative inline-flex h-2.5 w-2.5 rounded-full border border-white bg-emerald-500 dark:border-stone-900" />
        </span>
      )}
    </span>
  );
}
