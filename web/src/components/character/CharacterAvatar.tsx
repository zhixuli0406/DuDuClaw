import { useId } from 'react';
import { cn } from '@/lib/utils';
import { characterFor } from '@/lib/character-gen';
import { defaultOutfitFor, effectiveTint, type AgentOutfit } from '@/lib/outfit';
import { useAgentAvatar } from '@/stores/agent-avatar-store';
import { useAgentsStore } from '@/stores/agents-store';
import { StatusEmote, type StatusEmoteKind } from './StatusEmote';
import { HeadOutfit, BustOutfit } from './OutfitLayers';
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
  /**
   * Uploaded avatar image as a data URI (WP4). When provided (a non-empty
   * string) it replaces the generative character. `null`/`''` explicitly means
   * "no upload, use the generative face" and skips the lazy avatar lookup. When
   * omitted (`undefined`) the component resolves any uploaded avatar for
   * `agentId` from the shared cache, so every surface stays consistent.
   */
  avatar?: string | null;
  /**
   * Wardrobe composition. `undefined` resolves the saved outfit from the
   * agents store; `null` explicitly means "no saved outfit" (seeded default).
   * A dressed agent always renders the generative character — a saved outfit
   * outranks an uploaded photo.
   */
  outfit?: AgentOutfit | null;
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
  avatar,
  outfit,
  className,
}: CharacterAvatarProps) {
  const uid = useId().replace(/[:]/g, '');
  const gradId = `char-grad-${uid}`;
  const traits = characterFor(agentId);
  const isBust = variant ? variant === 'bust' : size >= 64;
  const label = name ?? agentId;

  // Resolve the saved outfit. An explicit prop wins (even null = "seeded");
  // when omitted, the agents store (if it has this agent loaded) supplies it.
  const storeOutfit = useAgentsStore((s) =>
    outfit === undefined ? s.agents.find((a) => a.name === agentId)?.outfit ?? null : null,
  );
  const savedOutfit = outfit !== undefined ? outfit : storeOutfit;
  const fit = savedOutfit ?? defaultOutfitFor(agentId);

  // Resolve an uploaded avatar. An explicit prop wins (even null = "generative");
  // when omitted we consult the shared cache so any surface picks it up.
  // A DRESSED agent always shows its character — the wardrobe replaces photos.
  const resolved = useAgentAvatar(avatar === undefined && !savedOutfit ? agentId : undefined);
  const uploadedSrc = savedOutfit
    ? undefined
    : avatar !== undefined
      ? avatar || undefined
      : resolved;

  const head: Head = isBust ? { cx: 24, cy: 17, r: 12.5 } : { cx: 24, cy: 24, r: 18 };
  const tintIndex = effectiveTint(agentId, savedOutfit);
  const aVar = `var(--agent-${tintIndex}a)`;
  const bVar = `var(--agent-${tintIndex}b)`;
  const canBlink = animated && pose !== 'sleeping';

  return (
    <span
      role="img"
      aria-label={label}
      className={cn('relative inline-block align-middle', className)}
      style={{ width: size, height: size }}
    >
      {uploadedSrc ? (
        <img
          src={uploadedSrc}
          alt=""
          aria-hidden="true"
          className="h-full w-full rounded-full object-cover ring-1 ring-inset ring-black/5 dark:ring-white/10"
        />
      ) : (
      <svg viewBox={`0 0 ${VIEW} ${VIEW}`} width={size} height={size} aria-hidden="true" className="overflow-visible">
        <defs>
          <linearGradient id={gradId} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0" stopColor={aVar} />
            <stop offset="1" stopColor={bVar} />
          </linearGradient>
        </defs>

        {isBust && <BustBody pose={pose} gradId={gradId} animated={animated} />}
        {isBust && <BustOutfit fit={fit} />}

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
        <HeadOutfit h={head} fit={fit} gradId={gradId} />
      </svg>
      )}

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
