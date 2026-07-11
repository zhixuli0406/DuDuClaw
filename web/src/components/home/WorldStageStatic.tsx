import { useState } from 'react';
import { useIntl } from 'react-intl';
import { CharacterAvatar, agentPose } from '@/components/ui';
import { getScene, readSceneKey } from '@/components/world/rooms';
import type { WorldStageAgent } from './WorldStagePlaceholder';

/**
 * WorldStageStatic — the reduced-motion / list-mode / WebGL-unavailable fallback
 * for the Home world band (V8-T8.5). A warm, static illustrated scene rendered in
 * pure SVG (no canvas), driven by the SAME agent data as the 3D stage so the
 * two never disagree. Busts are real `CharacterAvatar`s posed from each agent's
 * lifecycle status; all colours come from tokens (no hard-coded hex).
 *
 * Scene-aware (v2): it reads the persisted scene choice and swaps its backdrop —
 * an interior composition for office/lounge, a rooftop skyline for town — plus a
 * scene-name chip so the fallback never disagrees with which scene was picked.
 */

/** How many busts the static scene shows before deferring to a "+N" chip. */
const BUST_CAP = 7;

/** Interior backdrop (office / lounge): back-wall glow, window, clock, shelves. */
function InteriorBackdrop() {
  return (
    <>
      <rect x="0" y="0" width="1000" height="380" fill="url(#stage-sun)" />
      <path d="M0 250 L1000 250 L1000 380 L0 380 Z" fill="url(#stage-floor)" />
      <line x1="0" y1="250" x2="1000" y2="250" stroke="var(--character-ink-soft)" strokeOpacity="0.12" strokeWidth="1.5" />
      <g stroke="var(--character-ink-soft)" strokeOpacity="0.16" strokeWidth="2" fill="var(--character-bubble)" fillOpacity="0.35">
        <rect x="80" y="70" width="150" height="110" rx="8" />
        <line x1="155" y1="70" x2="155" y2="180" />
        <line x1="80" y1="125" x2="230" y2="125" />
      </g>
      <g stroke="var(--character-ink-soft)" strokeOpacity="0.18" strokeWidth="2.5" fill="none">
        <circle cx="860" cy="110" r="30" />
        <path d="M860 110 L860 90 M860 110 L876 118" strokeLinecap="round" />
      </g>
      <g>
        <rect x="620" y="70" width="130" height="90" rx="6" fill="var(--agent-3a)" fillOpacity="0.18" stroke="var(--character-ink-soft)" strokeOpacity="0.14" strokeWidth="2" />
        <rect x="640" y="88" width="40" height="26" rx="3" fill="var(--agent-6a)" fillOpacity="0.4" />
        <rect x="690" y="88" width="40" height="26" rx="3" fill="var(--agent-4a)" fillOpacity="0.4" />
        <rect x="640" y="122" width="90" height="10" rx="3" fill="var(--character-ink-soft)" fillOpacity="0.15" />
      </g>
    </>
  );
}

/** Town backdrop: a warm sky, a rooftop skyline in varied tints, and a street. */
function TownBackdrop() {
  const roofs: Array<{ x: number; w: number; h: number; c: string }> = [
    { x: 40, w: 120, h: 130, c: 'var(--agent-1a)' },
    { x: 175, w: 90, h: 90, c: 'var(--agent-4a)' },
    { x: 280, w: 110, h: 150, c: 'var(--agent-6a)' },
    { x: 405, w: 80, h: 100, c: 'var(--agent-3a)' },
    { x: 500, w: 130, h: 170, c: 'var(--agent-2a)' },
    { x: 645, w: 95, h: 110, c: 'var(--agent-7a)' },
    { x: 755, w: 120, h: 145, c: 'var(--agent-5a)' },
    { x: 890, w: 80, h: 95, c: 'var(--agent-8a)' },
  ];
  return (
    <>
      <rect x="0" y="0" width="1000" height="380" fill="url(#stage-sun)" />
      {/* Buildings sitting on the pavement line. */}
      <g stroke="var(--character-ink-soft)" strokeOpacity="0.12" strokeWidth="1.5">
        {roofs.map((r) => (
          <g key={r.x}>
            <rect x={r.x} y={280 - r.h} width={r.w} height={r.h} rx="4" fill={r.c} fillOpacity="0.55" />
            {/* two lit window rows */}
            <rect x={r.x + 12} y={280 - r.h + 16} width="18" height="22" rx="2" fill="var(--agent-9a)" fillOpacity="0.5" />
            <rect x={r.x + r.w - 30} y={280 - r.h + 16} width="18" height="22" rx="2" fill="var(--agent-9a)" fillOpacity="0.5" />
            <rect x={r.x + 12} y={280 - r.h + 52} width="18" height="22" rx="2" fill="var(--agent-9a)" fillOpacity="0.35" />
          </g>
        ))}
      </g>
      {/* Street + centre dashes. */}
      <path d="M0 280 L1000 280 L1000 380 L0 380 Z" fill="var(--character-ink-soft)" fillOpacity="0.14" />
      <line x1="0" y1="330" x2="1000" y2="330" stroke="var(--agent-9a)" strokeOpacity="0.5" strokeWidth="3" strokeDasharray="26 22" />
    </>
  );
}

