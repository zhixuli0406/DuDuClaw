import { useId, useState } from 'react';
import { Input } from '@/components/mds';
import {
  isValidEmployeeLabel,
  myIsolationApi,
  type RestorableEmployee,
} from '@/lib/isolation-restore-api';
import { FactorProof } from './FactorProof';
import {
  ADOPTION_CARRIED,
  ADOPTION_NOT_CARRIED,
  ItemList,
  ProgressSteps,
  TwoLists,
  useRestoreT,
} from './restore-ui';

/**
 * 由新員工接手 (owner) — the employee's runtime is gone (ROOT_GONE), so it
 * cannot come back as it was; a new Personal employee can take over what
 * was kept. The owner picks the new name (default: the original), proves
 * the request with their strongest pre-tombstone factor, and an Admin
 * approves.
 */
export function AdoptFlow({ employee, onFiled }: { employee: RestorableEmployee; onFiled: () => void }) {
  const t = useRestoreT();
  const labelId = useId();
  const labelHintId = useId();
  const pending = employee.open_request?.kind === 'adoption';
  const [label, setLabel] = useState(employee.label);
  const [filed, setFiled] = useState(false);
  const valid = isValidEmployeeLabel(label);
  const steps = [
    t('restoreFlow.adopt.step.prove'),
    t('restoreFlow.adopt.step.send'),
    t('restoreFlow.adopt.step.admin'),
    t('restoreFlow.adopt.step.ready'),
  ];
  const current = filed || pending ? 2 : 0;

  return (
    <div className="space-y-4">
      <p className="text-sm">{t('restoreFlow.adopt.why')}</p>
      <ProgressSteps steps={steps} current={current} label={t('restoreFlow.adopt.progress')} />
      <TwoLists
        leftTitle={t('restoreFlow.adopt.carried')}
        left={<ItemList keys={ADOPTION_CARRIED} />}
        rightTitle={t('restoreFlow.adopt.notCarried')}
        right={<ItemList keys={ADOPTION_NOT_CARRIED} />}
      />
      {filed || pending ? (
        <p role="status" className="text-sm">{t('restoreFlow.adopt.waiting')}</p>
      ) : (
        <>
          <div className="grid gap-1.5">
            <label htmlFor={labelId} className="text-sm font-medium">{t('restoreFlow.adopt.newLabel')}</label>
            <Input
              id={labelId}
              value={label}
              maxLength={64}
              autoComplete="off"
              spellCheck={false}
              onChange={(e) => setLabel(e.target.value.trim().toLowerCase())}
              aria-invalid={!valid}
              aria-describedby={labelHintId}
              className="font-mono sm:w-72"
            />
            <p id={labelHintId} className={valid ? 'text-xs text-muted-foreground' : 'text-xs text-destructive'}>
              {t('myPersonal.new.labelRule')}
            </p>
          </div>
          <FactorProof
            employeeId={employee.employee_id}
            incarnation={employee.incarnation}
            forAdoption
            canSubmit={valid}
            submitLabel={t('restoreFlow.adopt.submit')}
            onSubmit={async (proof) => {
              await myIsolationApi.requestAdoption(
                employee.employee_id, employee.incarnation, proof, label !== employee.label ? label : undefined,
              );
              setFiled(true);
              onFiled();
            }}
          />
        </>
      )}
    </div>
  );
}
