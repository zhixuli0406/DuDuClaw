import { useState } from 'react';
import { useIntl } from 'react-intl';
import { Button } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';
import { sessionAdminApi } from '@/lib/session-admin-api';
import { accountRequest, mfaErrorId } from '@/lib/mfa-api';
import { useAuthStore } from '@/stores/auth-store';

export function RevokeUserSessions({ userId, name }: { userId: string; name: string }) {
  const actor = useAuthStore(s => s.user);
  return actor?.role === 'admin' && userId ? <RevokeButton key={`${actor.id}:${userId}`} userId={userId} name={name} /> : null;
}

function RevokeButton({ userId, name }: { userId: string; name: string }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [done, setDone] = useState(false);
  const revoke = async () => {
    if (busy || useAuthStore.getState().user?.role !== 'admin') return;
    const attempt = useAuthStore.getState().authAttempt;
    const self = userId === useAuthStore.getState().user?.id;
    setBusy(true); setError(''); setDone(false);
    try {
      // Self-revocation invalidates the WebSocket before it can carry its reply.
      const revoked = self
        ? (await accountRequest<{ ok: boolean }>('/api/session/revoke', { all: true }))?.ok === true
        : (await sessionAdminApi.revokeUser(userId))?.revoked === true;
      if (!revoked) throw new Error('Revocation failed');
      if (attempt !== useAuthStore.getState().authAttempt) return;
      setOpen(false); setDone(true);
      if (self) useAuthStore.getState().logout();
    } catch (error) { setError(t(mfaErrorId(error))); }
    finally { setBusy(false); }
  };
  return <div className="space-y-2">
    <Button variant="destructive" disabled={busy} onClick={() => { setError(''); setDone(false); setOpen(true); }}>{t('sessions.revokeUser')}</Button>
    {done && <p role="status" className="text-sm">{t('sessions.revoked')}</p>}
    <ConfirmDialog open={open} onClose={() => { if (!busy) setOpen(false); }} onConfirm={() => void revoke()}
      title={t('sessions.revokeUser')} message={intl.formatMessage({ id: 'sessions.revokeConfirm' }, { name })}
      confirmLabel={t('mfa.confirm')} busy={busy}>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    </ConfirmDialog>
  </div>;
}
