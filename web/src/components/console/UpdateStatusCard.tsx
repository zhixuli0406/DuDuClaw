import { useState } from 'react';
import { useIntl } from 'react-intl';
import { Download, RotateCcw } from 'lucide-react';
import { Badge, Button } from '@/components/mds';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';
import type { UpdateStatusArtifact } from './artifact-types';
import { ArtifactShell } from './ArtifactShell';

/**
 * 系統更新卡 — the conversational twin of DevicePage's ②更新中心. Shows the
 * result of a `device.update_status` / `device.update_apply` call the agent
 * already made — this card never triggers the check/apply itself (that's
 * O-1's job: routing "檢查更新" to the `os_*` tool and attaching the result
 * here), it only reports what happened.
 *
 * `result` (the appliance OS image half) is OPTIONAL (P5): on a non-appliance
 * install `os_check_update` never has a `DeviceOpResult` to report, only the
 * `system` half (DuDuClaw's own version). This card renders whichever
 * half(s) `payload` actually carries — the device-result section (badge +
 * rollback button + details) entirely when `result` is absent, the `system`
 * section entirely when `system` is absent — and never fabricates a value
 * for either. Both absent is a backend contract violation that should never
 * happen (`os_operator.rs`'s `readonly_result_to_artifact` fails closed to no
 * artifact at all in that case) — the card falls back to just the title
 * shell rather than crashing.
 *
 * "回到上一版" is now wired to a real `device.update_rollback` call — unlike
 * check/apply, THIS card triggers the RPC directly (there is no dedicated
 * O-1 confirm artifact for rollback), because it is the only place a
 * completed update result is on screen for the user to react to. Clicking it
 * reveals an inline confirm step (this card lives inside a chat bubble —
 * small and scrollable — so it follows the same inline idle→confirm→busy
 * rhythm as `ConfirmActionCard`/`UpdateConfirmCard`, rather than DevicePage's
 * modal `ConfirmDialog`) with the exact same copy DevicePage uses. The
 * response needs THREE-way handling, not a plain success/failure binary —
 * see `runRollback` below for why. It only appears alongside the
 * device-result section (there is nothing to roll back on a system-only
 * card).
 */
