import { useId, useState, type FormEvent } from 'react';
import { Badge, Button, Card, CardContent, CardDescription, CardHeader, CardTitle, Input } from '@/components/mds';
import {
  MAX_OPEN_PERSONAL_REQUESTS,
  isValidEmployeeLabel,
  myIsolationApi,
  type PersonalEmployeeRequest,
} from '@/lib/isolation-restore-api';
import { toast } from '@/lib/toast';
import { RestoreErrorNotice, useRestoreT } from '@/pages/protected/restore/restore-ui';

const OPEN = new Set(['pending', 'applying']);

export function openRequestCount(requests: PersonalEmployeeRequest[] | null): number {
  return requests?.filter((r) => OPEN.has(r.status)).length ?? 0;
}

/**
 * 申請新的個人 AI 員工 — name rules, display name, Admin approval, at most
 * three open requests at a time. The server re-checks every rule.
 */
export function NewPersonalEmployeeForm({
  requests,
  onFiled,
}: {
  requests: PersonalEmployeeRequest[] | null;
  onFiled: () => void;
}) {
  const t = useRestoreT();
  const labelId = useId();
  const labelHint = useId();
  const displayId = useId();
  const displayHint = useId();
  const [label, setLabel] = useState('');
  const [display, setDisplay] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const open = openRequestCount(requests);
  const full = open >= MAX_OPEN_PERSONAL_REQUESTS;
  const labelOk = isValidEmployeeLabel(label);
  const displayTrim = display.trim();
  // Control characters are refused by the server too.
  // eslint-disable-next-line no-control-regex
  const displayOk = displayTrim.length > 0 && Array.from(displayTrim).length <= 64 && !/[\u0000-\u001f\u007f]/.test(displayTrim);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy || full || !labelOk || !displayOk) return;
    setBusy(true);
    setError(null);
    try {
      await myIsolationApi.requestPersonalEmployee(label, displayTrim);
      toast.success(t('myPersonal.new.sent'));
      setLabel('');
      setDisplay('');
      onFiled();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('myPersonal.new.title')}</CardTitle>
        <CardDescription>{t('myPersonal.new.desc')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <form className="grid gap-3" onSubmit={(e) => void submit(e)}>
          <div className="grid gap-1.5">
            <label htmlFor={labelId} className="text-sm font-medium">{t('myPersonal.new.label')}</label>
            <Input
              id={labelId}
              value={label}
              maxLength={64}
              autoComplete="off"
              spellCheck={false}
              placeholder="alice-assistant"
              onChange={(e) => setLabel(e.target.value.trim().toLowerCase())}
              aria-invalid={label !== '' && !labelOk}
              aria-describedby={labelHint}
              className="font-mono sm:w-72"
            />
            <p id={labelHint} className={label !== '' && !labelOk ? 'text-xs text-destructive' : 'text-xs text-muted-foreground'}>
              {t('myPersonal.new.labelRule')}
            </p>
          </div>
          <div className="grid gap-1.5">
            <label htmlFor={displayId} className="text-sm font-medium">{t('myPersonal.new.display')}</label>
            <Input
              id={displayId}
              value={display}
              maxLength={128}
              onChange={(e) => setDisplay(e.target.value)}
              aria-invalid={display !== '' && !displayOk}
              aria-describedby={displayHint}
              className="sm:w-72"
            />
            <p id={displayHint} className="text-xs text-muted-foreground">{t('myPersonal.new.displayRule')}</p>
          </div>
          <p className="text-sm text-muted-foreground">{t('myPersonal.new.approval', { max: MAX_OPEN_PERSONAL_REQUESTS })}</p>
          {full && <p role="status" className="text-sm">{t('myPersonal.new.full', { max: MAX_OPEN_PERSONAL_REQUESTS })}</p>}
          <RestoreErrorNotice error={error} />
          <div>
            <Button type="submit" variant="brand" disabled={busy || full || !labelOk || !displayOk}>
              {busy ? t('restoreFlow.common.sending') : t('myPersonal.new.submit')}
            </Button>
          </div>
        </form>

        {requests && requests.length > 0 && (
          <section aria-labelledby={`${labelId}-mine`} className="space-y-2">
            <h3 id={`${labelId}-mine`} className="text-sm font-medium">
              {t('myPersonal.new.mine')}{' '}
              <span className="font-mono text-xs tabular-nums text-muted-foreground">{open}/{MAX_OPEN_PERSONAL_REQUESTS}</span>
            </h3>
            <ul className="divide-y divide-surface-border rounded-lg border border-surface-border">
              {requests.map((r) => (
                <li key={r.request_id} className="flex flex-wrap items-center justify-between gap-2 px-3 py-2 text-sm">
                  <span className="min-w-0">
                    <span className="block truncate" title={r.display_name}>{r.display_name}</span>
                    <span className="font-mono text-xs text-muted-foreground">{r.label}</span>
                  </span>
                  <Badge variant={r.status === 'applied' ? 'secondary' : r.status === 'rejected' || r.status === 'failed' ? 'destructive' : 'outline'}>
                    {t(`myPersonal.request.status.${r.status}`)}
                  </Badge>
                </li>
              ))}
            </ul>
          </section>
        )}
      </CardContent>
    </Card>
  );
}
