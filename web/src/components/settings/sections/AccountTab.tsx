import { useState } from 'react';
import { useIntl } from 'react-intl';
import { useAuthStore } from '@/stores/auth-store';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
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
  const [current, setCurrent] = useState('');
  const [next, setNext] = useState('');
  const [confirm, setConfirm] = useState('');
  const [saving, setSaving] = useState(false);

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
    try {
      await api.users.changePassword(current, next);
      toast.success(intl.formatMessage({ id: 'settings.account.changed' }));
      setCurrent('');
      setNext('');
      setConfirm('');
    } catch (e) {
      console.warn('[api]', e);
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
      </SettingsSection>

      <SettingsSection description={intl.formatMessage({ id: 'settings.account.hint' })}>
        <SettingsCard>
          <SettingsRow label={intl.formatMessage({ id: 'settings.account.current' })} tier="text">
            <Input
              type="password"
              value={current}
              onChange={(e) => setCurrent(e.target.value)}
              autoComplete="current-password"
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
