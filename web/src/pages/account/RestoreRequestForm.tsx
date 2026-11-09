import { useState } from 'react';
import { useIntl } from 'react-intl';
import { CheckCircle2 } from 'lucide-react';
import { myIsolationApi, toMillis, type RestorableEmployee, type RestoreFiled } from '@/lib/isolation-restore-api';
import { FactorProof } from '@/pages/protected/restore/FactorProof';
import {
  ADMIN_RESETUP_ITEMS,
  ItemList,
  RESTORED_ITEMS,
  TwoLists,
  useRestoreT,
} from '@/pages/protected/restore/restore-ui';

/**
 * 申請還原「…」 (board "Mine") — what comes back, what an Admin must set up
 * again, that work stays paused, then the owner's strongest-factor proof
 * (用兩步驟驗證確認並送出). The reply (`RestoreFiled`) says until when an
 * Admin can decide and whether Admins were told.
 */
export function RestoreRequestForm({ employee, onFiled }: { employee: RestorableEmployee; onFiled: () => void }) {
  const t = useRestoreT();
  const intl = useIntl();
  const [filed, setFiled] = useState<RestoreFiled | null>(null);
  const name = employee.display_name || employee.label;

  if (filed) {
    const expiresMs = toMillis(filed.expires_at ?? null);
    return (
      <div role="status" className="space-y-1 text-sm">
        <p className="flex items-start gap-2">
          <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-success" aria-hidden="true" />
          {t('myPersonal.restore.filed', { name })}
        </p>
        {expiresMs != null && (
          <p className="text-muted-foreground">
            {t('myPersonal.restore.expires', { time: intl.formatDate(expiresMs, { dateStyle: 'medium', timeStyle: 'short' }) })}
          </p>
        )}
        {filed.notified !== undefined && filed.notified !== false && filed.notified !== 0 && (
          <p className="text-muted-foreground">{t('myPersonal.restore.notified')}</p>
        )}
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <h4 className="text-sm font-medium">{t('myPersonal.restore.title', { name })}</h4>
      <TwoLists
        leftTitle={t('myPersonal.restore.willRestore')}
        left={<ItemList keys={RESTORED_ITEMS} />}
        rightTitle={t('myPersonal.restore.adminResetup')}
        right={<ItemList keys={ADMIN_RESETUP_ITEMS} />}
      />
      <p className="text-sm">{t('myPersonal.restore.workPaused')}</p>
      <p className="text-sm text-muted-foreground">{t('myPersonal.restore.credentialBlock')}</p>
      <FactorProof
        employeeId={employee.employee_id}
        incarnation={employee.incarnation}
        submitLabel={t('myPersonal.restore.submit')}
        onSubmit={async (proof) => {
          const reply = await myIsolationApi.requestRestore(employee.employee_id, employee.incarnation, proof);
          setFiled(reply ?? { request_id: '', state: 'pending' });
          onFiled();
        }}
      />
    </div>
  );
}
