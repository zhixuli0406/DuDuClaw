import { useEffect, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { api, type AgentInfo } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
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
import { AlertTriangle } from 'lucide-react';

/** Channel type picker options — value ⇒ human label (spec §4 Select). */
export const CHANNEL_TYPES: ReadonlyArray<{ value: string; label: string }> = [
  { value: 'telegram', label: 'Telegram' },
  { value: 'line', label: 'LINE' },
  { value: 'discord', label: 'Discord' },
  { value: 'slack', label: 'Slack' },
  { value: 'whatsapp', label: 'WhatsApp' },
  { value: 'feishu', label: 'Feishu' },
  { value: 'wecom', label: 'WeCom (企業微信)' },
  { value: 'dingtalk', label: 'DingTalk (釘釘)' },
  { value: 'googlechat', label: 'Google Chat' },
  { value: 'teams', label: 'Microsoft Teams' },
];

export const SUPPORTS_PER_AGENT = ['discord', 'telegram', 'slack'];

/** Stacked label + control block used across the channel dialogs (spec §5.3). */
export function DialogField({
  label,
  help,
  children,
}: {
  label: string;
  help?: string;
  children: ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label className="text-sm font-medium text-foreground">{label}</label>
      {children}
      {help && <p className="text-xs text-muted-foreground">{help}</p>}
    </div>
  );
}

/**
 * Add / re-configure a messaging channel. Shared between the Channels
 * management page and the agent-settings 整合 tab (2026-08-13 unification —
 * previously the agent form kept its own 13 raw token fields writing the same
 * `agent.toml [channels.*]` sections, a drift-prone duplicate editor).
 *
 * `lockedAgent` pins the agent picker to one agent (agent-settings entry):
 * per-agent-capable platforms (discord/telegram/slack) are bound to that
 * agent; webhook platforms fall through to the global channel with the agent
 * bound as its default responder — exactly the `channels.add` backend
 * semantics.
 */
export function AddChannelDialog({
  open,
  onClose,
  onCreated,
  fixedType,
  lockedAgent,
}: {
  open: boolean;
  onClose: () => void;
  onCreated: (createdType?: string) => void;
  fixedType?: string;
  lockedAgent?: string;
}) {
  const intl = useIntl();
  // Parse fixedType: "discord:lab-bot" → platform="discord", agent="lab-bot"
  const parsedPlatform = fixedType?.split(':')[0];
  const parsedAgent = fixedType?.includes(':') ? fixedType.split(':').slice(1).join(':') : undefined;

  const [channelType, setChannelType] = useState(parsedPlatform ?? fixedType ?? 'line');
  const [selectedAgent, setSelectedAgent] = useState(parsedAgent ?? lockedAgent ?? '');
  const [agents, setAgents] = useState<AgentInfo[]>([]);

  useEffect(() => {
    if (fixedType) {
      setChannelType(parsedPlatform ?? fixedType);
      setSelectedAgent(parsedAgent ?? lockedAgent ?? '');
    }
  }, [fixedType, parsedPlatform, parsedAgent, lockedAgent]);

  useEffect(() => {
    if (open) {
      api.agents.list().then((r) => setAgents(r.agents ?? [])).catch((e) => {
        console.warn("[api]", e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      });
    }
  }, [open, intl]);

  const [token, setToken] = useState('');
  const [secret, setSecret] = useState('');
  // G.6 — extra per-platform tokens stored under config.* keys (write-only).
  const [waVerifyToken, setWaVerifyToken] = useState('');
  const [waAppSecret, setWaAppSecret] = useState('');
  const [feishuVerifyToken, setFeishuVerifyToken] = useState('');
  const [teamsTenantId, setTeamsTenantId] = useState('');
  const [wecomAgentId, setWecomAgentId] = useState('');
  const [wecomCallbackToken, setWecomCallbackToken] = useState('');
  const [wecomAesKey, setWecomAesKey] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);

  const handleSubmit = async () => {
    if (!token.trim()) return;
    setSubmitting(true);
    try {
      const config: Record<string, string> = { token: token.trim() };
      if (secret.trim()) config.secret = secret.trim();
      // G.6 — extra global channel tokens; only sent when typed.
      if (channelType === 'whatsapp') {
        if (waVerifyToken.trim()) config.whatsapp_verify_token = waVerifyToken.trim();
        if (waAppSecret.trim()) config.whatsapp_app_secret = waAppSecret.trim();
      }
      if (channelType === 'feishu' && feishuVerifyToken.trim()) {
        config.feishu_verification_token = feishuVerifyToken.trim();
      }
      if (channelType === 'teams' && teamsTenantId.trim()) {
        config.teams_tenant_id = teamsTenantId.trim();
      }
      if (channelType === 'wecom') {
        if (wecomAgentId.trim()) config.wecom_agent_id = wecomAgentId.trim();
        if (wecomCallbackToken.trim()) config.wecom_callback_token = wecomCallbackToken.trim();
        if (wecomAesKey.trim()) config.wecom_encoding_aes_key = wecomAesKey.trim();
      }
      await api.channels.add(channelType, config, selectedAgent || undefined);
      onCreated(channelType);
      onClose();
      setToken('');
      setSecret('');
      setWaVerifyToken('');
      setWaAppSecret('');
      setFeishuVerifyToken('');
      setTeamsTenantId('');
      setWecomAgentId('');
      setWecomCallbackToken('');
      setWecomAesKey('');
      setSelectedAgent(lockedAgent ?? '');
    } catch (e) {
      setAddError(String(e));
    } finally {
      setSubmitting(false);
    }
  };

  const channelGuide: Record<string, { tokenLabel: string; secretLabel?: string; stepKeys: string[] }> = {
    telegram: {
      tokenLabel: 'Bot Token',
      stepKeys: [
        'channels.setup.telegram.step1',
        'channels.setup.telegram.step2',
        'channels.setup.telegram.step3',
        'channels.setup.telegram.note',
      ],
    },
    line: {
      tokenLabel: 'Channel Access Token',
      secretLabel: 'Channel Secret',
      stepKeys: [
        'channels.setup.line.step1',
        'channels.setup.line.step2',
        'channels.setup.line.step3',
        'channels.setup.line.step4',
        'channels.setup.line.step5',
        'channels.setup.line.note',
      ],
    },
    discord: {
      tokenLabel: 'Bot Token',
      stepKeys: [
        'channels.setup.discord.step1',
        'channels.setup.discord.step2',
        'channels.setup.discord.step3',
        'channels.setup.discord.intentWarning',
        'channels.setup.discord.intentRecommend',
        'channels.setup.discord.step4',
        'channels.setup.discord.step5',
        'channels.setup.discord.perm1',
        'channels.setup.discord.perm2',
        'channels.setup.discord.perm3',
        'channels.setup.discord.step6',
        'channels.setup.discord.reinviteTip',
      ],
    },
    slack: {
      tokenLabel: 'Bot User OAuth Token (xoxb-...)',
      secretLabel: 'App-Level Token (xapp-...)',
      stepKeys: [
        'channels.setup.slack.step1',
        'channels.setup.slack.step2',
        'channels.setup.slack.step3',
        'channels.setup.slack.step4',
        'channels.setup.slack.step5',
        'channels.setup.slack.note',
      ],
    },
    whatsapp: {
      tokenLabel: 'Access Token',
      secretLabel: 'Phone Number ID',
      stepKeys: [
        'channels.setup.whatsapp.step1',
        'channels.setup.whatsapp.step2',
        'channels.setup.whatsapp.step3',
        'channels.setup.whatsapp.step4',
        'channels.setup.whatsapp.step5',
        'channels.setup.whatsapp.step6',
        'channels.setup.whatsapp.note',
      ],
    },
    feishu: {
      tokenLabel: 'App ID',
      secretLabel: 'App Secret',
      stepKeys: [
        'channels.setup.feishu.step1',
        'channels.setup.feishu.step2',
        'channels.setup.feishu.step3',
        'channels.setup.feishu.step4',
        'channels.setup.feishu.step5',
        'channels.setup.feishu.step6',
      ],
    },
    wecom: {
      tokenLabel: 'Corp Secret',
      secretLabel: 'Corp ID',
      stepKeys: [],
    },
    dingtalk: {
      tokenLabel: 'App Secret',
      secretLabel: 'App Key (Client ID)',
      stepKeys: [],
    },
    googlechat: {
      tokenLabel: 'Service Account JSON',
      secretLabel: 'Project Number',
      stepKeys: [
        'channels.setup.googlechat.step1',
        'channels.setup.googlechat.step2',
        'channels.setup.googlechat.step3',
        'channels.setup.googlechat.note',
      ],
    },
    teams: {
      tokenLabel: 'App Password',
      secretLabel: 'App ID',
      stepKeys: [
        'channels.setup.teams.step1',
        'channels.setup.teams.step2',
        'channels.setup.teams.step3',
        'channels.setup.teams.note',
      ],
    },
  };

  const guide = channelGuide[channelType] ?? { tokenLabel: 'Token', stepKeys: [] };
  const steps = guide.stepKeys.map((id) => intl.formatMessage({ id }));
  const typeLabel = CHANNEL_TYPES.find((c) => c.value === channelType)?.label ?? channelType;
  const lockedAgentLabel = lockedAgent
    ? agents.find((a) => a.name === lockedAgent)?.display_name || lockedAgent
    : '';

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>
            {fixedType
              ? intl.formatMessage({ id: 'channels.dialog.editTitle' }, { type: fixedType })
              : intl.formatMessage({ id: 'channels.dialog.addTitle' })}
          </DialogTitle>
        </DialogHeader>

        <div className="max-h-[60vh] space-y-4 overflow-y-auto">
          <DialogField label={intl.formatMessage({ id: 'channels.dialog.type' })}>
            <Select value={channelType} onValueChange={(v) => setChannelType(String(v))} disabled={!!fixedType}>
              <SelectTrigger className="w-full">
                <SelectValue>{typeLabel}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                {CHANNEL_TYPES.map((c) => (
                  <SelectItem key={c.value} value={c.value}>{c.label}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </DialogField>

          {lockedAgent ? (
            <DialogField
              label={intl.formatMessage({ id: 'channels.dialog.assignAgent' })}
              help={intl.formatMessage(
                {
                  id: SUPPORTS_PER_AGENT.includes(channelType)
                    ? 'channels.dialog.lockedAgent.perAgent'
                    : 'channels.dialog.lockedAgent.globalNote',
                },
                { agent: lockedAgentLabel },
              )}
            >
              <Input value={lockedAgentLabel} disabled readOnly />
            </DialogField>
          ) : (
            SUPPORTS_PER_AGENT.includes(channelType) && agents.length > 0 && (
              <>
                <DialogField
                  label={intl.formatMessage({ id: 'channels.dialog.assignAgent' })}
                  help={intl.formatMessage({ id: 'channels.dialog.assignAgentHint' })}
                >
                  <Select value={selectedAgent} onValueChange={(v) => setSelectedAgent(String(v))}>
                    <SelectTrigger className="w-full">
                      <SelectValue>
                        {selectedAgent
                          ? agents.find((a) => a.name === selectedAgent)?.display_name || selectedAgent
                          : intl.formatMessage({ id: 'channels.dialog.global' })}
                      </SelectValue>
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="">{intl.formatMessage({ id: 'channels.dialog.global' })}</SelectItem>
                      {agents.map((a) => (
                        <SelectItem key={a.name} value={a.name}>{a.display_name || a.name}</SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </DialogField>
                {/* R-PLAIN-WORDS (§2-1): distinguishes this "路由訊息" setting from
                    UsersPage's "授權存取" (who can operate the AI staff) and the
                    channel-identity block's "驗證身分" (who is who on the channel). */}
                <p className="-mt-3 text-xs text-muted-foreground/80">
                  {intl.formatMessage({ id: 'channels.dialog.assignAgent.explain' })}
                </p>
              </>
            )
          )}

          {/* Setup guide */}
          <div className="rounded-lg bg-warning/10 p-3 text-xs text-warning">
            <p className="mb-1 font-medium">{intl.formatMessage({ id: 'channels.dialog.setupGuide' })}</p>
            {steps.map((step, i) => (
              <p key={i} className={step.startsWith('⚠') ? 'font-semibold text-destructive' : ''}>
                {step}
              </p>
            ))}
          </div>

          <DialogField label={guide.tokenLabel}>
            <Input
              type="password"
              value={token}
              onChange={(e) => setToken(e.target.value)}
              placeholder={intl.formatMessage({ id: 'channels.dialog.pastePlaceholder' }, { tokenLabel: guide.tokenLabel.toLowerCase() })}
            />
          </DialogField>

          {guide.secretLabel && (
            <DialogField label={guide.secretLabel}>
              <Input
                type="password"
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
                placeholder={guide.secretLabel}
              />
            </DialogField>
          )}

          {/* G.6 — extra WhatsApp tokens (global) */}
          {channelType === 'whatsapp' && (
            <>
              <DialogField label="Verify Token" help={intl.formatMessage({ id: 'channels.field.writeOnly' })}>
                <Input type="password" value={waVerifyToken} onChange={(e) => setWaVerifyToken(e.target.value)} autoComplete="off" />
              </DialogField>
              <DialogField label="App Secret" help={intl.formatMessage({ id: 'channels.field.writeOnly' })}>
                <Input type="password" value={waAppSecret} onChange={(e) => setWaAppSecret(e.target.value)} autoComplete="off" />
              </DialogField>
            </>
          )}

          {/* G.6 — extra Feishu token (global) */}
          {channelType === 'feishu' && (
            <DialogField label="Verification Token" help={intl.formatMessage({ id: 'channels.field.writeOnly' })}>
              <Input type="password" value={feishuVerifyToken} onChange={(e) => setFeishuVerifyToken(e.target.value)} autoComplete="off" />
            </DialogField>
          )}

          {channelType === 'teams' && (
            <DialogField label="Tenant ID" help={intl.formatMessage({ id: 'channels.setup.teams.tenantHint' })}>
              <Input type="text" value={teamsTenantId} onChange={(e) => setTeamsTenantId(e.target.value)} autoComplete="off" placeholder="(multi-tenant)" />
            </DialogField>
          )}

          {/* G.6 — extra WeCom tokens (global) */}
          {channelType === 'wecom' && (
            <>
              <DialogField label="AgentId">
                <Input type="text" value={wecomAgentId} onChange={(e) => setWecomAgentId(e.target.value)} autoComplete="off" />
              </DialogField>
              <DialogField label="Callback Token" help={intl.formatMessage({ id: 'channels.field.writeOnly' })}>
                <Input type="password" value={wecomCallbackToken} onChange={(e) => setWecomCallbackToken(e.target.value)} autoComplete="off" />
              </DialogField>
              <DialogField label="EncodingAESKey" help={intl.formatMessage({ id: 'channels.field.writeOnly' })}>
                <Input type="password" value={wecomAesKey} onChange={(e) => setWecomAesKey(e.target.value)} autoComplete="off" />
              </DialogField>
            </>
          )}

          {addError && (
            <div className="flex items-start gap-2 rounded-lg bg-destructive/10 px-3 py-2 text-xs text-destructive">
              <AlertTriangle className="mt-0.5 size-3 shrink-0" />
              <span>{addError}</span>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'channels.dialog.cancel' })}
          </Button>
          <Button variant="brand" onClick={() => { setAddError(null); handleSubmit(); }} disabled={submitting || !token.trim()}>
            {submitting ? intl.formatMessage({ id: 'channels.dialog.adding' }) : intl.formatMessage({ id: 'channels.dialog.add' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
