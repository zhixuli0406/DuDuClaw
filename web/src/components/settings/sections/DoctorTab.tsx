import { useState } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useSystemStore } from '@/stores/system-store';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Button, Empty, ErrorState } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';
import { Play, Wrench, CheckCircle, AlertTriangle, XCircle, Stethoscope } from 'lucide-react';

export function DoctorTab() {
  const intl = useIntl();
  // P05: when `system.doctor` itself throws, the store keeps `doctorChecks`
  // empty and records the reason in `error` — which this tab never read, so a
  // failed health check rendered as the "press 執行 to run" empty state. The
  // store stores an i18n key, not a message.
  const { doctorChecks, runDoctor, loading, error } = useSystemStore();
  const [repairError, setRepairError] = useState<unknown>(null);
  // "一鍵修復" used to fire with zero confirmation and no indication of what it
  // would actually change on the system (phase4 audit C06/C11 Blocker) — gate
  // it behind the shared ConfirmDialog with a preview of the repairable checks.
  const [confirmRepair, setConfirmRepair] = useState(false);

  const statusIcon: Record<string, React.ReactNode> = {
    pass: <CheckCircle className="h-5 w-5 text-success" />,
    warn: <AlertTriangle className="h-5 w-5 text-warning" />,
    fail: <XCircle className="h-5 w-5 text-destructive" />,
  };

  const statusBg: Record<string, string> = {
    pass: 'border-success/30 bg-success/10',
    warn: 'border-warning/30 bg-warning/10',
    fail: 'border-destructive/30 bg-destructive/10',
  };

  const repairableChecks = doctorChecks.filter((c) => c.can_repair);

  const handleRepair = async () => {
    setRepairError(null);
    try {
      await api.system.doctorRepair();
      await runDoctor();
    } catch (e) {
      setRepairError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  };

  const runFailed = error != null && doctorChecks.length === 0 && !loading;

  return (
    <div className="space-y-6">
      <div className="flex gap-2">
        <Button variant="brand" size="sm" onClick={runDoctor} disabled={loading}>
          <Play />
          {intl.formatMessage({ id: 'settings.doctor.run' })}
        </Button>
        <Button variant="secondary" size="sm" onClick={() => setConfirmRepair(true)} disabled={loading}>
          <Wrench />
          {intl.formatMessage({ id: 'settings.doctor.repair' })}
        </Button>
      </div>

      {repairError != null && (
        <ErrorState
          variant="inline"
          error={repairError}
          title={intl.formatMessage({ id: 'errorState.manage.actionFailed' })}
          onRetry={() => void handleRepair()}
        />
      )}

      {runFailed ? (
        <ErrorState
          title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
          description={
            error && intl.messages[error]
              ? intl.formatMessage({ id: error })
              : intl.formatMessage({ id: 'errorState.manage.notEmptyHint' })
          }
          onRetry={() => void runDoctor()}
        />
      ) : doctorChecks.length === 0 ? (
        <Empty
          icon={Stethoscope}
          variant="dashed"
          title={intl.formatMessage({ id: 'settings.doctor.run' })}
        />
      ) : (
        <div className="grid gap-3 sm:grid-cols-2">
          {doctorChecks.map((check) => (
            <div
              key={check.name}
              className={cn(
                'rounded-xl border p-5',
                statusBg[check.status] ?? 'border-surface-border bg-surface'
              )}
            >
              <div className="flex items-start gap-3">
                {statusIcon[check.status]}
                <div className="flex-1">
                  <h4 className="font-medium text-foreground">
                    {/* Translated title when a key exists; raw name as fallback
                        so unknown backend checks still render. */}
                    {intl.messages[`settings.doctor.check.${check.name}`]
                      ? intl.formatMessage({ id: `settings.doctor.check.${check.name}` })
                      : check.name}
                  </h4>
                  <p className="mt-1 text-sm text-muted-foreground">
                    {check.message}
                  </p>
                  {check.can_repair && check.repair_hint && (
                    <p className="mt-2 text-xs text-warning">
                      {check.repair_hint}
                    </p>
                  )}
                </div>
              </div>
            </div>
          ))}
        </div>
      )}

      <ConfirmDialog
        open={confirmRepair}
        onClose={() => setConfirmRepair(false)}
        onConfirm={() => { setConfirmRepair(false); void handleRepair(); }}
        title={intl.formatMessage({ id: 'settings.doctor.repair' })}
        message={
          repairableChecks.length > 0
            ? intl.formatMessage(
                { id: 'confirm.settings.doctorRepair.messageWithChecks' },
                {
                  checks: repairableChecks
                    .map((c) =>
                      intl.messages[`settings.doctor.check.${c.name}`]
                        ? intl.formatMessage({ id: `settings.doctor.check.${c.name}` })
                        : c.name,
                    )
                    .join(', '),
                },
              )
            : intl.formatMessage({ id: 'confirm.settings.doctorRepair.messageGeneric' })
        }
        confirmLabel={intl.formatMessage({ id: 'settings.doctor.repair' })}
      />
    </div>
  );
}
