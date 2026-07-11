import { Suspense, lazy, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { LayoutGrid, Sparkles, Maximize2 } from 'lucide-react';
import { WorldStageStatic } from './WorldStageStatic';
import {
  resolveStageMode,
  detectWebglSupport,
  readStageChoice,
  writeStageChoice,
  type StageChoice,
} from '@/components/world/stage-mode';

/**
 * WorldStagePlaceholder — the world mount point + degradation *splitter*. It
 * resolves the fallback chain and renders exactly one of:
 *   ① the static illustrated office (`WorldStageStatic`) when reduced-motion is
 *      on, WebGL is unavailable, or the user picked the "⊞ 清單" list view; or
 *   ② the interactive PixiJS `WorldStage` (lazily loaded — its chunk, incl.
 *      pixi.js + the CSP unsafe-eval polyfill, only downloads here) otherwise.
 *
 * `variant`: `'band'` is the compact 38vh Home strip; `'full'` is the immersive
 * `/world` page (the parent gives it a full-bleed, viewport-height box). Both go
 * through the same splitter. In the band the top-right carries a ⤢ 展開 button
 * (→ `/world`) plus the ⊞/✨ toggle; in the full page the ⊞ lives inside the
 * ROOM panel (passed down as `onToggleList`).
 */
export interface WorldStageAgent {
  name: string;
  display_name: string;
  status: 'active' | 'paused' | 'terminated';
}

export interface WorldStageProps {
  /** Agents to stage, already data-scoped by the caller. */
  agents: ReadonlyArray<WorldStageAgent>;
  variant?: 'band' | 'full';
}

/** Band-mode band styling (identical box to WorldStage's band section). */
const BAND_CLASS =
  'relative h-[38vh] min-h-[260px] w-full overflow-hidden rounded-2xl border border-stone-200/70 bg-gradient-to-b from-amber-50 via-orange-50/40 to-stone-100 shadow-soft dark:border-white/5 dark:from-stone-800 dark:via-stone-800/60 dark:to-stone-900';

const LazyWorldStage = lazy(() =>
  import('@/components/world/WorldStage').then((m) => ({ default: m.WorldStage })),
);

/** Watch a media query, returning its current match reactively. */
function useMediaMatch(query: string): boolean {
  const [matches, setMatches] = useState(() =>
    typeof matchMedia !== 'undefined' ? matchMedia(query).matches : false,
  );
  useEffect(() => {
    if (typeof matchMedia === 'undefined') return;
    const mq = matchMedia(query);
    const on = () => setMatches(mq.matches);
    on();
    mq.addEventListener('change', on);
    return () => mq.removeEventListener('change', on);
  }, [query]);
  return matches;
}

export function WorldStagePlaceholder({ agents, variant = 'band' }: WorldStageProps) {
  const intl = useIntl();
  const navigate = useNavigate();
  const isFull = variant === 'full';
  const prefersReducedMotion = useMediaMatch('(prefers-reduced-motion: reduce)');
  const isMobile = useMediaMatch('(max-width: 767px)');
  const [webglAvailable] = useState(detectWebglSupport);
  const [userChoice, setUserChoice] = useState<StageChoice>(readStageChoice);

  const mode = resolveStageMode({ prefersReducedMotion, webglAvailable, userChoice, isMobile });
  const stagePossible = webglAvailable && !prefersReducedMotion;

  const toggle = () => {
    const next = mode === 'stage' ? 'list' : 'stage';
    writeStageChoice(next);
    setUserChoice(next);
  };

  const bootFallback = (
    <div className={isFull ? 'h-full w-full' : BAND_CLASS}>
      <div className="absolute inset-0 grid place-items-center">
        <p className="animate-pulse text-sm text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'world.booting' })}
        </p>
      </div>
    </div>
  );

  const stageBranch =
    mode === 'stage' ? (
      <Suspense fallback={bootFallback}>
        <LazyWorldStage
          agents={agents}
          variant={variant}
          onToggleList={isFull && stagePossible ? toggle : undefined}
        />
      </Suspense>
    ) : (
      <section
        aria-label={intl.formatMessage({ id: 'home.stage.aria' })}
        className={isFull ? 'relative h-full w-full overflow-hidden' : BAND_CLASS}
      >
        <WorldStageStatic agents={agents} variant={variant} />
      </section>
    );

  // Full page: the parent owns the box; the ⊞ toggle lives inside the ROOM panel
  // for stage mode, or floats top-right for the static fallback.
  if (isFull) {
    return (
      <div className="relative h-full w-full">
        {stageBranch}
        {mode !== 'stage' && stagePossible && (
          <button
            type="button"
            onClick={toggle}
            title={intl.formatMessage({ id: 'home.stage.toggleStage' })}
            aria-label={intl.formatMessage({ id: 'home.stage.toggleStage' })}
            className="absolute right-4 top-4 z-10 inline-flex items-center gap-1.5 rounded-lg border border-stone-200/70 bg-white/70 px-2.5 py-1.5 text-xs font-medium text-stone-600 backdrop-blur transition-transform hover:bg-white/90 active:scale-[0.97] dark:border-white/10 dark:bg-stone-900/60 dark:text-stone-300"
          >
            <Sparkles className="h-3.5 w-3.5" />
            {intl.formatMessage({ id: 'home.stage.toggleStageLabel' })}
          </button>
        )}
      </div>
    );
  }

  // Band: the ⤢ 展開 link (→ /world) plus the ⊞/✨ list toggle, top-right.
  return (
    <div className="relative">
      {stageBranch}
      <div className="absolute right-3 top-3 z-10 flex items-center gap-1.5">
        <button
          type="button"
          onClick={() => navigate('/world')}
          title={intl.formatMessage({ id: 'world.expand' })}
          aria-label={intl.formatMessage({ id: 'world.expand' })}
          className="inline-flex items-center gap-1.5 rounded-lg border border-stone-200/70 bg-white/70 px-2.5 py-1.5 text-xs font-medium text-stone-600 backdrop-blur transition-transform hover:bg-white/90 active:scale-[0.97] dark:border-white/10 dark:bg-stone-900/60 dark:text-stone-300"
        >
          <Maximize2 className="h-3.5 w-3.5" />
          {intl.formatMessage({ id: 'world.expand.label' })}
        </button>
        {stagePossible && (
          <button
            type="button"
            onClick={toggle}
            title={intl.formatMessage({ id: mode === 'stage' ? 'home.stage.toggleList' : 'home.stage.toggleStage' })}
            aria-label={intl.formatMessage({ id: mode === 'stage' ? 'home.stage.toggleList' : 'home.stage.toggleStage' })}
            className="inline-flex items-center gap-1.5 rounded-lg border border-stone-200/70 bg-white/70 px-2.5 py-1.5 text-xs font-medium text-stone-600 backdrop-blur transition-transform hover:bg-white/90 active:scale-[0.97] dark:border-white/10 dark:bg-stone-900/60 dark:text-stone-300"
          >
            {mode === 'stage' ? <LayoutGrid className="h-3.5 w-3.5" /> : <Sparkles className="h-3.5 w-3.5" />}
            {intl.formatMessage({ id: mode === 'stage' ? 'home.stage.toggle' : 'home.stage.toggleStageLabel' })}
          </button>
        )}
      </div>
    </div>
  );
}