export function UpdateStatusCard({ payload }: { payload: UpdateStatusArtifact['payload'] }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [showDetails, setShowDetails] = useState(false);
  const [rollbackConfirming, setRollbackConfirming] = useState(false);
  const [rollbackBusy, setRollbackBusy] = useState(false);
  const [rollbackNotice, setRollbackNotice] = useState<{ tone: 'success' | 'info' | 'error'; message: string } | null>(
    null,
  );

  const { action, result, system } = payload;
  const title = t(action === 'apply' ? 'console.artifact.updateStatus.title.apply' : 'console.artifact.updateStatus.title.check');
  const details = result ? [result.stdout, result.stderr].filter(Boolean).join('\n') : '';
  const systemError = system && 'error' in system ? system.error : null;
  const systemInfo = system && !('error' in system) ? system : null;

  // Three-way response handling — mirrors DevicePage's `runUpdateRollback`
  // exactly, see its doc comment for the full reasoning:
  //   1. `{ success: true }` → the box is about to reboot; toast says
  //      "scheduled", never "done" (this tab loses the connection).
  //   2. Rejects with `code: 'unsupported'` → an honest refusal. The
  //      gateway's own zh-TW reason (`error.message`) is shown verbatim,
  //      never replaced by a generic clause.
  //   3. Anything else → the ordinary `formatError()` toast.
  const runRollback = async () => {
    setRollbackBusy(true);
    try {
      const res = await api.device.updateRollback();
      if (res.success) {
        toast.success(t('device.update.rollback.scheduled'));
        setRollbackNotice({ tone: 'success', message: t('device.update.rollback.scheduled') });
        setRollbackConfirming(false);
      } else {
        toast.error(t('device.update.rollback.failed'));
        setRollbackNotice({ tone: 'error', message: t('device.update.rollback.failed') });
      }
    } catch (e) {
      console.warn('[console.artifact.updateStatus.rollback]', e);
      const code =
        e && typeof e === 'object' && 'code' in e && typeof (e as { code?: unknown }).code === 'string'
          ? (e as { code: string }).code
          : undefined;
      if (code === 'unsupported') {
        const backendMessage =
          e && typeof e === 'object' && 'message' in e && typeof (e as { message?: unknown }).message === 'string'
            ? (e as { message: string }).message
            : undefined;
        const msg = backendMessage ?? t('device.update.rollback.unsupportedFallback');
        toast.info(msg);
        setRollbackNotice({ tone: 'info', message: msg });
      } else {
        const msg = formatError(e);
        toast.error(msg);
        setRollbackNotice({ tone: 'error', message: msg });
      }
    } finally {
      setRollbackBusy(false);
    }
  };

  return (
    <ArtifactShell icon={Download} title={title} advancedTo="/device">
      <div className="space-y-2.5">
        {/* `system` half — DuDuClaw's own version (as opposed to `result`,
            which is always the appliance OS image half). Optional: renders
            nothing when absent, never fabricated. */}
        {systemInfo && (
          <div className="space-y-1 rounded-lg border border-surface-border bg-muted/30 p-2.5">
            <div className="flex items-center justify-between gap-2">
              <span className="text-xs text-muted-foreground">{t('console.artifact.updateStatus.system.label')}</span>
              <Badge
                variant="secondary"
                className={systemInfo.available ? 'bg-warning/15 text-warning' : 'bg-success/15 text-success'}
              >
                {t(systemInfo.available ? 'console.artifact.updateStatus.system.available' : 'console.artifact.updateStatus.system.upToDate')}
              </Badge>
            </div>
            <p className="text-xs text-muted-foreground">
              {t('console.artifact.updateStatus.system.version', {
                current: systemInfo.current_version,
                latest: systemInfo.latest_version,
              })}
            </p>
          </div>
        )}
        {systemError && (
          <p className="text-xs text-muted-foreground">
            {t('console.artifact.updateStatus.system.checkFailed', { error: systemError })}
          </p>
        )}

        {/* `result` half — the appliance OS image half. Optional (P5): a
            non-appliance install's payload never carries it, so this whole
            section (badge, rollback button, details toggle) is omitted
            rather than rendered against a fabricated result. */}
        {result && (
          <>
            <div className="flex items-center gap-2">
              <Badge
                variant="secondary"
                className={result.success ? 'bg-success/15 text-success' : 'bg-destructive/15 text-destructive'}
              >
                {t(result.success ? 'device.update.applyDone' : 'device.update.applyFailed')}
              </Badge>
              {!rollbackConfirming && (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    setRollbackConfirming(true);
                    setRollbackNotice(null);
                  }}
                >
                  <RotateCcw />
                  {t('device.update.rollback')}
                </Button>
              )}
            </div>

            {rollbackNotice && (
              <p
                role={rollbackNotice.tone === 'error' ? 'alert' : 'status'}
                className={cn(
                  'rounded-lg border px-2.5 py-1.5 text-xs',
                  rollbackNotice.tone === 'error'
                    ? 'border-destructive/30 bg-destructive/10 text-destructive'
                    : rollbackNotice.tone === 'success'
                      ? 'border-success/30 bg-success/10 text-success'
                      : 'border-brand/30 bg-brand/10 text-brand',
                )}
              >
                {rollbackNotice.message}
              </p>
            )}

            {rollbackConfirming && (
              <div className="space-y-2 rounded-lg border border-warning/30 bg-warning/10 p-2.5">
                <p className="text-xs text-warning">{t('device.update.rollback.confirmMessage')}</p>
                <div className="flex gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => {
                      setRollbackConfirming(false);
                      setRollbackNotice(null);
                    }}
                    disabled={rollbackBusy}
                  >
                    {t('common.cancel')}
                  </Button>
                  <Button variant="destructive" size="sm" onClick={() => void runRollback()} disabled={rollbackBusy}>
                    {rollbackBusy ? t('common.saving') : t('device.update.rollback')}
                  </Button>
                </div>
              </div>
            )}

            {details && (
              <div className="space-y-1.5">
                <button
                  type="button"
                  onClick={() => setShowDetails((v) => !v)}
                  className="text-xs font-medium text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
                >
                  {t(showDetails ? 'device.update.details.hide' : 'device.update.details')}
                </button>
                {showDetails && (
                  <pre className={cn('max-h-40 overflow-auto rounded-lg border border-surface-border bg-muted/40 p-2 text-xs whitespace-pre-wrap text-muted-foreground')}>
                    {details}
                  </pre>
                )}
              </div>
            )}
          </>
        )}
      </div>
    </ArtifactShell>
  );
}
