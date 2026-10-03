import { useEffect } from 'react';
import { useIntl } from 'react-intl';
import { RotateCw, X } from 'lucide-react';
import { api } from '@/lib/api';
import { useRestartRequiredStore } from '@/stores/restart-required-store';

/**
 * Persistent 「以下設定要重啟 gateway 才生效：…」 banner (v1.68, W2). Fed by
 * every save that returns `restart_required`; disappears once the gateway
 * reports an uptime that started after the first pending save, or when the
 * operator dismisses it.
 */
export function RestartRequiredBanner() {
  const intl = useIntl();
  const keys = useRestartRequiredStore((s) => s.keys);
  const clear = useRestartRequiredStore((s) => s.clear);
  const reconcileUptime = useRestartRequiredStore((s) => s.reconcileUptime);

  useEffect(() => {
    if (keys.length === 0) return;
    let cancelled = false;
    Promise.resolve()
      .then(() => api.system.status())
      .then((st) => {
        if (!cancelled && typeof st?.uptime_seconds === 'number') reconcileUptime(st.uptime_seconds);
      })
      .catch(() => {
        /* keep the banner — not knowing is not the same as restarted */
      });
    return () => {
      cancelled = true;
    };
    // Check once per mount / when the pending set grows.
  }, [keys.length, reconcileUptime]);

  if (keys.length === 0) return null;

  return (
    <div
      role="status"
      data-testid="restart-required-banner"
      className="flex items-start gap-2 rounded-lg border border-warning/40 bg-warning/10 px-3 py-2.5 text-sm text-foreground"
    >
      <RotateCw className="mt-0.5 size-4 shrink-0 text-warning" aria-hidden="true" />
      <div className="min-w-0 flex-1 space-y-0.5">
        <p className="font-medium">
          {intl.formatMessage({ id: 'settings.restartRequired.title' }, { keys: keys.join('、') })}
        </p>
        <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'settings.restartRequired.hint' })}</p>
      </div>
      <button
        type="button"
        onClick={clear}
        className="shrink-0 rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground"
        aria-label={intl.formatMessage({ id: 'settings.restartRequired.dismiss' })}
      >
        <X className="size-3.5" />
      </button>
    </div>
  );
}
