import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { RotateCw, Users } from 'lucide-react';
import { useAuthStore } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';
import type { AgentLiveState } from '@/stores/agent-activity-store';
import { SCENES, getScene, readSceneKey, resolveRoomPalette, writeSceneKey } from './rooms';
import { worldObjectRoute } from './interactions';
import { useWorldState, type WorldInputAgent } from './useWorldState';
import type { SceneKey, WorldObjectId } from './types';
import { WorldScene } from './stage-scene';
import { ScenePicker } from './ScenePicker';

/**
 * WorldStage — the PixiJS 2D isometric world. Renders the room + one character
 * per agent, driven by `useWorldState`, inside a pan/zoom 2D camera (wheel/pinch
 * zoom + drag pan + a ⟲ recenter button — no rotation). PixiJS (+ the CSP
 * `unsafe-eval` polyfill) is dynamically imported inside `WorldScene`, so
 * mounting this component is what pulls the lazy chunk; the static fallback never
 * touches it.
 *
 * On renderer-init failure (WebGL context wedge / 10s timeout) it shows an inline
 * error panel with a retry button rather than a blank canvas. The scene is torn
 * down and rebuilt when the colour theme changes (palette is read from CSS tokens
 * at build time).
 *
 * `variant`: `'band'` is the compact Home strip (ScenePicker top-left, no info
 * card); `'full'` is the immersive `/world` page — the ROOM panel floats
 * top-right (carrying ⟲ and, when `onToggleList` is given, a ⊞ 清單 button) and a
 * small info card (scene name + online headcount) floats top-left.
 */
export interface WorldStageProps {
  agents: ReadonlyArray<WorldInputAgent>;
  variant?: 'band' | 'full';
  /** Full variant only: switch to the list/static view from inside the ROOM panel. */
  onToggleList?: () => void;
}

/** Detect the active theme so a change can trigger a palette rebuild. */
function currentThemeKey(): string {
  if (typeof document === 'undefined') return 'light';
  const attr = document.documentElement.getAttribute('data-theme');
  if (attr === 'dark' || attr === 'light') return attr;
  return typeof matchMedia !== 'undefined' && matchMedia('(prefers-color-scheme: dark)').matches
    ? 'dark'
    : 'light';
}

