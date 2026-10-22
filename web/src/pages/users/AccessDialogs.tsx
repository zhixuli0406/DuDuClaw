// Grant-access and verify-channel-identity dialogs of the Users page (moved
// out of UsersPage.tsx unchanged).
import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { bindChannelIdentity } from '@/lib/channel-identity-api';
import {
  Button,
  Input,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/mds';
import { DialogField, IDENTITY_CHANNELS, accessOptions, type Option } from './dialog-common';

export function BindAgentDialog({
  userId,
  boundAgents,
  onClose,
  onBound,
}: {
  userId: string;
  /** Agent names this user is already bound to — excluded from the picker. */
  boundAgents: string[];
  onClose: () => void;
  onBound: () => void;
}) {
  const intl = useIntl();
  const [agentName, setAgentName] = useState('');
  const [accessLevel, setAccessLevel] = useState('owner');
  const [agentOptions, setAgentOptions] = useState<Option[] | null>(null);
  const [error, setError] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const accesses = accessOptions(intl);

  useEffect(() => {
    let cancelled = false;
    api.agents
      .list()
      .then(({ agents }) => {
        if (cancelled) return;
        const options = agents
          .filter((a) => !boundAgents.includes(a.name))
          .map((a) => ({
            value: a.name,
            label: a.display_name && a.display_name !== a.name ? `${a.display_name} · ${a.name}` : a.name,
          }));
        setAgentOptions(options);
        setAgentName((prev) => prev || options[0]?.value || '');
      })
      .catch(() => {
        // Roster fetch failed — fall back to the free-text input below.
        if (!cancelled) setAgentOptions(null);
      });
    return () => {
      cancelled = true;
    };
    // boundAgents is derived fresh from the parent each open; the dialog
    // remounts per open so a one-shot fetch is enough.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleSubmit = async () => {
    setError('');
    setSubmitting(true);
    try {
      await api.users.bindAgent(userId, agentName, accessLevel);
      onBound();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'users.action.bind' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <p className="text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'users.action.bind.explain' })}
          </p>
          {error && (
            <div className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</div>
          )}
          <DialogField label={intl.formatMessage({ id: 'users.field.agent_name' })}>
            {agentOptions && agentOptions.length > 0 ? (
              <Select value={agentName} onValueChange={(v) => setAgentName(String(v))}>
                <SelectTrigger className="w-full">
                  <SelectValue>{agentOptions.find((o) => o.value === agentName)?.label ?? agentName}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {agentOptions.map((o) => (
                    <SelectItem key={o.value} value={o.value}>{o.label}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : agentOptions && agentOptions.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                {intl.formatMessage({ id: 'users.bind.none_available' })}
              </p>
            ) : (
              <Input
                placeholder={intl.formatMessage({ id: 'users.field.agent_name' })}
                value={agentName}
                onChange={(e) => setAgentName(e.target.value)}
              />
            )}
          </DialogField>
          <DialogField label={intl.formatMessage({ id: 'users.action.bind.accessLevel' })}>
            <Select value={accessLevel} onValueChange={(v) => setAccessLevel(String(v))}>
              <SelectTrigger className="w-full">
                <SelectValue>{accesses.find((a) => a.value === accessLevel)?.label}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {accesses.map((a) => (
                  <SelectItem key={a.value} value={a.value}>{a.label}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </DialogField>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="brand" onClick={handleSubmit} disabled={submitting || !agentName}>
            {intl.formatMessage({ id: 'users.action.bind' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// ── WP-B (2026-08-12 IA audit §2-1) — 驗證身分 ──────────────────
//
// The third of three unrelated "binding" concepts in the dashboard:
// dashboard user ↔ channel DM identity (`channel_identities` table). This is
// the data `approver_links`/`mapped_role` actually read for channel-side
// approvals (`decision_notify.rs`) — previously had a ready backend
// (`verified_channels_for_user`, `POST /api/channel-identity/bind`) but zero
// dashboard surface, so an admin who only did "授權存取" (§ user↔agent) and
// "路由訊息" (§ agent↔channel) would reasonably assume channel approvals were
// already wired up. They were not — this dialog closes that gap.
export function BindChannelIdentityDialog({
  userId,
  onClose,
  onBound,
}: {
  userId: string;
  onClose: () => void;
  onBound: () => void;
}) {
  const intl = useIntl();
  const [channel, setChannel] = useState(IDENTITY_CHANNELS[0].value);
  const [channelUserId, setChannelUserId] = useState('');
  const [error, setError] = useState('');
  const [submitting, setSubmitting] = useState(false);

  const handleSubmit = async () => {
    const trimmed = channelUserId.trim();
    if (!trimmed) return;
    setError('');
    setSubmitting(true);
    try {
      await bindChannelIdentity(userId, channel, trimmed);
      onBound();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  const howToId = `users.channelIdentity.howTo.${channel}`;

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'users.channelIdentity.add' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <p className="text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'users.channelIdentity.explain' })}
          </p>
          {error && (
            <div className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</div>
          )}
          <DialogField label={intl.formatMessage({ id: 'users.channelIdentity.channel' })}>
            <Select value={channel} onValueChange={(v) => setChannel(String(v))}>
              <SelectTrigger className="w-full">
                <SelectValue>{IDENTITY_CHANNELS.find((c) => c.value === channel)?.label}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {IDENTITY_CHANNELS.map((c) => (
                  <SelectItem key={c.value} value={c.value}>{c.label}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </DialogField>
          <DialogField
            label={intl.formatMessage({ id: 'users.channelIdentity.channelUserId' })}
            help={intl.formatMessage({ id: howToId })}
          >
            <Input
              value={channelUserId}
              onChange={(e) => setChannelUserId(e.target.value)}
              placeholder={intl.formatMessage({ id: 'users.channelIdentity.channelUserIdPlaceholder' })}
            />
          </DialogField>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="brand" onClick={handleSubmit} disabled={submitting || !channelUserId.trim()}>
            {intl.formatMessage({ id: 'users.channelIdentity.add' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
