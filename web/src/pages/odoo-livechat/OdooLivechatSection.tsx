import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, CheckCircle, Loader2, RefreshCw, Save, Undo2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  api,
  type OdooLivechatConfig,
  type OdooLivechatGetResult,
  type OdooLivechatSession,
  type OdooLivechatTestResult,
} from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Badge, Button, Checkbox, SettingsCard, SettingsRow, SettingsSection, Spinner } from '@/components/mds';
import { RowNumber, RowSecret, RowSelect, RowSwitch, RowText } from '@/pages/agent-form/form-rows';
import type { SelectOption } from '@/components/settings/controls';
import { AudienceRulesEditor, ListInput } from './AudienceRulesEditor';
import { toggleId, validateConfig } from './livechat-form';

const STATUS_REFRESH_MS = 10_000;
const MAIN_AGENT = '';

/**
 * Odoo website Live Chat (`odoo.livechat.*`): an AI employee answers website
 * visitors as an Odoo operator, routed per visitor by ordered audience rules.
 * Lives under the Odoo tab and saves its own `[odoo_livechat]` section, so it
 * never touches the connection settings above it.
 */
export function OdooLivechatSection() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);

  const [data, setData] = useState<OdooLivechatGetResult | null>(null);
  const [cfg, setCfg] = useState<OdooLivechatConfig | null>(null);
  const [botKey, setBotKey] = useState('');
  const [agents, setAgents] = useState<SelectOption[]>([]);
  const [test, setTest] = useState<OdooLivechatTestResult | null>(null);
  const [testError, setTestError] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [saving, setSaving] = useState(false);
  const [sessions, setSessions] = useState<OdooLivechatSession[]>([]);

  const load = useCallback(async () => {
    try {
      const res = await api.odoo.livechatGet();
      setData(res);
      setCfg(res.config);
      setSessions(res.sessions);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  useEffect(() => {
    void load();
    api.agents
      .list()
      .then((res) =>
        setAgents((res?.agents ?? []).map((a) => ({ value: a.name, label: a.display_name || a.name }))),
      )
      .catch((e) => console.warn('[api]', e));
  }, [load]);

  // Live status + sessions, refreshed while the page is open.
  useEffect(() => {
    const id = window.setInterval(() => {
      api.odoo
        .livechatStatus()
        .then((res) => {
          setSessions(res.sessions);
          setData((prev) => (prev ? { ...prev, status: res.status } : prev));
        })
        .catch(() => undefined);
    }, STATUS_REFRESH_MS);
    return () => window.clearInterval(id);
  }, []);

  if (!cfg || !data) {
    return (
      <SettingsSection title={t('odoo.livechat.title')}>
        <div className="flex justify-center py-8">
          <Spinner className="size-5 text-muted-foreground" />
        </div>
      </SettingsSection>
    );
  }

  const patch = (p: Partial<OdooLivechatConfig>) => setCfg({ ...cfg, ...p });
  const agentOptions: SelectOption[] = agents;
  const defaultAgentOptions: SelectOption[] = [
    { value: MAIN_AGENT, label: t('odoo.livechat.mainAgent') },
    ...agents,
  ];

  const handleTest = async () => {
    setTesting(true);
    setTestError(null);
    try {
      const res = await api.odoo.livechatTest({
        bot_username: cfg.bot_username,
        ...(botKey ? { bot_api_key: botKey } : {}),
      });
      setTest(res);
    } catch (e) {
      setTest(null);
      setTestError(formatError(e));
    } finally {
      setTesting(false);
    }
  };

  const handleSave = async () => {
    const problem = validateConfig(cfg);
    if (problem) {
      const [, n, field] = problem.split(':');
      toast.error(t(field === 'name' ? 'odoo.livechat.rule.needName' : 'odoo.livechat.rule.needAgent', { n }));
      return;
    }
    setSaving(true);
    try {
      await api.odoo.livechatSet(cfg, botKey || undefined);
      setBotKey('');
      toast.success(t('odoo.livechat.saved'));
      await load();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  const handleResume = async (id: number) => {
    try {
      await api.odoo.livechatResume(id);
      setSessions((list) => list.map((s) => (s.id === id ? { ...s, human: false, human_reason: null } : s)));
      toast.success(t('odoo.livechat.sessions.resumed'));
    } catch (e) {
      toast.error(formatError(e));
    }
  };

  const status = data.status;
  const statusTone =
    status.state === 'online' ? 'text-success' : status.state === 'error' ? 'text-destructive' : 'text-muted-foreground';

  return (
    <>
      <SettingsSection title={t('odoo.livechat.title')} description={t('odoo.livechat.desc')}>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <span className={cn('inline-flex items-center gap-1.5 text-sm font-medium', statusTone)}>
            {status.state === 'online' ? <CheckCircle className="size-4" /> : status.state === 'error' ? <AlertTriangle className="size-4" /> : null}
            {t(`odoo.livechat.state.${status.state}`)}
            {status.state === 'online' && status.bot_name && ` · ${status.bot_name}`}
            {status.presence_api && ` · ${t(`odoo.livechat.presence.${status.presence_api}`)}`}
          </span>
          {status.message && <span className="text-sm text-muted-foreground">{status.message}</span>}
        </div>
        {!data.odoo_configured && (
          <p className="text-sm text-destructive">{t('odoo.livechat.needOdoo')}</p>
        )}

        <SettingsCard>
          <RowSwitch label={t('odoo.livechat.enabled')} description={t('odoo.livechat.enabledDesc')} checked={cfg.enabled} onChange={(v) => patch({ enabled: v })} />
          <RowText
            label={t('odoo.livechat.botUsername')}
            description={t('odoo.livechat.botUsernameDesc')}
            value={cfg.bot_username}
            onChange={(v) => patch({ bot_username: v })}
            placeholder="ai-support@mycompany.com"
          />
          <RowSecret
            label={t('odoo.livechat.botKey')}
            description={t('odoo.livechat.botKeyDesc')}
            value={botKey}
            onChange={setBotKey}
            placeholder={data.bot_api_key_set ? t('odoo.livechat.keySet') : ''}
          />
          <RowSelect
            label={t('odoo.livechat.defaultAgent')}
            description={t('odoo.livechat.defaultAgentDesc')}
            value={cfg.default_agent}
            onChange={(v) => patch({ default_agent: v })}
            options={defaultAgentOptions}
          />
          <RowText
            label={t('odoo.livechat.greeting')}
            description={t('odoo.livechat.greetingDesc')}
            value={cfg.greeting}
            onChange={(v) => patch({ greeting: v })}
          />
          <RowNumber
            label={t('odoo.livechat.pollInterval')}
            value={cfg.poll_interval_seconds}
            onChange={(v) => patch({ poll_interval_seconds: v })}
            min={2}
            max={60}
          />
        </SettingsCard>

        <div className="flex flex-wrap items-center gap-3">
          <Button variant="outline" size="sm" onClick={handleTest} disabled={testing || !data.odoo_configured}>
            {testing ? <Loader2 className="animate-spin" /> : <RefreshCw />}
            {t('odoo.livechat.test')}
          </Button>
          {testError && <span className="text-sm text-destructive">{testError}</span>}
          {test && (
            <span className="inline-flex items-center gap-1.5 text-sm font-medium text-success">
              <CheckCircle className="size-4" />
              {t('odoo.livechat.testOk', { name: test.bot.name, generation: test.odoo_generation })}
            </span>
          )}
        </div>
        {test && test.warnings.length > 0 && (
          <ul className="space-y-1.5 rounded-lg bg-warning/10 px-4 py-3 text-sm">
            {test.warnings.map((w) => (
              <li key={w} className="flex gap-2">
                <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" />
                <span>{t(`odoo.livechat.warn.${w}`)}</span>
              </li>
            ))}
          </ul>
        )}
        {test && test.channels.length > 0 && (
          <SettingsCard>
            <SettingsRow label={t('odoo.livechat.channels')} description={t('odoo.livechat.channelsDesc')} tier="text">
              <div className="flex flex-col items-end gap-2">
                {test.channels.map((c) => (
                  <label key={c.id} className="inline-flex items-center gap-2 text-sm">
                    <Checkbox
                      checked={cfg.livechat_channel_ids.includes(c.id)}
                      onCheckedChange={(v) => patch({ livechat_channel_ids: toggleId(cfg.livechat_channel_ids, c.id, Boolean(v)) })}
                    />
                    {c.name}
                    {!c.bot_is_operator && <Badge variant="destructive">{t('odoo.livechat.notOperator')}</Badge>}
                  </label>
                ))}
              </div>
            </SettingsRow>
          </SettingsCard>
        )}
      </SettingsSection>

      <SettingsSection title={t('odoo.livechat.handoff')} description={t('odoo.livechat.handoffDesc')}>
        <SettingsCard>
          <SettingsRow label={t('odoo.livechat.handoffUsers')} description={t('odoo.livechat.handoffUsersDesc')} tier="text">
            {test && test.users.length > 0 ? (
              <div className="flex max-h-48 flex-col items-end gap-2 overflow-y-auto">
                {test.users
                  .filter((u) => u.id !== test.bot.uid)
                  .map((u) => (
                    <label key={u.id} className="inline-flex items-center gap-2 text-sm">
                      <Checkbox
                        checked={cfg.handoff_user_ids.includes(u.id)}
                        onCheckedChange={(v) => patch({ handoff_user_ids: toggleId(cfg.handoff_user_ids, u.id, Boolean(v)) })}
                      />
                      {u.name}
                    </label>
                  ))}
              </div>
            ) : (
              <ListInput
                ids
                values={cfg.handoff_user_ids}
                onChange={(v) => patch({ handoff_user_ids: v.map(Number) })}
                placeholder={t('odoo.livechat.idsPlaceholder')}
              />
            )}
          </SettingsRow>
          <SettingsRow label={t('odoo.livechat.handoffKeywords')} tier="text">
            <ListInput values={cfg.handoff_keywords} onChange={(v) => patch({ handoff_keywords: v })} />
          </SettingsRow>
          <RowText label={t('odoo.livechat.handoffMessage')} value={cfg.handoff_message} onChange={(v) => patch({ handoff_message: v })} />
        </SettingsCard>
      </SettingsSection>

      <SettingsSection title={t('odoo.livechat.rules')} description={t('odoo.livechat.rulesDesc')}>
        <AudienceRulesEditor
          rules={cfg.audience_rules}
          onChange={(rules) => patch({ audience_rules: rules })}
          agentOptions={agentOptions}
          pickers={test}
        />
        <div className="flex justify-end border-t border-surface-border pt-4">
          <Button onClick={handleSave} disabled={saving}>
            {saving ? <Loader2 className="animate-spin" /> : <Save />}
            {t('odoo.livechat.save')}
          </Button>
        </div>
      </SettingsSection>

      <SettingsSection title={t('odoo.livechat.sessions')} description={t('odoo.livechat.sessionsDesc')}>
        {sessions.length === 0 ? (
          <p className="py-6 text-center text-sm text-muted-foreground">{t('odoo.livechat.sessions.empty')}</p>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead className="text-left text-xs text-muted-foreground">
                <tr>
                  <th className="py-2 pr-3 font-medium">{t('odoo.livechat.sessions.visitor')}</th>
                  <th className="py-2 pr-3 font-medium">{t('odoo.livechat.sessions.segment')}</th>
                  <th className="py-2 pr-3 font-medium">{t('odoo.livechat.sessions.agent')}</th>
                  <th className="py-2 pr-3 font-medium">{t('odoo.livechat.sessions.state')}</th>
                  <th className="py-2 pr-3 font-medium">{t('odoo.livechat.sessions.replies')}</th>
                  <th className="py-2 font-medium" />
                </tr>
              </thead>
              <tbody>
                {sessions.map((s) => (
                  <tr key={s.id} className="border-t border-surface-border">
                    <td className="py-2 pr-3">
                      {s.visitor_name || `#${s.id}`}
                      <span className="ml-1 text-xs text-muted-foreground">
                        {[s.logged_in ? t('odoo.livechat.sessions.member') : null, s.country, s.lang].filter(Boolean).join(' · ')}
                      </span>
                    </td>
                    <td className="py-2 pr-3">{s.agent ? (s.rule ?? t('odoo.livechat.sessions.default')) : '—'}</td>
                    <td className="py-2 pr-3">{agents.find((a) => a.value === s.agent)?.label ?? (s.agent || '—')}</td>
                    <td className="py-2 pr-3">
                      {s.human ? (
                        <Badge variant="secondary">
                          {t(s.human_reason === 'visitor_requested' ? 'odoo.livechat.sessions.handedOff' : s.human_reason === 'operator_member' ? 'odoo.livechat.sessions.operatorJoined' : 'odoo.livechat.sessions.takenOver')}
                        </Badge>
                      ) : (
                        <Badge>{t('odoo.livechat.sessions.ai')}</Badge>
                      )}
                    </td>
                    <td className="py-2 pr-3 tabular-nums">{s.replies}</td>
                    <td className="py-2 text-right">
                      {s.human && (
                        <Button variant="ghost" size="sm" onClick={() => void handleResume(s.id)}>
                          <Undo2 />
                          {t('odoo.livechat.sessions.resume')}
                        </Button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </SettingsSection>
    </>
  );
}
