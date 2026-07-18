import { useIntl } from 'react-intl';
import { MapPin, RotateCcw, LayoutGrid } from 'lucide-react';
import { Button, Segmented, type SegmentedOption } from '@/components/mds';
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
  const options: SegmentedOption<SceneKey>[] = scenes.map((s) => ({
    value: s.key,
    label: intl.formatMessage({ id: s.nameId }),
  }));

  return (
    <div className={`absolute ${align === 'right' ? 'right-4' : 'left-3'} top-4 z-10 max-w-[calc(100%-1.5rem)]`}>
      <div className="rounded-xl border border-surface-border bg-surface/90 p-2.5 shadow-[var(--menu-shadow)] backdrop-blur-md">
        <div className="mb-1.5 flex items-center gap-1.5 px-0.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
          <MapPin className="h-3 w-3" />
          {intl.formatMessage({ id: 'world.scene.title' })}
          <div className="ml-auto flex items-center gap-0.5">
            {onRecenter && (
              <Button
                variant="ghost"
                size="icon-xs"
                onClick={onRecenter}
                title={intl.formatMessage({ id: 'world.recenter' })}
                aria-label={intl.formatMessage({ id: 'world.recenter' })}
              >
                <RotateCcw />
              </Button>
            )}
            {onToggleList && (
              <Button
                variant="ghost"
                size="xs"
                onClick={onToggleList}
                title={intl.formatMessage({ id: 'home.stage.toggleList' })}
                aria-label={intl.formatMessage({ id: 'home.stage.toggleList' })}
              >
                <LayoutGrid />
                {intl.formatMessage({ id: 'home.stage.toggle' })}
              </Button>
            )}
          </div>
        </div>

        {/* Desktop / tablet: mds Segmented scene switcher. */}
        <div className="hidden sm:block">
          <Segmented
            value={value}
            onValueChange={onChange}
            options={options}
            aria-label={intl.formatMessage({ id: 'world.scene.title' })}
          />
        </div>

        {/* Mobile: native select (compact, battery-friendly). */}
        <label className="sm:hidden">
          <span className="sr-only">{intl.formatMessage({ id: 'world.scene.title' })}</span>
          <select
            value={value}
            onChange={(e) => onChange(e.target.value as SceneKey)}
            className="h-8 w-full rounded-lg border border-input bg-transparent px-2.5 py-1 text-xs font-medium text-foreground dark:bg-input/30"
          >
            {scenes.map((s) => (
              <option key={s.key} value={s.key}>
                {intl.formatMessage({ id: s.nameId })}
              </option>
            ))}
          </select>
        </label>

        <p className="mt-1.5 max-w-[15rem] px-0.5 text-[11px] leading-snug text-muted-foreground">
          {intl.formatMessage({ id: active.descId })}
        </p>
      </div>
    </div>
  );
}
