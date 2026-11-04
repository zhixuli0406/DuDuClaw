import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import type { RedactionConfig } from '@/lib/api';
import { Button, Input, Spinner } from '@/components/mds';
import { client } from '@/lib/ws-client';
import { formatError, toast } from '@/lib/toast';

type Group = { channel: string; group_id: string; scope_id?: string; max_messages: number; max_age_hours: number };
type GroupList = { groups: Group[]; known_groups: { channel: string; group_id: string }[] };
type Choices = { agents: string[]; available_agents: string[]; guilds: { id: string; name: string }[] };

export function GroupContextSettings({ platform, active }: { platform: string; active: boolean }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [redactionOn, setRedactionOn] = useState<boolean | null>(null);
  const [data, setData] = useState<GroupList>({ groups: [], known_groups: [] });
  const [group, setGroup] = useState('');
  const [scope, setScope] = useState('');
  const [hours, setHours] = useState(24);
  const [count, setCount] = useState(50);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const load = async () => {
    const result = await client.call('channels.group_context_list', { channel: platform }) as GroupList;
    setData(result);
  };
  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    setLoading(true);
    setError('');
    setRedactionOn(null);
    client.call('redaction.get')
      .then((result) => {
        const config = result as RedactionConfig;
        if (!cancelled) setRedactionOn(config.enabled === true && !config.poisoned
          && config.sources?.user_input?.mode === 'on');
      })
      .catch(() => { if (!cancelled) setRedactionOn(null); });
    client.call('channels.group_context_list', { channel: platform })
      .then((result) => { if (!cancelled) setData(result as GroupList); })
      .catch((e) => { if (!cancelled) setError(formatError(e)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [active, platform]);
  const save = async (remove: boolean, id = group) => {
    setBusy(true);
    setError('');
    try {
      await client.call(remove ? 'channels.group_context_remove' : 'channels.group_context_set', remove
        ? { channel: platform, group_id: id }
        : { channel: platform, group_id: id, ...(scope ? { scope_id: scope } : {}), max_messages: count, max_age_hours: hours });
      await load();
      toast.success(t('channels.detail.saved'));
    } catch (e) { setError(formatError(e)); }
    finally { setBusy(false); }
  };
  const chooseGroup = (id: string) => {
    setGroup(id);
    const existing = data.groups.find((g) => g.group_id === id);
    setHours(existing?.max_age_hours ?? 24);
    setCount(existing?.max_messages ?? 50);
    setScope(existing?.scope_id ?? '');
  };
  return <div className="space-y-5">
    <section className="space-y-3">
      <h3 className="text-sm font-semibold">{t('channels.groupContext.title')}</h3>
      <p className="text-xs text-muted-foreground">{t('channels.groupContext.help')}</p>
      {loading ? <Spinner /> : <>
        {data.groups.map((g) => <div key={g.group_id} className="flex items-center justify-between gap-3 rounded-lg border p-3">
          <button type="button" className="text-left text-sm" onClick={() => chooseGroup(g.group_id)}>
            <span className="break-all font-medium">{g.group_id}</span>
            <span className="block text-xs text-muted-foreground">{intl.formatMessage({ id: 'channels.groupContext.retention' }, { hours: g.max_age_hours, count: g.max_messages })}</span>
          </button>
          <Button variant="outline" size="sm" disabled={busy} onClick={() => void save(true, g.group_id)}>{t('channels.groupContext.disable')}</Button>
        </div>)}
        <label className="block space-y-1 text-sm">{t('channels.groupContext.group')}
          <Input list={`group-context-${platform}`} value={group} onChange={(e) => chooseGroup(e.target.value)} />
        </label>
        <datalist id={`group-context-${platform}`}>
          {data.known_groups.map((g) => <option key={g.group_id} value={g.group_id} />)}
        </datalist>
        {platform === 'discord' && <label className="block space-y-1 text-sm">{t('channels.groupContext.guild')}
          <Input value={scope} onChange={(e) => setScope(e.target.value)} />
        </label>}
        <div className="grid grid-cols-2 gap-3">
          <label className="space-y-1 text-sm">{t('channels.groupContext.hours')}
            <Input type="number" min={1} max={168} value={hours} onChange={(e) => setHours(Number(e.target.value))} />
          </label>
          <label className="space-y-1 text-sm">{t('channels.groupContext.count')}
            <Input type="number" min={1} max={200} value={count} onChange={(e) => setCount(Number(e.target.value))} />
          </label>
        </div>
        <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'channels.groupContext.disclosure' }, { hours, count })}</p>
        {redactionOn !== true && <div role="status" className="rounded-lg border p-3 text-sm">
          <p>{t(redactionOn === false ? 'channels.groupContext.redactionOff' : 'channels.groupContext.redactionUnknown')}</p>
          <Link className="text-brand underline underline-offset-2" to="/app/system/settings?tab=redaction">{t('channels.groupContext.redactionSettings')}</Link>
        </div>}
        <Button variant="brand" size="sm" onClick={() => void save(false)} disabled={busy || !group.trim() || (platform === 'discord' && !scope.trim()) || !Number.isInteger(hours) || hours < 1 || hours > 168 || !Number.isInteger(count) || count < 1 || count > 200}>{t('channels.groupContext.enable')}</Button>
      </>}
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    </section>
    {platform === 'discord' && <DiscordChoicesSettings active={active} />}
  </div>;
}

function DiscordChoicesSettings({ active }: { active: boolean }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [guild, setGuild] = useState('');
  const [loadedGuild, setLoadedGuild] = useState<string | null>(null);
  const [data, setData] = useState<Choices>({ agents: [], available_agents: [], guilds: [] });
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    setLoading(true);
    setError('');
    setLoadedGuild(null);
    client.call('channels.discord_agent_choices_get', { guild_id: guild })
      .then((result) => { if (!cancelled) { setData(result as Choices); setLoadedGuild(guild); } })
      .catch((e) => { if (!cancelled) setError(formatError(e)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [active, guild]);
  const save = async () => {
    setBusy(true);
    setError('');
    try {
      await client.call('channels.discord_agent_choices_set', { guild_id: guild, agents: data.agents });
      toast.success(t('channels.detail.saved'));
    } catch (e) { setError(formatError(e)); }
    finally { setBusy(false); }
  };
  return <section className="space-y-3 border-t pt-4">
    <h3 className="text-sm font-semibold">{t('channels.discordChoices.title')}</h3>
    <p className="text-xs text-muted-foreground">{t('channels.discordChoices.help')}</p>
    <label className="block space-y-1 text-sm">{t('channels.groupContext.guild')}
      <Input list="discord-choice-guilds" value={guild} onChange={(e) => setGuild(e.target.value)} />
    </label>
    <datalist id="discord-choice-guilds">{data.guilds.map((g) => <option key={g.id} value={g.id}>{g.name}</option>)}</datalist>
    {loading ? <Spinner /> : guild && <div className="space-y-2">{data.available_agents.map((id) => <label key={id} className="flex items-center gap-2 text-sm">
      <input type="checkbox" disabled={busy} checked={data.agents.includes(id)} onChange={(e) => setData({ ...data, agents: e.target.checked ? [...data.agents, id] : data.agents.filter((a) => a !== id) })} />{id}
    </label>)}</div>}
    <Button variant="brand" size="sm" disabled={busy || loading || loadedGuild !== guild || !/^\d{1,64}$/.test(guild)} onClick={() => void save()}>{t('common.save')}</Button>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
  </section>;
}