export function WorldStageStatic({
  agents,
  variant = 'band',
}: {
  agents: ReadonlyArray<WorldStageAgent>;
  variant?: 'band' | 'full';
}) {
  const intl = useIntl();
  // The scene choice is set by the interactive stage; mirror it here so the
  // fallback shows the right backdrop + label even without a live canvas.
  const [scene] = useState(() => getScene(readSceneKey()));
  const shown = agents.slice(0, BUST_CAP);
  const overflow = agents.length - shown.length;
  const onlineCount = agents.filter((a) => a.status === 'active').length;

  return (
    <div className="absolute inset-0 h-full w-full">
      {/* Painted backdrop — interior for office/lounge, skyline for town. */}
      <svg
        viewBox="0 0 1000 380"
        preserveAspectRatio="xMidYMax slice"
        className="absolute inset-0 h-full w-full"
        aria-hidden="true"
      >
        <defs>
          <radialGradient id="stage-sun" cx="18%" cy="10%" r="55%">
            <stop offset="0" stopColor="var(--agent-1a)" stopOpacity="0.5" />
            <stop offset="1" stopColor="var(--agent-1a)" stopOpacity="0" />
          </radialGradient>
          <linearGradient id="stage-floor" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0" stopColor="var(--agent-9a)" stopOpacity="0.22" />
            <stop offset="1" stopColor="var(--agent-9b)" stopOpacity="0.10" />
          </linearGradient>
        </defs>
        {scene.key === 'town' ? <TownBackdrop /> : <InteriorBackdrop />}
      </svg>

      {/* Scene-name chip (top-left) so the static view names its scene. The full
          page mirrors the live info card: scene name + online headcount. */}
      <div className="absolute left-3 top-3 z-10 rounded-lg border border-stone-200/70 bg-white/70 px-2.5 py-1 text-[11px] font-medium text-stone-600 backdrop-blur dark:border-white/10 dark:bg-stone-900/60 dark:text-stone-300">
        <span>{intl.formatMessage({ id: scene.nameId })}</span>
        {variant === 'full' && (
          <span className="ml-1.5 text-stone-500 dark:text-stone-400">
            · {intl.formatMessage({ id: 'world.online' }, { count: onlineCount })}
          </span>
        )}
      </div>

      {/* Foreground: real agent busts standing behind desks. */}
      <div className="absolute inset-x-0 bottom-0 flex items-end justify-evenly gap-2 px-4 pb-3 sm:px-8">
        {shown.length === 0 ? (
          <p className="mb-10 text-center text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'home.stage.empty' })}
          </p>
        ) : (
          shown.map((a) => {
            const live = a.status === 'active';
            return (
              <div key={a.name} className="flex min-w-0 flex-col items-center">
                <CharacterAvatar
                  agentId={a.name}
                  name={a.display_name}
                  size={72}
                  variant="bust"
                  pose={agentPose(a.status, live)}
                  live={live}
                />
                <div className="mt-1 h-2.5 w-16 rounded-t-md bg-gradient-to-b from-stone-300/80 to-stone-400/50 dark:from-stone-600/70 dark:to-stone-700/50" />
                <span className="mt-1 max-w-[5.5rem] truncate text-[11px] font-medium text-stone-600 dark:text-stone-300">
                  {a.display_name}
                </span>
              </div>
            );
          })
        )}
        {overflow > 0 && (
          <div className="mb-8 flex flex-col items-center justify-center">
            <span className="grid h-10 w-10 place-items-center rounded-full bg-stone-500/10 text-xs font-semibold text-stone-500 dark:bg-white/10 dark:text-stone-300">
              +{overflow}
            </span>
          </div>
        )}
      </div>
    </div>
  );
}
