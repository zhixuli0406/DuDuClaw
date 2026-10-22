import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, Building2, Loader2, Lock, MessageSquare, Trash2, User } from 'lucide-react';
import { ActorAvatar, Button, Empty, ErrorState } from '@/components/mds';
import { selectClass } from '@/components/shared/controlClass';
import { cn } from '@/lib/utils';
import { PrincipalPicker, principalLabels, type PickedPrincipal } from '@/components/knowledge/PrincipalPicker';
import { toast, formatError } from '@/lib/toast';
import { KB_ROLES, knowledgeApi, type KbAclEntry, type KbCollection, type KbPrincipalKind, type KbRole } from '@/lib/knowledge-api';

/** Roles each kind of principal may hold (mirrors the gateway's rule). */
export function allowedRoles(kind: KbPrincipalKind): readonly KbRole[] {
  switch (kind) {
    case 'user':
      return KB_ROLES;
    case 'department':
      return ['viewer', 'editor'];
    default:
      return ['viewer'];
  }
}

/** Number of people (not departments) who own the collection. */
export function ownerCount(entries: readonly KbAclEntry[]): number {
  return entries.filter((e) => e.kind === 'user' && e.role === 'owner').length;
}

/** Whether a grant opens the collection to strangers on a chat channel. */
export function isWholeChannel(e: Pick<KbAclEntry, 'kind' | 'id'>): boolean {
  return e.kind === 'channel' && !e.id.includes(':');
}

const KIND_ICON = { user: User, department: Building2, channel: MessageSquare } as const;

function sameEntries(a: readonly KbAclEntry[], b: readonly KbAclEntry[]): boolean {
  if (a.length !== b.length) return false;
  const key = (e: KbAclEntry) => `${e.kind}:${e.id}:${e.role}`;
  const set = new Set(a.map(key));
  return b.every((e) => set.has(key(e)));
}

/**
 * 權限 — who the collection is shared with. Owners edit the list and save
 * it in one go; if someone else saved in between, the save is refused and
 * the page asks to reload. Everyone else sees a short explanation only.
 */
