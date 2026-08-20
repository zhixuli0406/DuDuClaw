import { useState } from 'react';
import { useIntl } from 'react-intl';
import { Download, RotateCcw } from 'lucide-react';
import { Badge, Button } from '@/components/mds';
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
 * "回到上一版" mirrors DevicePage exactly: always rendered disabled with a
 * "即將推出" caption, because `device.update_rollback` always answers
 * `unsupported` this round (no verified appliance A/B boot-selection
 * mechanism yet) — the card must not pretend the action works. It only
 * appears alongside the device-result section (there is nothing to roll back
 * on a system-only card).
 */
export function UpdateStatusCard({ payload }: { payload: UpdateStatusArtifact['payload'] }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [showDetails, setShowDetails] = useState(false);

  const { action, result, system } = payload;
  const title = t(action === 'apply' ? 'console.artifact.updateStatus.title.apply' : 'console.artifact.updateStatus.title.check');
  const details = result ? [result.stdout, result.stderr].filter(Boolean).join('\n') : '';
  const systemError = system && 'error' in system ? system.error : null;
  const systemInfo = system && !('error' in system) ? system : null;

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
              <Button variant="outline" size="sm" disabled title={t('device.update.rollback.comingSoon')}>
                <RotateCcw />
                {t('device.update.rollback')}
              </Button>
            </div>
            <p className="text-xs text-muted-foreground">{t('device.update.rollback.comingSoon')}</p>

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