export function WorldStage({ agents, variant = 'band', onToggleList }: WorldStageProps) {
  const intl = useIntl();
  const navigate = useNavigate();
  const role = useAuthStore((s) => s.user?.role);
  const isManager = hasMinRole(role, 'manager');
  const isFull = variant === 'full';

  const hostRef = useRef<HTMLDivElement | null>(null);
  const sceneRef = useRef<WorldScene | null>(null);
  const [status, setStatus] = useState<'booting' | 'ready' | 'error'>('booting');
  const [attempt, setAttempt] = useState(0);
  const [themeKey, setThemeKey] = useState(currentThemeKey);
  const [sceneKey, setSceneKey] = useState<SceneKey>(readSceneKey);

  const scene = getScene(sceneKey);
  const onlineCount = useMemo(() => agents.filter((a) => a.status === 'active').length, [agents]);

  const chooseScene = useCallback((key: SceneKey) => {
    writeSceneKey(key);
    setSceneKey(key);
  }, []);

  // i18n bubble phrases for live states (kept out of the pure mapping layer).
  const phraseFor = useCallback(
    (state: AgentLiveState): string => intl.formatMessage({ id: `world.say.${state}` }),
    [intl],
  );

  const states = useWorldState(agents, { phraseFor, scene });

  const onObject = useCallback(
    (object: WorldObjectId, agentId?: string) => {
      const route = worldObjectRoute(object, { isManager }, agentId);
      if (route) navigate(route);
    },
    [isManager, navigate],
  );

  const onRecenter = useCallback(() => {
    sceneRef.current?.resetCamera();
  }, []);

  // Watch for theme changes → rebuild the scene with a fresh palette.
  useEffect(() => {
    if (typeof document === 'undefined') return;
    const sync = () => setThemeKey(currentThemeKey());
    const mo = new MutationObserver(sync);
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'class'] });
    const mq = typeof matchMedia !== 'undefined' ? matchMedia('(prefers-color-scheme: dark)') : null;
    mq?.addEventListener('change', sync);
    return () => {
      mo.disconnect();
      mq?.removeEventListener('change', sync);
    };
  }, []);

  // Mount / rebuild the scene. Re-runs on retry (attempt) and theme/scene change.
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let cancelled = false;
    setStatus('booting');
    const worldScene = new WorldScene({
      palette: resolveRoomPalette(),
      room: scene.build(agents.length),
      scene,
      onObject,
    });
    worldScene
      .init(host)
      .then(() => {
        if (cancelled) {
          worldScene.destroy();
          return;
        }
        sceneRef.current = worldScene;
        setStatus('ready');
      })
      .catch((error: unknown) => {
        console.error('[world] stage init failed:', error);
        if (!cancelled) setStatus('error');
        worldScene.destroy();
      });
    return () => {
      cancelled = true;
      sceneRef.current = null;
      worldScene.destroy();
    };
    // room rebuilds on agent-count / theme / scene change; onObject stable per role.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [attempt, themeKey, agents.length, sceneKey]);

  // Push authoritative states into the scene whenever they change.
  useEffect(() => {
    if (status === 'ready') sceneRef.current?.updateAgents(states);
  }, [states, status]);

  const sectionClass = isFull
    ? 'relative h-full w-full overflow-hidden bg-gradient-to-b from-amber-50 via-orange-50/40 to-stone-100 dark:from-stone-800 dark:via-stone-800/60 dark:to-stone-900'
    : 'relative h-[38vh] min-h-[260px] w-full overflow-hidden rounded-2xl border border-stone-200/70 bg-gradient-to-b from-amber-50 via-orange-50/40 to-stone-100 shadow-soft dark:border-white/5 dark:from-stone-800 dark:via-stone-800/60 dark:to-stone-900';

  return (
    <section role="img" aria-label={intl.formatMessage({ id: 'home.stage.aria' })} className={sectionClass}>
      <div ref={hostRef} className="absolute inset-0 h-full w-full" />

      {status === 'ready' && (
        <>
          {isFull && (
            <div className="pointer-events-none absolute left-4 top-4 z-10 rounded-xl border border-stone-200/70 bg-white/70 px-3 py-2 shadow-soft backdrop-blur-md dark:border-white/10 dark:bg-stone-900/60">
              <p className="text-sm font-semibold text-stone-700 dark:text-stone-100">
                {intl.formatMessage({ id: scene.nameId })}
              </p>
              <p className="mt-0.5 flex items-center gap-1 text-xs text-stone-500 dark:text-stone-400">
                <Users className="h-3 w-3" />
                {intl.formatMessage({ id: 'world.online' }, { count: onlineCount })}
              </p>
            </div>
          )}
          <ScenePicker
            scenes={SCENES}
            value={sceneKey}
            onChange={chooseScene}
            onRecenter={onRecenter}
            align={isFull ? 'right' : 'left'}
            onToggleList={isFull ? onToggleList : undefined}
          />
        </>
      )}

      {status === 'booting' && (
        <div className="absolute inset-0 grid place-items-center">
          <p className="animate-pulse text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'world.booting' })}
          </p>
        </div>
      )}

      {status === 'error' && (
        <div className="absolute inset-0 grid place-items-center px-6">
          <div className="flex max-w-sm flex-col items-center gap-3 rounded-xl border border-stone-200/70 bg-white/80 p-5 text-center backdrop-blur dark:border-white/10 dark:bg-stone-900/70">
            <p className="text-sm font-medium text-stone-700 dark:text-stone-200">
              {intl.formatMessage({ id: 'world.error.title' })}
            </p>
            <p className="text-xs text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'world.error.hint' })}
            </p>
            <button
              type="button"
              onClick={() => setAttempt((n) => n + 1)}
              className="inline-flex items-center gap-1.5 rounded-lg bg-amber-500 px-3 py-1.5 text-xs font-semibold text-white transition-transform hover:bg-amber-600 active:scale-[0.97]"
            >
              <RotateCw className="h-3.5 w-3.5" />
              {intl.formatMessage({ id: 'world.error.retry' })}
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
