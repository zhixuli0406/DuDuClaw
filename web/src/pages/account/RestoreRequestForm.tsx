import { useState } from 'react';
import { CheckCircle2 } from 'lucide-react';
import { myIsolationApi, type RestorableEmployee } from '@/lib/isolation-restore-api';
import { FactorProof } from '@/pages/protected/restore/FactorProof';
import {
  ADMIN_RESETUP_ITEMS,
  ItemList,
  RESTORED_ITEMS,
  TwoLists,
  useRestoreT,
} from '@/pages/protected/restore/restore-ui';

/**
 * 申請還原 — what comes back, what an Admin must set up again, that work
 * stays paused, then the owner's strongest-factor proof.
 */
export function RestoreRequestForm({ employee, onFiled }: { employee: RestorableEmployee; onFiled: () => void }) {
  const t = useRestoreT();
  const [filed, setFiled] = useState(false);
  const name = employee.display_name || employee.label;

  if (filed) {
    return (
      <p role="status" className="flex items-start gap-2 text-sm">
        <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-success" aria-hidden="true" />
        {t('myPersonal.restore.filed', { name })}
      </p>
    );
  }

  return (
    <div className="space-y-4">
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
          await myIsolationApi.requestRestore(employee.employee_id, employee.incarnation, proof);
          setFiled(true);
          onFiled();
        }}
      />
    </div>
  );
}
