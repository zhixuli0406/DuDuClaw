import { useState } from 'react';
import { useIntl } from 'react-intl';
import { Download, Check, X, RotateCcw } from 'lucide-react';
import { Button } from '@/components/mds';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';
import type { UpdateConfirmArtifact } from './artifact-types';
import { ArtifactShell } from './ArtifactShell';

type CardStatus = 'idle' | 'busy' | 'done' | 'cancelled';

/**
 * 更新內嵌確認卡 — the O-3 twin of `ConfirmActionCard` for a pending
 * `os_apply_update` (O-0). Unlike `restart`/`shutdown`/`factory_reset`,
 * applying an update is recoverable and briefly disruptive at worst — no
 * data is destroyed — so this card skips both `ConfirmActionCard`'s typed
 * "RESET" gate and its mandatory countdown: one explicit click is enough.
 *
 * `payload.target` selects which of the two RPCs `os_apply_update` bridges
 * the confirm click actually calls (see `mcp_os_ops.rs`'s doc comment for
 * why the O-0 tool combines them under one `target` param):
 *  - `device` → `api.device.updateApply()` — the appliance OS image update
 *    via duduclaw-sysd (`DeviceOpResult { success, stdout, stderr }`).
 *  - `system` → `api.system.applyUpdate()` — duduclaw's own self-update
 *    (`{ success, message, needs_restart }`).
 * The two RPCs return different shapes, so this card normalizes both into
 * one `{ ok, detail }` pair for rendering rather than forcing a fake
 * `DeviceOpResult` onto the system branch.
 *
 * `system.applyUpdate()`'s `needs_restart` flag has no counterpart on the
 * `device` branch (`DeviceOpResult` never carries it) — when true, the
 * gateway's own `system.apply_update` handler (`handlers.rs`) has ALREADY
 * broadcast a `system.update_installed` WS event and scheduled its own
 * self-restart ~3s later (`MainLayout`'s `useUpdateStore` already renders a
 * full-page "restarting…" overlay off that same event, app-wide). This
 * card's reminder is therefore deliberately text-only, not a button: there
 * is no separate action left to trigger — `device.power('restart')` would be
 * the WRONG call here regardless (it restarts the appliance via
 * duduclaw-sysd, not DuDuClaw's own process).
 *
 * A settled-but-unsuccessful result (`success: false` — the RPC completed,
 * the update just didn't apply) is treated the same as a thrown RPC error:
 * both surface the failure inline and re-arm the confirm button for an
 * immediate retry, matching `ConfirmActionCard`'s "allow an immediate retry
 * without re-arming the gate" convention.
 */
export function UpdateConfirmCard({ payload }: { payload: UpdateConfirmArtifact['payload'] }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const { target } = payload;

  const [status, setStatus] = useState<CardStatus>('idle');
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);
  const [detail, setDetail] = useState<string | null>(null);
  /** `target === 'system'` only — `DeviceOpResult` has no such field. */
  const [needsRestart, setNeedsRestart] = useState(false);

  const title = t(`console.artifact.updateConfirm.title.${target}`);
  // `target: 'device'` is the appliance OS image (duduclaw-sysd) — DevicePage
  // (`/device`, legacy-redirects to `/app/system/device`) is where that
  // status lives. `target: 'system'` is duduclaw's OWN self-update, which
  // has never lived on DevicePage — it moved to its own management entry
  // (2026-08-04, D18) and now lives at `/app/system/updates`
  // (`SystemUpdatePage`, N-3 relocation). Pointing a system-target card at
  // `/device` sent the operator to a page with no matching content.
  const advancedTo = target === 'system' ? '/app/system/updates' : '/device';

  const runConfirm = async () => {
    setStatus('busy');
    setError(null);
    try {
      if (target === 'device') {
        const res = await api.device.updateApply();
        const okDetail = [res.stdout, res.stderr].filter(Boolean).join('\n').trim() || null;
        if (res.success) {
          setSuccess(true);
          setDetail(okDetail);
          toast.success(t('device.update.applyDone'));
          setStatus('done');
        } else {
          const message = t('device.update.applyFailed');
          toast.error(message);
          setError(okDetail ?? message);
          setStatus('idle'); // allow an immediate retry without re-arming the gate
        }
      } else {
        const res = await api.system.applyUpdate();
        if (res.success) {
          setSuccess(true);
          setDetail(res.message || null);
          setNeedsRestart(res.needs_restart === true);
          toast.success(t('device.update.applyDone'));
          setStatus('done');
        } else {
          const message = res.message || t('device.update.applyFailed');
          toast.error(message);
          setError(message);
          setStatus('idle'); // allow an immediate retry without re-arming the gate
        }
      }
    } catch (e) {
      console.warn('[console.artifact.update_confirm]', target, e);
      const message = formatError(e);
      toast.error(message);
      setError(message);
      setStatus('idle'); // allow an immediate retry without re-arming the gate
    }
  };

  if (status === 'done') {
    return (
      <ArtifactShell icon={Check} title={title} advancedTo={advancedTo}>
        <p className="flex items-center gap-2 text-sm text-success">
          <Check className="size-4 shrink-0" aria-hidden="true" />
          {t('device.update.applyDone')}
        </p>
        {success && detail && <p className="break-words text-xs text-muted-foreground">{detail}</p>}
        {needsRestart && (
          <p
            role="status"
            className={cn(
              'flex items-center gap-1.5 rounded-lg border border-warning/30 bg-warning/10 px-2.5 py-1.5 text-xs text-warning',
            )}
          >
            <RotateCcw className="size-3.5 shrink-0" aria-hidden="true" />
            {t('console.artifact.updateConfirm.needsRestart')}
          </p>
        )}
      </ArtifactShell>
    );
  }

  if (status === 'cancelled') {
    return (
      <ArtifactShell icon={X} title={title}>
        <p className="text-sm text-muted-foreground">{t('console.artifact.confirmAction.cancelled')}</p>
      </ArtifactShell>
    );
  }

  return (
    <ArtifactShell icon={Download} title={title} advancedTo={advancedTo}>
      <div className="space-y-3">
        <p className="text-sm text-muted-foreground">{t(`console.artifact.updateConfirm.desc.${target}`)}</p>

        {error && (
          <p
            role="alert"
            className={cn(
              'rounded-lg border border-destructive/30 bg-destructive/10 px-2.5 py-1.5 text-xs text-destructive',
            )}
          >
            {error}
          </p>
        )}

        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={() => setStatus('cancelled')} disabled={status === 'busy'}>
            {t('common.cancel')}
          </Button>
          <Button variant="brand" size="sm" onClick={() => void runConfirm()} disabled={status === 'busy'}>
            {status === 'busy' ? t('common.saving') : t('console.artifact.updateConfirm.cta')}
          </Button>
        </div>
      </div>
    </ArtifactShell>
  );
}
