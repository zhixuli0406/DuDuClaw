import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useSystemStore } from '@/stores/system-store';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Button, Empty } from '@/components/mds';
import { Play, Wrench, CheckCircle, AlertTriangle, XCircle, Stethoscope } from 'lucide-react';

export function DoctorTab() {
  const intl = useIntl();
  const { doctorChecks, runDoctor, loading } = useSystemStore();

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

  const handleRepair = async () => {
    try {
      await api.system.doctorRepair();
      await runDoctor();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  };

  return (
    <div className="space-y-6">
      <div className="flex gap-2">
        <Button variant="brand" size="sm" onClick={runDoctor} disabled={loading}>
          <Play />
          {intl.formatMessage({ id: 'settings.doctor.run' })}
        </Button>
        <Button variant="secondary" size="sm" onClick={handleRepair} disabled={loading}>
          <Wrench />
          {intl.formatMessage({ id: 'settings.doctor.repair' })}
        </Button>
      </div>

      {doctorChecks.length === 0 ? (
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
    </div>
  );
}
