import { useIntl } from 'react-intl';
import { MapPin, RotateCcw, LayoutGrid } from 'lucide-react';
import type { SceneDefinition, SceneKey } from './types';

/**
 * ScenePicker — the floating "場景" ROOM card (openhuman-style). A frosted-glass
 * tile pinned to a corner of the world: a title, a segmented button group of
 * scenes (the selected one filled amber), and the selected scene's one-line
 * description. On narrow viewports it collapses to a native `<select>`.
 *
 * The header carries a ⟲ recenter button (2D camera → back to the contain-fit
 * framing) and, on the immersive `/world` page, a ⊞ 清單 button that drops to the
 * static list view. `align` pins it top-left (Home band) or top-right (full page).
 */
export interface ScenePickerProps {
  readonly scenes: ReadonlyArray<SceneDefinition>;
  readonly value: SceneKey;
  readonly onChange: (key: SceneKey) => void;
  /** Snap the 2D camera back to its initial contain-fit framing (⟲ button). */
  readonly onRecenter?: () => void;
  /** When set, render a ⊞ 清單 button that switches to the static list view. */
  readonly onToggleList?: () => void;
  /** Which corner to pin to. Defaults to top-left. */
  readonly align?: 'left' | 'right';
}

export function ScenePicker({ scenes, value, onChange, onRecenter, onToggleList, align = 'left' }: ScenePickerProps) {
  const intl = useIntl();
  const active = scenes.find((s) => s.key === value) ?? scenes[0];

  return (
    <div className={`absolute ${align === 'right' ? 'right-4' : 'left-3'} top-4 z-10 max-w-[calc(100%-1.5rem)]`}>
      <div className="rounded-xl border border-stone-200/70 bg-white/70 p-2.5 shadow-soft backdrop-blur-md dark:border-white/10 dark:bg-stone-900/60">
        <div className="mb-1.5 flex items-center gap-1.5 px-0.5 text-[11px] font-semibold uppercase tracking-wide text-stone-500 dark:text-stone-400">
          <MapPin className="h-3 w-3" />
          {intl.formatMessage({ id: 'world.scene.title' })}
          <div className="ml-auto flex items-center gap-0.5">
            {onRecenter && (
              <button
                type="button"
                onClick={onRecenter}
                title={intl.formatMessage({ id: 'world.recenter' })}
                aria-label={intl.formatMessage({ id: 'world.recenter' })}
                className="inline-flex h-5 w-5 items-center justify-center rounded-md text-stone-500 transition-colors hover:bg-stone-500/10 hover:text-stone-700 dark:text-stone-400 dark:hover:bg-white/10 dark:hover:text-stone-200"
              >
                <RotateCcw className="h-3 w-3" />
              </button>
            )}
            {onToggleList && (
              <button
                type="button"
                onClick={onToggleList}
                title={intl.formatMessage({ id: 'home.stage.toggleList' })}
                aria-label={intl.formatMessage({ id: 'home.stage.toggleList' })}
                className="inline-flex h-5 items-center gap-1 rounded-md px-1 text-[10px] font-medium text-stone-500 transition-colors hover:bg-stone-500/10 hover:text-stone-700 dark:text-stone-400 dark:hover:bg-white/10 dark:hover:text-stone-200"
              >
                <LayoutGrid className="h-3 w-3" />
                {intl.formatMessage({ id: 'home.stage.toggle' })}
              </button>
            )}
          </div>
        </div>

        {/* Desktop / tablet: segmented button group. */}
        <div className="hidden gap-1 sm:flex" role="group" aria-label={intl.formatMessage({ id: 'world.scene.title' })}>
          {scenes.map((s) => {
            const selected = s.key === value;
            return (
              <button
                key={s.key}
                type="button"
                aria-pressed={selected}
                onClick={() => onChange(s.key)}
                className={
                  'rounded-lg px-2.5 py-1 text-xs font-medium transition-colors ' +
                  (selected
                    ? 'bg-amber-500 text-white shadow-sm'
                    : 'text-stone-600 hover:bg-stone-500/10 dark:text-stone-300 dark:hover:bg-white/10')
                }
              >
                {intl.formatMessage({ id: s.nameId })}
              </button>
            );
          })}
        </div>

        {/* Mobile: native select (compact, battery-friendly). */}
        <label className="sm:hidden">
          <span className="sr-only">{intl.formatMessage({ id: 'world.scene.title' })}</span>
          <select
            value={value}
            onChange={(e) => onChange(e.target.value as SceneKey)}
            className="w-full rounded-lg border border-stone-200/70 bg-white/80 px-2 py-1 text-xs font-medium text-stone-700 dark:border-white/10 dark:bg-stone-900/70 dark:text-stone-200"
          >
            {scenes.map((s) => (
              <option key={s.key} value={s.key}>
                {intl.formatMessage({ id: s.nameId })}
              </option>
            ))}
          </select>
        </label>

        <p className="mt-1.5 max-w-[15rem] px-0.5 text-[11px] leading-snug text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: active.descId })}
        </p>
      </div>
    </div>
  );
}
