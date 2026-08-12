import { useState } from 'react';
import { useIntl } from 'react-intl';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
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
