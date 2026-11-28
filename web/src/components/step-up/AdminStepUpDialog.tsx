import { useEffect, useId, useRef, useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { Button, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, Input } from '@/components/mds';
import { stepUpApi, type StepUpToken } from '@/lib/step-up';

export interface AdminStepUpDialogProps {
  open: boolean;
  reason?: string;
  onDone: (token: StepUpToken | null) => void;
}

/** Direct verification; never reads the protected-mode employee list. */
export function AdminStepUpDialog({ open, reason, onDone }: AdminStepUpDialogProps) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [password, setPassword] = useState('');
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const generation = useRef(0);
  const passwordId = useId();
  const codeId = useId();
  const errorId = useId();

  useEffect(() => {
    setPassword('');
    setCode('');
    setFailed(false);
    setBusy(false);
    return () => { generation.current += 1; };
  }, [open]);

  const close = (token: StepUpToken | null) => {
    generation.current += 1;
    setPassword('');
    setCode('');
    setBusy(false);
    onDone(token);
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy || !password || !/^\d{6}$/.test(code)) return;
    const current = generation.current;
    setBusy(true);
    setFailed(false);
    try {
      const token = await stepUpApi.adminVerify(password, code);
      if (current === generation.current) close(token);
    } catch {
      if (current === generation.current) setFailed(true);
    } finally {
      // Closing or reopening invalidates this attempt; never alter a newer form.
      if (current === generation.current) {
        setPassword('');
        setCode('');
        setBusy(false);
      }
    }
  };

  return (
    <Dialog open={open} onOpenChange={(next) => { if (!next) close(null); }}>
      <DialogContent className="sm:max-w-md" showClose={false}>
        <DialogHeader>
          <DialogTitle>{t('adminStepUp.title')}</DialogTitle>
          <DialogDescription>{reason || t('adminStepUp.reason')}</DialogDescription>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">{t('adminStepUp.note')}</p>
        <form id={`${passwordId}-form`} className="grid gap-3" onSubmit={(event) => void submit(event)}>
          <div className="grid gap-1.5">
            <label htmlFor={passwordId}>{t('restoreFlow.stepUp.password')}</label>
            <Input id={passwordId} type="password" autoComplete="current-password" maxLength={4096}
              required disabled={busy} value={password} onChange={(event) => setPassword(event.target.value)}
              aria-invalid={failed} aria-describedby={failed ? errorId : undefined} />
          </div>
          <div className="grid gap-1.5">
            <label htmlFor={codeId}>{t('restoreFlow.stepUp.code')}</label>
            <Input id={codeId} inputMode="numeric" autoComplete="one-time-code" maxLength={6}
              required disabled={busy} value={code} onChange={(event) => setCode(event.target.value.replace(/\D/g, '').slice(0, 6))}
              aria-invalid={failed} aria-describedby={failed ? errorId : undefined} />
            <p className="text-xs text-muted-foreground">{t('restoreFlow.stepUp.codeHint')}</p>
          </div>
          {failed && <p id={errorId} role="alert" className="text-sm text-destructive">{t('restoreFlow.stepUp.failed')}</p>}
        </form>
        <DialogFooter>
          <Button variant="outline" onClick={() => close(null)}>{t('common.cancel')}</Button>
          <Button type="submit" form={`${passwordId}-form`} disabled={busy || !password || code.length !== 6}>
            {t(busy ? 'restoreFlow.stepUp.verifying' : 'restoreFlow.stepUp.verify')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
