import { useEffect, useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { api, type EmailSetup } from '@/lib/api';
import { changeOwnEmail, emailChangeErrorId } from '@/lib/account-email';
import { useConnectionStore } from '@/stores/connection-store';
import { toast } from '@/lib/toast';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
} from '@/components/mds';
import { Field } from '@/components/onboarding';

const DISMISS_KEY = 'duduclaw.ownerEmailPrompt.dismissed';

function wasDismissed(): boolean {
  try {
    return localStorage.getItem(DISMISS_KEY) === '1';
  } catch {
    return false;
  }
}

function rememberDismissed() {
  try {
    localStorage.setItem(DISMISS_KEY, '1');
  } catch {
    // Storage unavailable: the prompt simply comes back next visit.
  }
}

/**
 * Asks the owner for a real email while the account is still on the
 * placeholder `admin@local`. That happens on a Personal install, where local
 * auto-login claims the account before the operator ever sees the first-run
 * form, and on installs set up before the form asked for an email. "Later"
 * hides it in this browser; account settings can change the email any time.
 * Mounted once in MainLayout.
 */
export function OwnerEmailPrompt() {
  const intl = useIntl();
  const connected = useConnectionStore((s) => s.state) === 'authenticated';
  // An account that must change its password first can only call the
  // password-change RPCs, so `users.change_email` would be refused. Wait until
  // the password is set; the forced reconnect after that clears the flag and
  // this effect runs again.
  const mustChangePassword = useConnectionStore((s) => s.mustChangePassword);
  const [setup, setSetup] = useState<EmailSetup | null>(null);
  const [open, setOpen] = useState(false);
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');

  useEffect(() => {
    if (!connected || mustChangePassword || setup || wasDismissed()) return;
    let active = true;
    api.users
      .me()
      .then((me) => {
        if (!active || !me.email_setup) return;
        setSetup(me.email_setup);
        if (me.email_setup.placeholder) setOpen(true);
      })
      .catch(() => {
        // Not worth an error toast: the prompt is optional.
      });
    return () => {
      active = false;
    };
  }, [connected, mustChangePassword, setup]);

  const later = () => {
    rememberDismissed();
    setOpen(false);
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setError('');
    setSaving(true);
    try {
      await changeOwnEmail(email, setup?.password_required ? password : undefined);
      setOpen(false);
      toast.success(intl.formatMessage({ id: 'settings.account.email.changed' }));
    } catch (err) {
      setError(intl.formatMessage({ id: emailChangeErrorId(err) }));
    } finally {
      setSaving(false);
    }
  };

  if (!setup?.placeholder || mustChangePassword) return null;

  return (
    <Dialog open={open} onOpenChange={(next) => (next ? setOpen(true) : later())}>
      <DialogContent>
        <form onSubmit={submit} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{intl.formatMessage({ id: 'ownerEmail.title' })}</DialogTitle>
            <DialogDescription>{intl.formatMessage({ id: 'ownerEmail.body' })}</DialogDescription>
          </DialogHeader>
          <Field label={intl.formatMessage({ id: 'login.claim.email' })} htmlFor="owner-email">
            <Input
              id="owner-email"
              type="email"
              autoComplete="email"
              required
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              placeholder="you@company.com"
            />
          </Field>
          {setup.password_required && (
            <Field label={intl.formatMessage({ id: 'settings.account.current' })} htmlFor="owner-email-pw">
              <Input
                id="owner-email-pw"
                type="password"
                autoComplete="current-password"
                required
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </Field>
          )}
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          <DialogFooter>
            <Button type="button" variant="ghost" size="sm" onClick={later}>
              {intl.formatMessage({ id: 'ownerEmail.later' })}
            </Button>
            <Button
              type="submit"
              variant="brand"
              size="sm"
              disabled={saving || email.trim() === '' || (setup.password_required && !password)}
            >
              {saving
                ? intl.formatMessage({ id: 'common.saving' })
                : intl.formatMessage({ id: 'ownerEmail.save' })}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
