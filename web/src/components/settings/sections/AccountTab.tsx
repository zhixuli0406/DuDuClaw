import { useState } from 'react';
import { useIntl } from 'react-intl';
import { useLocation, useNavigate } from 'react-router';
import { KeyRound } from 'lucide-react';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { useConnectionStore } from '@/stores/connection-store';
import { api } from '@/lib/api';
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

// ── Account tab — self-service password change ────────────────────────────────
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
  // gateway bakes `must_change_password` into the connection's UserContext
  // once at `connect` time — a plain RPC success does NOT refresh it on the
  // live socket) then send the user back to wherever they were headed.
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

  const submit = async () => {
    if (next.length < 8) {
      toast.error(intl.formatMessage({ id: 'settings.account.tooShort' }));
      return;
    }
    if (next !== confirm) {
      toast.error(intl.formatMessage({ id: 'settings.account.mismatch' }));
      return;
    }
    if (next === current) {
      toast.error(intl.formatMessage({ id: 'settings.account.sameAsOld' }));
      return;
    }
    setSaving(true);
    setSubmitError(null);
    try {
      await api.users.changePassword(current, next);
      toast.success(intl.formatMessage({ id: 'settings.account.changed' }));
      setCurrent('');
      setNext('');
      setConfirm('');
      if (mustChangePassword) {
        // The gateway bakes `must_change_password` into this connection's
        // UserContext once at handshake time — a successful RPC alone never
        // refreshes it. Force a fresh handshake so the rest of the dashboard
        // unlocks, then send the user back only if the server actually
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
          disabled={saving || !current || !next || !confirm}
        >
          {saving
            ? intl.formatMessage({ id: 'common.saving' })
            : intl.formatMessage({ id: 'settings.account.submit' })}
        </Button>
      </div>
    </div>
  );
}