export function PermissionsTab({
  collection,
  isOwner,
  onChanged,
}: {
  collection: KbCollection;
  isOwner: boolean;
  onChanged: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const [saved, setSaved] = useState<KbAclEntry[] | null>(null);
  const [draft, setDraft] = useState<KbAclEntry[]>([]);
  const [revision, setRevision] = useState(0);
  const [labels, setLabels] = useState<Map<string, string>>(new Map());
  const [error, setError] = useState<unknown>(null);
  const [saving, setSaving] = useState(false);
  const [conflict, setConflict] = useState(false);

  const load = useCallback(async () => {
    try {
      setError(null);
      setConflict(false);
      const r = await knowledgeApi.getAcl(collection.id);
      setSaved(r.entries);
      setDraft(r.entries);
      setRevision(r.revision);
    } catch (e) {
      setError(e);
    }
  }, [collection.id]);

  useEffect(() => {
    if (!isOwner) return;
    void load();
  }, [isOwner, collection.id, load]);

  // Names for the people already on the list (the picker never lists
  // people in bulk, so they are looked up by id).
  const savedUserIds = (saved ?? []).filter((e) => e.kind === 'user').map((e) => e.id).join('\u0000');
  useEffect(() => {
    if (!isOwner || saved === null) return;
    const ids = savedUserIds ? savedUserIds.split('\u0000') : [];
    knowledgeApi
      .searchPrincipals(collection.id, '', ids)
      .then((p) => setLabels((m) => new Map([...m, ...principalLabels(p)])))
      .catch(() => {
        /* Names are a nicety; ids still show. */
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isOwner, collection.id, savedUserIds]);

  const dirty = saved !== null && !sameEntries(saved, draft);
  const owners = ownerCount(draft);
  const opensChannel = useMemo(() => draft.some(isWholeChannel), [draft]);

  if (!isOwner) {
    return (
      <Empty
        icon={Lock}
        title={t('kb.perm.readOnlyTitle')}
        description={t('kb.perm.readOnly', { role: collection.my_role ? t(`kb.role.${collection.my_role}`) : '' })}
      />
    );
  }
  if (error != null && saved === null) return <ErrorState error={error} onRetry={() => void load()} />;
  if (saved === null) {
    return (
      <div className="flex justify-center py-10">
        <Loader2 aria-hidden className="size-5 text-muted-foreground motion-safe:animate-spin" />
        <span className="sr-only">{t('common.loading')}</span>
      </div>
    );
  }

  const labelOf = (e: Pick<KbAclEntry, 'kind' | 'id'>): string => {
    if (e.kind === 'channel') {
      const [channel, chat] = e.id.split(/:(.*)/s);
      const name = t(`kb.channel.${channel}`);
      return chat ? t('kb.principal.channelChat', { channel: name, chat }) : t('kb.principal.channelAll', { channel: name });
    }
    return labels.get(`${e.kind}:${e.id}`) ?? e.id;
  };

  const add = (p: PickedPrincipal) => {
    setLabels((m) => new Map(m).set(`${p.kind}:${p.id}`, p.label));
    setDraft((d) => (d.some((e) => e.kind === p.kind && e.id === p.id) ? d : [...d, { kind: p.kind, id: p.id, role: 'viewer' }]));
  };
  const setRole = (i: number, role: KbRole) => setDraft((d) => d.map((e, j) => (j === i ? { ...e, role } : e)));
  const remove = (i: number) => setDraft((d) => d.filter((_, j) => j !== i));

  const save = async () => {
    if (owners === 0 || saving) return;
    setSaving(true);
    try {
      const r = await knowledgeApi.setAcl(collection.id, revision, draft);
      setRevision(r.revision);
      setSaved(draft);
      toast.success(t('kb.perm.saved'));
      onChanged();
    } catch (e) {
      // The gateway's stale-revision refusal asks to reload.
      const raw = e instanceof Error ? e.message : String(e);
      if (raw.includes('重新載入')) setConflict(true);
      toast.error(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_320px]">
      <section className="space-y-3" aria-labelledby="kb-perm-current">
        <h2 id="kb-perm-current" className="text-sm font-medium">
          {t('kb.perm.current')}
        </h2>
        <ul className="divide-y divide-surface-border overflow-hidden rounded-xl border border-surface-border">
          {draft.map((e, i) => {
            const isLastOwner = e.kind === 'user' && e.role === 'owner' && owners === 1;
            const label = labelOf(e);
            const Icon = e.kind === 'agent' ? null : KIND_ICON[e.kind];
            return (
              <li key={`${e.kind}:${e.id}`} className="flex flex-wrap items-center gap-2 px-3 py-2.5 sm:flex-nowrap">
                {Icon ? (
                  <Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                ) : (
                  <ActorAvatar actorType="agent" name={label} size="sm" />
                )}
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm" title={label}>
                    {label}
                  </span>
                  <span className="block text-xs text-muted-foreground">{t(`kb.principal.kind.${e.kind}`)}</span>
                </span>
                <select
                  className={cn(selectClass, 'w-32 shrink-0')}
                  aria-label={t('kb.perm.roleFor', { name: label })}
                  value={e.role}
                  disabled={isLastOwner}
                  onChange={(ev) => setRole(i, ev.target.value as KbRole)}
                >
                  {KB_ROLES.map((r) => (
                    <option key={r} value={r} disabled={!allowedRoles(e.kind).includes(r)}>
                      {t(`kb.role.${r}`)}
                    </option>
                  ))}
                </select>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  disabled={isLastOwner}
                  title={isLastOwner ? t('kb.perm.lastOwner') : undefined}
                  aria-label={t('kb.perm.remove', { name: label })}
                  onClick={() => remove(i)}
                >
                  <Trash2 />
                </Button>
              </li>
            );
          })}
        </ul>
        <p className="text-xs text-muted-foreground">{t('kb.perm.rolesHelp')}</p>
        {opensChannel && (
          <p role="alert" className="flex items-start gap-2 rounded-lg bg-amber-500/12 px-3 py-2 text-sm text-amber-700 dark:text-amber-400">
            <AlertTriangle aria-hidden className="mt-0.5 size-4 shrink-0" />
            {t('kb.perm.channelWarning')}
          </p>
        )}
        {conflict && (
          <div role="alert" className="flex flex-wrap items-center gap-2 rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">
            <span className="flex-1">{t('kb.perm.conflict')}</span>
            <Button variant="outline" size="sm" onClick={() => void load()}>
              {t('kb.perm.reload')}
            </Button>
          </div>
        )}
        <div className="flex flex-wrap items-center justify-end gap-2">
          {dirty && (
            <Button variant="ghost" size="sm" onClick={() => setDraft(saved)} disabled={saving}>
              {t('kb.perm.discard')}
            </Button>
          )}
          <Button variant="brand" size="sm" onClick={() => void save()} disabled={!dirty || saving || owners === 0}>
            {saving && <Loader2 aria-hidden className="motion-safe:animate-spin" />}
            {t('kb.perm.save')}
          </Button>
        </div>
      </section>
      <section className="space-y-3" aria-labelledby="kb-perm-add">
        <h2 id="kb-perm-add" className="text-sm font-medium">
          {t('kb.perm.add')}
        </h2>
        <PrincipalPicker collectionId={collection.id} existing={draft} onPick={add} />
      </section>
    </div>
  );
}
