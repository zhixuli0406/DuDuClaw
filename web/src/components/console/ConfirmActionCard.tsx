import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, RefreshCw, Power, Check, X, type LucideIcon } from 'lucide-react';
import { Button, Input } from '@/components/mds';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import type { ConfirmActionArtifact, ConfirmActionKind } from './artifact-types';
import { ArtifactShell } from './ArtifactShell';

const RESTART_COUNTDOWN_SECONDS = 5;

const ACTION_ICON: Record<ConfirmActionKind, LucideIcon> = {
  factory_reset: AlertTriangle,
  restart: RefreshCw,
  shutdown: Power,
};

/** `device.danger.<key>.*` i18n key segment per action — same catalogue
 *  DevicePage's DangerZone uses, so the copy is identical whether the user
 *  reaches the action from `/device` or from conversation. */
const ACTION_KEY: Record<ConfirmActionKind, string> = {
  factory_reset: 'factoryReset',
  restart: 'restart',
  shutdown: 'shutdown',
};

type CardStatus = 'idle' | 'busy' | 'done' | 'cancelled';

/**
 * 破壞性操作內嵌確認卡 — the O-3 replacement for DevicePage's modal
 * `ConfirmDialog` when the same action is triggered through conversation
 * (§6.2: "破壞性操作內嵌確認（輸入 RESET／倒數）...比照 DevicePage DangerZone
 * 慣例但內嵌對話"). Nothing happens until the user explicitly confirms here
 * — clicking through this card is what calls the gated `device.*` RPC
 * (`confirm: true` is still only ever sent from this one place, never
 * inferred client-side ahead of time — same authority boundary DevicePage
 * enforces, see coding convention #4).
 *
 *  - `factory_reset` (irreversible): confirm stays disabled until the user
 *    types the literal string "RESET", mirroring DevicePage exactly.
 *  - `restart` / `shutdown` (disruptive, not data-destroying): confirm stays
 *    disabled for a short mandatory countdown instead — a chat message is
 *    easier to fire off by accident than a page you had to navigate to, so
 *    the countdown buys a moment to reconsider without demanding typed
 *    confirmation for something recoverable.
 */
export function ConfirmActionCard({ payload }: { payload: ConfirmActionArtifact['payload'] }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const { action } = payload;
  const key = ACTION_KEY[action];
  const Icon = ACTION_ICON[action];

  const [status, setStatus] = useState<CardStatus>('idle');
  const [typed, setTyped] = useState('');
  const [secondsLeft, setSecondsLeft] = useState(action === 'factory_reset' ? 0 : RESTART_COUNTDOWN_SECONDS);
  const [error, setError] = useState<string | null>(null);

  // One `setInterval` registered once at mount — NOT a `setTimeout`
  // rescheduled from inside the effect on every `secondsLeft` change. The
  // latter ties each tick to React having already flushed the previous
  // render before the next timer gets armed, which is fragile under fake
  // timers (and just generally more moving parts); a single interval ticks
  // on its own schedule regardless of render timing, using a functional
  // state update so it never reads a stale `secondsLeft`.
  useEffect(() => {
    if (action === 'factory_reset') return;
    const id = setInterval(() => {
      setSecondsLeft((s) => {
        if (s <= 1) {
          clearInterval(id);
          return 0;
        }
        return s - 1;
      });
    }, 1000);
    return () => clearInterval(id);
  }, [action]);

  const canConfirm =
    status === 'idle' && (action === 'factory_reset' ? typed.trim() === 'RESET' : secondsLeft <= 0);

  const runConfirm = async () => {
    setStatus('busy');
    setError(null);
    try {
      if (action === 'factory_reset') {
        await api.device.factoryReset();
      } else {
        await api.device.power(action);
      }
      toast.success(t(`device.danger.${key}.done`));
      setStatus('done');
    } catch (e) {
      console.warn('[console.artifact.confirm_action]', action, e);
      const message = formatError(e);
      toast.error(message);
      setError(message);
      setStatus('idle'); // allow an immediate retry without re-arming the gate
    }
  };

  if (status === 'done') {
    return (
      <ArtifactShell icon={Check} title={t(`device.danger.${key}.title`)}>
        <p className="flex items-center gap-2 text-sm text-success">
          <Check className="size-4 shrink-0" aria-hidden="true" />
          {t(`device.danger.${key}.done`)}
        </p>
      </ArtifactShell>
    );
  }

  if (status === 'cancelled') {
    return (
      <ArtifactShell icon={X} title={t(`device.danger.${key}.title`)}>
        <p className="text-sm text-muted-foreground">{t('console.artifact.confirmAction.cancelled')}</p>
      </ArtifactShell>
    );
  }

  return (
    <ArtifactShell icon={Icon} title={t(`device.danger.${key}.title`)}>
      <div className="space-y-3">
        <p className="text-sm text-muted-foreground">{t(`device.danger.${key}.desc`)}</p>

        {error && (
          <p role="alert" className="rounded-lg border border-destructive/30 bg-destructive/10 px-2.5 py-1.5 text-xs text-destructive">
            {error}
          </p>
        )}

        {action === 'factory_reset' ? (
          <div className="space-y-1.5">
            <p className="text-xs text-muted-foreground">{t('device.danger.factoryReset.confirmHint')}</p>
            <Input
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              placeholder="RESET"
              disabled={status === 'busy'}
            />
          </div>
        ) : (
          secondsLeft > 0 && (
            <p className="text-xs text-muted-foreground">
              {t('console.artifact.confirmAction.countdown', { seconds: secondsLeft })}
            </p>
          )
        )}

        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={() => setStatus('cancelled')} disabled={status === 'busy'}>
            {t('common.cancel')}
          </Button>
          <Button variant="destructive" size="sm" onClick={() => void runConfirm()} disabled={!canConfirm}>
            {status === 'busy' ? t('common.saving') : t(`device.danger.${key}.cta`)}
          </Button>
        </div>
      </div>
    </ArtifactShell>
  );
}
