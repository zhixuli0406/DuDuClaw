import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useSystemStore } from '@/stores/system-store';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Card, Button, EmptyState } from '@/components/ui';
import { Play, Wrench, CheckCircle, AlertTriangle, XCircle, Stethoscope } from 'lucide-react';

export function DoctorTab() {
  const intl = useIntl();
  const { doctorChecks, runDoctor, loading } = useSystemStore();

  const statusIcon: Record<string, React.ReactNode> = {
    pass: <CheckCircle className="h-5 w-5 text-emerald-500" />,
    warn: <AlertTriangle className="h-5 w-5 text-amber-500" />,
    fail: <XCircle className="h-5 w-5 text-rose-500" />,
  };

  const statusBg: Record<string, string> = {
    pass: 'border-emerald-200 bg-emerald-50 dark:border-emerald-800 dark:bg-emerald-900/20',
    warn: 'border-amber-200 bg-amber-50 dark:border-amber-800 dark:bg-amber-900/20',
    fail: 'border-rose-200 bg-rose-50 dark:border-rose-800 dark:bg-rose-900/20',
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
      <p className="rounded-lg bg-stone-500/5 px-4 py-3 text-sm text-stone-500 dark:bg-white/5 dark:text-stone-400">
        {intl.formatMessage({ id: 'settings.doctor.desc' })}
      </p>
      <div className="flex gap-2">
        <Button variant="primary" icon={Play} onClick={runDoctor} disabled={loading}>
          {intl.formatMessage({ id: 'settings.doctor.run' })}
        </Button>
        <Button variant="secondary" icon={Wrench} onClick={handleRepair} disabled={loading}>
          {intl.formatMessage({ id: 'settings.doctor.repair' })}
        </Button>
      </div>

      {doctorChecks.length === 0 ? (
        <Card padded={false}>
          <EmptyState
            icon={Stethoscope}
            dudu="idle"
            title={intl.formatMessage({ id: 'settings.doctor.run' })}
          />
        </Card>
      ) : (
        <div className="grid gap-3 sm:grid-cols-2">
          {doctorChecks.map((check) => (
            <div
              key={check.name}
              className={cn(
                'rounded-xl border p-5',
                statusBg[check.status] ?? 'border-stone-200 bg-white'
              )}
            >
              <div className="flex items-start gap-3">
                {statusIcon[check.status]}
                <div className="flex-1">
                  <h4 className="font-semibold text-stone-900 dark:text-stone-50">
                    {check.name}
                  </h4>
                  <p className="mt-1 text-sm text-stone-600 dark:text-stone-400">
                    {check.message}
                  </p>
                  {check.can_repair && check.repair_hint && (
                    <p className="mt-2 text-xs text-amber-600 dark:text-amber-400">
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
