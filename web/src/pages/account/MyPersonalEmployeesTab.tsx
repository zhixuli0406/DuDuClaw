import { useCallback, useEffect, useState } from 'react';
import { myIsolationApi, type PersonalEmployeeRequest } from '@/lib/isolation-restore-api';
import { RestoreErrorNotice, useRestoreT } from '@/pages/protected/restore/restore-ui';
import { DelegationRequestsCard } from './DelegationRequestsCard';
import { MyPersonalEmployeeCard } from './MyPersonalEmployeeCard';
import { NewPersonalEmployeeForm } from './NewPersonalEmployeeForm';
import { TombstonedEmployeesCard } from './TombstonedEmployeesCard';

/** Shown on the account page as a tab since v1.81 (protected mode only). */
export const MY_PERSONAL_TAB = { id: 'personal-employees', newIn: '1.81.0', labelId: 'myPersonal.tab' } as const;

/**
 * 我的帳號 › 我的個人 AI 員工 (v1.81 F5/F6e-UI, board "Mine").
 */
export function MyPersonalEmployeesTab() {
  const t = useRestoreT();
  const [requests, setRequests] = useState<PersonalEmployeeRequest[] | null>(null);
  const [error, setError] = useState<unknown>(null);

  const load = useCallback(async () => {
    try {
      const res = await myIsolationApi.listPersonalEmployeeRequests();
      setRequests(Array.isArray(res?.requests) ? res.requests : []);
      setError(null);
    } catch (e) {
      setRequests([]);
      setError(e);
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-xl font-semibold">{t('myPersonal.title')}</h2>
        <p className="mt-1 text-sm text-muted-foreground">{t('myPersonal.intro')}</p>
      </div>
      <RestoreErrorNotice error={error} />
      {error == null && <MyPersonalEmployeeCard requests={requests} />}
      <DelegationRequestsCard />
      <TombstonedEmployeesCard />
      <NewPersonalEmployeeForm requests={requests} onFiled={() => void load()} />
    </div>
  );
}
