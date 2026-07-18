import { useState } from 'react';
import { useIntl } from 'react-intl';
import { inputClass, selectClass } from './controlClass';
import type { AvailableModel } from '@/hooks/useAvailableModels';

const CUSTOM = '__custom__';

interface ModelSelectProps {
  value: string;
  onChange: (value: string) => void;
  models: AvailableModel[];
  loading?: boolean;
  error?: string | null;
  id?: string;
  ariaLabel?: string;
  /** RFC3339 timestamp of the last discovery run — drives "updated N ago". */
  discoveredAt?: string | null;
  /** True while a live re-probe is in flight (disables the refresh button). */
  refreshing?: boolean;
  /** When provided, renders a 🔄 refresh button that triggers a live re-probe. */
  onRefresh?: () => void;
}

/** Coarse relative-time bucket for `intl.formatRelativeTime`. */
function relativeParts(
  iso: string | null | undefined
): { value: number; unit: 'second' | 'minute' | 'hour' | 'day' } | null {
  if (!iso) return null;
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return null;
  const diffSec = Math.round((then - Date.now()) / 1000); // negative = past
  const abs = Math.abs(diffSec);
  if (abs < 60) return { value: diffSec, unit: 'second' };
  if (abs < 3600) return { value: Math.round(diffSec / 60), unit: 'minute' };
  if (abs < 86400) return { value: Math.round(diffSec / 3600), unit: 'hour' };
  return { value: Math.round(diffSec / 86400), unit: 'day' };
}

/**
 * Unified model picker backed by the live `models.list` registry.
 *
 * - Lists cloud/local models grouped by provider type.
 * - Keeps the current value selectable even when it is not in the registry
 *   (shown as "custom") — never silently drops an unknown / local model id.
 * - Offers an explicit "custom…" escape hatch to type any model id.
 * - Degrades to a plain text input when the registry cannot be fetched, so the
 *   operator can still enter a model instead of facing a fake or empty list.
 */
export function ModelSelect({
  value,
  onChange,
  models,
  loading,
  error,
  id,
  ariaLabel,
  discoveredAt,
  refreshing,
  onRefresh,
}: ModelSelectProps) {
  const intl = useIntl();
  const [custom, setCustom] = useState(false);

  const cloud = models.filter((m) => m.type === 'cloud');
  const local = models.filter((m) => m.type === 'local');
  const known = models.some((m) => m.id === value);
  const noList = Boolean(error) || (!loading && models.length === 0);

  // Group cloud models by provider so a provider whose live discovery failed
  // (source === 'fallback') can be flagged as a stale default list.
  const providerOrder: string[] = [];
  const byProvider = new Map<string, AvailableModel[]>();
  for (const m of cloud) {
    const p = m.provider ?? 'cloud';
    if (!byProvider.has(p)) {
      byProvider.set(p, []);
      providerOrder.push(p);
    }
    byProvider.get(p)!.push(m);
  }

  const parts = relativeParts(discoveredAt);
  const updatedText = parts
    ? intl.formatMessage(
        { id: 'model.select.updated' },
        { time: intl.formatRelativeTime(parts.value, parts.unit) }
      )
    : intl.formatMessage({ id: 'model.select.neverUpdated' });

  // Header row: "updated N ago" + optional 🔄 refresh button. Shown whenever a
  // refresh handler is wired, regardless of list/manual mode.
  const header = onRefresh ? (
    <div className="flex items-center justify-between gap-2">
      <span className="text-xs text-muted-foreground">{updatedText}</span>
      <button
        type="button"
        onClick={onRefresh}
        disabled={refreshing}
        title={intl.formatMessage({ id: 'model.select.refresh' })}
        aria-label={intl.formatMessage({ id: 'model.select.refresh' })}
        className="cursor-pointer text-xs text-muted-foreground transition hover:text-brand disabled:cursor-not-allowed disabled:opacity-50"
      >
        <span className={refreshing ? 'inline-block animate-spin' : undefined}>🔄</span>
      </button>
    </div>
  ) : null;

  // No usable registry (fetch failed, or empty and not loading) OR the operator
  // opted into free-form entry → plain text input. Never fabricate a list.
  if (noList || custom) {
    return (
      <div className="space-y-1">
        {header}
        <input
          id={id}
          type="text"
          value={value}
          aria-label={ariaLabel}
          onChange={(e) => onChange(e.target.value)}
          placeholder={intl.formatMessage({ id: 'model.select.customPlaceholder' })}
          className={inputClass}
        />
        {noList ? (
          <p className="text-xs text-destructive">
            {intl.formatMessage({ id: 'model.select.unavailable' })}
          </p>
        ) : (
          <button
            type="button"
            onClick={() => setCustom(false)}
            className="cursor-pointer text-xs text-brand hover:underline"
          >
            {intl.formatMessage({ id: 'model.select.backToList' })}
          </button>
        )}
      </div>
    );
  }

  const cloudLabel = intl.formatMessage({ id: 'model.select.cloud' });
  const fallbackNote = intl.formatMessage({ id: 'model.select.fallbackNote' });

  return (
    <div className="space-y-1">
      {header}
      <select
        id={id}
        aria-label={ariaLabel}
        value={value}
        disabled={loading}
        onChange={(e) => {
          if (e.target.value === CUSTOM) {
            setCustom(true);
            return;
          }
          onChange(e.target.value);
        }}
        className={selectClass}
      >
        {loading && <option value={value}>{intl.formatMessage({ id: 'model.select.loading' })}</option>}
        {!loading && value === '' && (
          <option value="">{intl.formatMessage({ id: 'model.select.placeholder' })}</option>
        )}
        {value !== '' && !known && (
          <option value={value}>
            {intl.formatMessage({ id: 'model.select.customLabel' }, { model: value })}
          </option>
        )}
        {providerOrder.map((provider) => {
          const group = byProvider.get(provider)!;
          const isFallback = group.every((m) => m.source === 'fallback');
          const providerLabel = provider === 'cloud' ? cloudLabel : `${cloudLabel} · ${provider}`;
          const label = isFallback ? `${providerLabel} ${fallbackNote}` : providerLabel;
          return (
            <optgroup key={provider} label={label}>
              {group.map((m) => (
                <option key={m.id} value={m.id}>{m.label}</option>
              ))}
            </optgroup>
          );
        })}
        {local.length > 0 && (
          <optgroup label={intl.formatMessage({ id: 'model.select.local' })}>
            {local.map((m) => (
              <option key={m.id} value={m.id}>{m.label}</option>
            ))}
          </optgroup>
        )}
        <option value={CUSTOM}>{intl.formatMessage({ id: 'model.select.custom' })}</option>
      </select>
    </div>
  );
}
