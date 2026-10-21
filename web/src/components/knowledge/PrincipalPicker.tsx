import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { Building2, Loader2, MessageSquare, Plus, Search, User } from 'lucide-react';
import { Button, Input, Segmented } from '@/components/mds';
import { ActorAvatar } from '@/components/mds';
import { knowledgeApi, type KbAclEntry, type KbPrincipalKind, type KbPrincipals } from '@/lib/knowledge-api';

/** A principal chosen in the picker, with the name the table shows. */
export interface PickedPrincipal {
  kind: KbPrincipalKind;
  id: string;
  label: string;
}

/** Lookup of display names, built from one picker search. */
export function principalLabels(p: KbPrincipals | null): Map<string, string> {
  const m = new Map<string, string>();
  if (!p) return m;
  for (const u of p.users) m.set(`user:${u.id}`, u.display_name || u.id);
  for (const a of p.agents) m.set(`agent:${a.id}`, a.display_name || a.id);
  for (const d of p.departments) m.set(`department:${d}`, d);
  return m;
}

const KIND_ICON = { user: User, department: Building2, agent: User, channel: MessageSquare } as const;

/** Channel chat ids: anything printable, up to 256 characters. */
function validChat(chat: string): boolean {
  // eslint-disable-next-line no-control-regex
  return chat.length <= 256 && !/[\u0000-\u001f\u007f]/.test(chat);
}

/**
 * PrincipalPicker — choose who to share a collection with. The gateway
 * returns only principals the signed-in owner may grant (Personal edition:
 * AI employees and chat channels only); the same rule is enforced again when
 * the grants are saved.
 */
export function PrincipalPicker({
  collectionId,
  existing,
  onPick,
}: {
  collectionId: string;
  existing: readonly KbAclEntry[];
  onPick: (p: PickedPrincipal) => void;
}) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string>) => intl.formatMessage({ id }, v);
  const [query, setQuery] = useState('');
  const [debounced, setDebounced] = useState('');
  const [data, setData] = useState<KbPrincipals | null>(null);
  const [kind, setKind] = useState<KbPrincipalKind>('agent');
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const [chat, setChat] = useState('');

  useEffect(() => {
    const h = setTimeout(() => setDebounced(query.trim()), 250);
    return () => clearTimeout(h);
  }, [query]);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    knowledgeApi
      .searchPrincipals(collectionId, debounced)
      .then((d) => {
        if (!alive) return;
        setData(d);
        setFailed(false);
        setKind((k) => (d.kinds.includes(k) ? k : (d.kinds[0] ?? 'agent')));
      })
      .catch(() => alive && setFailed(true))
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
  }, [collectionId, debounced]);

  const taken = useMemo(() => new Set(existing.map((e) => `${e.kind}:${e.id}`)), [existing]);

  const options = useMemo(() => {
    if (!data) return [] as PickedPrincipal[];
    switch (kind) {
      case 'user':
        return data.users.map((u) => ({ kind, id: u.id, label: u.display_name || u.id }));
      case 'department':
        return data.departments.map((d) => ({ kind, id: d, label: d }));
      case 'agent':
        return data.agents.map((a) => ({ kind, id: a.id, label: a.display_name || a.id }));
      case 'channel':
        return data.channels.map((c) => ({ kind, id: c, label: t(`kb.channel.${c}`) }));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data, kind, intl]);

  const kindOptions = (data?.kinds ?? []).map((k) => ({ value: k, label: t(`kb.principal.kind.${k}`) }));
  const chatTrimmed = chat.trim();
  const chatOk = validChat(chatTrimmed);

  const pick = (p: PickedPrincipal) => {
    if (p.kind === 'channel' && chatTrimmed) {
      onPick({ ...p, id: `${p.id}:${chatTrimmed}`, label: t('kb.principal.channelChat', { channel: p.label, chat: chatTrimmed }) });
      setChat('');
      return;
    }
    onPick(p);
  };

  return (
    <div className="space-y-3">
      {kindOptions.length > 1 && (
        <Segmented
          aria-label={t('kb.principal.kindLabel')}
          value={kind}
          onValueChange={setKind}
          options={kindOptions}
          className="flex-wrap"
        />
      )}
      <div className="relative">
        <Search aria-hidden className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t('kb.principal.searchPlaceholder')}
          aria-label={t('kb.principal.searchPlaceholder')}
          className="pl-8"
        />
      </div>
      {kind === 'channel' && (
        <div className="space-y-1">
          <label htmlFor="kb-channel-chat" className="text-xs font-medium text-foreground">
            {t('kb.principal.chatLabel')}
          </label>
          <Input
            id="kb-channel-chat"
            value={chat}
            onChange={(e) => setChat(e.target.value)}
            placeholder={t('kb.principal.chatPlaceholder')}
            aria-invalid={!chatOk}
          />
          <p className="text-xs text-muted-foreground">{t('kb.principal.chatHint')}</p>
        </div>
      )}
      <div className="max-h-64 overflow-y-auto rounded-lg border border-surface-border" aria-busy={loading}>
        {loading && !data ? (
          <div className="flex justify-center py-6">
            <Loader2 aria-hidden className="size-4 text-muted-foreground motion-safe:animate-spin" />
            <span className="sr-only">{t('common.loading')}</span>
          </div>
        ) : failed ? (
          <p className="px-3 py-4 text-center text-xs text-destructive">{t('kb.principal.loadError')}</p>
        ) : options.length === 0 ? (
          <p className="px-3 py-4 text-center text-xs text-muted-foreground">{t('kb.principal.none')}</p>
        ) : (
          <ul className="divide-y divide-surface-border">
            {options.map((o) => {
              const id = kind === 'channel' && chatTrimmed ? `${o.id}:${chatTrimmed}` : o.id;
              const already = taken.has(`${o.kind}:${id}`);
              const Icon = KIND_ICON[o.kind];
              return (
                <li key={`${o.kind}:${o.id}`} className="flex items-center gap-2 px-3 py-2">
                  {o.kind === 'agent' ? (
                    <ActorAvatar actorType="agent" name={o.label} size="sm" />
                  ) : (
                    <Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                  )}
                  <span className="min-w-0 flex-1 truncate text-sm" title={o.label}>
                    {o.label}
                  </span>
                  <Button
                    variant="outline"
                    size="xs"
                    disabled={already || (o.kind === 'channel' && !chatOk)}
                    onClick={() => pick(o)}
                    aria-label={t('kb.principal.addNamed', { name: o.label })}
                  >
                    <Plus aria-hidden />
                    {already ? t('kb.principal.added') : t('common.add')}
                  </Button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}
