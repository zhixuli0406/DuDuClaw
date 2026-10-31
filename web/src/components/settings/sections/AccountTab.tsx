import { AccountMfa } from '@/components/mfa/AccountMfa';
import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useLocation, useNavigate } from 'react-router';
import { KeyRound } from 'lucide-react';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { useConnectionStore } from '@/stores/connection-store';
import { api, type EmailSetup } from '@/lib/api';
import { changeOwnEmail, emailChangeErrorId } from '@/lib/account-email';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  ErrorState,
  Input,
  SettingsSection,
  SettingsCard,
  SettingsRow,
} from '@/components/mds';
import { SettingRow } from './shared';

// ── Account tab — self-service email and password change ──────────────────────
// The single-owner (personal) edition hides the multi-user Users page, so this
// is the only place the sole admin can rotate their own dashboard password.
// Available in every edition; it only mutates the logged-in user's own account.

export function AccountTab() {
  const intl = useIntl();
  const user = useAuthStore((s) => s.user);
  // WP-F1 / D4-A: the Personal edition signs the owner in automatically over
  // loopback, which is also why the logout item is gone. Say so here in plain
  // words — the switch itself lives in `config.toml`, deliberately not in the
  // UI (turning it off from inside a session you got for free is a footgun:
  // you would need a password you have never set).
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  // WP-0 (bootstrap-admin recovery): `AuthGuard` force-redirected here because
  // this account still must change its password before doing anything else.
  // Say so, and once the change succeeds, force a fresh WS handshake (the
  // gateway re-reads the flag on every RPC, but this store's
  // `mustChangePassword` is only refreshed by the `connect` response) then
  // send the user back to wherever they were headed.
  const mustChangePassword = useConnectionStore((s) => s.mustChangePassword);
  const connectWithAuth = useConnectionStore((s) => s.connectWithAuth);
  const disconnectWs = useConnectionStore((s) => s.disconnect);
  const location = useLocation();
  const navigate = useNavigate();
  const [current, setCurrent] = useState('');
  const [next, setNext] = useState('');
  const [confirm, setConfirm] = useState('');
  const [saving, setSaving] = useState(false);
  // P05: a rejected password change used to leave nothing behind but a toast,
  // so "did my old password not match?" was unanswerable 7 seconds later. The
  // reason now stays under the field group until the next attempt.
  const [submitError, setSubmitError] = useState<unknown>(null);

  // Self-service email change. `users.me` says whether the current password
  // is needed (for the email AND the password forms): a Personal owner signed
  // in by local auto-login never chose one.
  const [emailSetup, setEmailSetup] = useState<EmailSetup | null>(null);
  const [newEmail, setNewEmail] = useState('');
  const [emailPassword, setEmailPassword] = useState('');
  const [emailSaving, setEmailSaving] = useState(false);
  const [emailError, setEmailError] = useState('');

  useEffect(() => {
    let active = true;
    api.users
      .me()
      .then((me) => {
        if (active) setEmailSetup(me.email_setup ?? { placeholder: false, password_required: true });
      })
      .catch(() => {
        if (active) setEmailSetup({ placeholder: false, password_required: true });
      });
    return () => {
      active = false;
    };
  }, []);

  const emailPasswordRequired = emailSetup?.password_required ?? true;

  const submitEmail = async () => {
    setEmailError('');
    setEmailSaving(true);
    try {
      await changeOwnEmail(newEmail, emailPasswordRequired ? emailPassword : undefined);
      toast.success(intl.formatMessage({ id: 'settings.account.email.changed' }));
      setNewEmail('');
      setEmailPassword('');
      setEmailSetup((prev) => (prev ? { ...prev, placeholder: false } : prev));
    } catch (e) {
      setEmailError(intl.formatMessage({ id: emailChangeErrorId(e) }));
    } finally {
      setEmailSaving(false);
    }
  };

  const submit = async () => {
    if (next.length < 8) {
      toast.error(intl.formatMessage({ id: 'settings.account.tooShort' }));
      return;
    }
    if (next !== confirm) {
      toast.error(intl.formatMessage({ id: 'settings.account.mismatch' }));
      return;
    }
    if (emailPasswordRequired && next === current) {
      toast.error(intl.formatMessage({ id: 'settings.account.sameAsOld' }));
      return;
    }
    setSaving(true);
    setSubmitError(null);
    try {
      await api.users.changePassword(emailPasswordRequired ? current : '', next);
      toast.success(intl.formatMessage({ id: 'settings.account.changed' }));
      setCurrent('');
      setNext('');
      setConfirm('');
      if (mustChangePassword) {
        // The gateway re-reads the flag on every RPC, but the client-side
        // `mustChangePassword` comes from the `connect` response. Force a
        // fresh handshake so the rest of the dashboard unlocks, then send
        // the user back only if the server actually
        // confirms the flag is gone (never navigate away and leave the
        // account still locked out with no explanation on screen).
        disconnectWs();
        await connectWithAuth(() => useAuthStore.getState().jwt ?? undefined);
        if (!useConnectionStore.getState().mustChangePassword) {
          const from = (location.state as { from?: string } | null)?.from;
          navigate(from && from !== location.pathname + location.search ? from : '/', { replace: true });
        }
      }
    } catch (e) {
      console.warn('[api]', e);
      setSubmitError(e);
      toast.error(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-8">
      <AccountMfa />
      {mustChangePassword && (
        <div
          role="status"
          className="flex items-start gap-2.5 rounded-lg border border-warning/30 bg-warning/5 px-3 py-2.5"
        >
          <KeyRound className="mt-0.5 size-4 shrink-0 text-warning" />
          <div className="min-w-0">
            <p className="text-sm font-medium text-foreground">
              {intl.formatMessage({ id: 'settings.account.mustChange.title' })}
            </p>
            <p className="mt-0.5 text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'settings.account.mustChange.desc' })}
            </p>
          </div>
        </div>
      )}
      <SettingsSection>
        <SettingsCard>
          <SettingRow
            label={intl.formatMessage({ id: 'settings.account.signedInAs' })}
            value={user ? `${user.display_name || user.email} (${user.email})` : '-'}
          />
        </SettingsCard>
        {isPersonal && (
          <p className="mt-2 text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'login.local.accountNote' })}
          </p>
        )}
      </SettingsSection>

      <SettingsSection description={intl.formatMessage({ id: 'settings.account.hint' })}>
        <SettingsCard>
          {emailPasswordRequired && (
            <SettingsRow label={intl.formatMessage({ id: 'settings.account.current' })} tier="text">
              <Input
                type="password"
                value={current}
                onChange={(e) => setCurrent(e.target.value)}
                autoComplete="current-password"
                aria-invalid={submitError != null}
                aria-describedby={submitError != null ? 'account-submit-error' : undefined}
              />
            </SettingsRow>
          )}
          <SettingsRow label={intl.formatMessage({ id: 'settings.account.new' })} tier="text">
            <Input
              type="password"
              value={next}
              onChange={(e) => setNext(e.target.value)}
              autoComplete="new-password"
            />
          </SettingsRow>
          <SettingsRow label={intl.formatMessage({ id: 'settings.account.confirm' })} tier="text">
            <Input
              type="password"
              value={confirm}
              onChange={(e) => setConfirm(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  void submit();
                }
              }}
              autoComplete="new-password"
            />
          </SettingsRow>
        </SettingsCard>
        {submitError != null && (
          <div id="account-submit-error" className="mt-2">
            <ErrorState
              variant="inline"
              error={submitError}
              title={intl.formatMessage({ id: 'errorState.manage.actionFailed' })}
            />
          </div>
        )}
      </SettingsSection>

      <div className="flex items-center justify-end">
        <Button
          variant="brand"
          size="sm"
          onClick={() => void submit()}
          disabled={saving || emailSetup === null || (emailPasswordRequired && !current) || !next || !confirm}
        >
          {saving
            ? intl.formatMessage({ id: 'common.saving' })
            : intl.formatMessage({ id: 'settings.account.submit' })}
        </Button>
      </div>

      <SettingsSection
        title={intl.formatMessage({ id: 'settings.account.email.title' })}
        description={intl.formatMessage({ id: 'settings.account.email.hint' })}
      >
        <SettingsCard>
          <SettingsRow label={intl.formatMessage({ id: 'settings.account.email.new' })} tier="text">
            <Input
              type="email"
              value={newEmail}
              onChange={(e) => setNewEmail(e.target.value)}
              autoComplete="email"
              placeholder="you@company.com"
              aria-invalid={emailError !== ''}
              aria-describedby={emailError ? 'account-email-error' : undefined}
            />
          </SettingsRow>
          {emailPasswordRequired && (
            <SettingsRow label={intl.formatMessage({ id: 'settings.account.current' })} tier="text">
              <Input
                type="password"
                value={emailPassword}
                onChange={(e) => setEmailPassword(e.target.value)}
                autoComplete="current-password"
              />
            </SettingsRow>
          )}
        </SettingsCard>
        {emailError && (
          <p id="account-email-error" role="alert" className="mt-2 text-sm text-destructive">
            {emailError}
          </p>
        )}
        <div className="mt-3 flex items-center justify-end">
          <Button
            variant="brand"
            size="sm"
            onClick={() => void submitEmail()}
            disabled={
              emailSetup === null ||
              emailSaving ||
              newEmail.trim() === '' ||
              (emailPasswordRequired && !emailPassword)
            }
          >
            {emailSaving
              ? intl.formatMessage({ id: 'common.saving' })
              : intl.formatMessage({ id: 'settings.account.email.submit' })}
          </Button>
        </div>
      </SettingsSection>
    </div>
  );
}
