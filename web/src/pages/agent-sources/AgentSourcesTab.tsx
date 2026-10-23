import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { AlertTriangle, ArrowRight, FileText, Info, Library, Loader2, Plus } from 'lucide-react';
import { Badge, Button, Card, CardContent, CardHeader, CardTitle, Empty, ErrorState } from '@/components/mds';
import { toast, formatError } from '@/lib/toast';
import {
  knowledgeApi,
  type KbAgentDataSources,
  type KbAgentTabCollection,
  type KbCollection,
} from '@/lib/knowledge-api';
import { SensitivityBadge } from '@/pages/knowledge/labels';
import { AddSourceDialog } from '@/pages/knowledge/AddSourceDialog';
import { CreateOwnDialog } from './CreateOwnDialog';
import { OwnFiles, anyBusy } from './OwnFiles';
import { hasChannelReader, readersSentence } from './readers';

/** How often the tab refreshes while a file is still being read. */
export const POLL_MS = 3000;

/**
 * 資料來源 — what this AI employee can look up in the document library.
 * Its own files come first (added and removed right here); collections
 * other people shared with it follow. Everyone who can view the employee
 * sees the tab; only its owners (and Admins) change files.
 */
export function AgentSourcesTab({ agentId, agentName }: { agentId: string; agentName: string }) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const [data, setData] = useState<KbAgentDataSources | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [creating, setCreating] = useState(false);
  const [adding, setAdding] = useState<KbCollection | null>(null);
  const [opening, setOpening] = useState(false);

  const load = useCallback(async () => {
    try {
      setData(await knowledgeApi.agentDataSources(agentId));
      setError(null);
    } catch (e) {
      setError(e);
    }
  }, [agentId]);

  useEffect(() => {
    void load();
  }, [load]);

  const busy = anyBusy(data?.own?.sources);
  useEffect(() => {
    if (!busy) return;
    const h = setInterval(() => {
      if (typeof document === 'undefined' || document.visibilityState !== 'hidden') void load();
    }, POLL_MS);
    return () => clearInterval(h);
  }, [busy, load]);

  // The own collection exists: confirm it (and the employee's settings
  // using it) once more, then open the upload dialog on it.
  const openAdd = async () => {
    if (!data?.own) {
      setCreating(true);
      return;
    }
    setOpening(true);
    try {
      const r = await knowledgeApi.ensureOwnCollection(agentId);
      setAdding(r.collection);
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setOpening(false);
    }
  };

  if (data === null) {
    return error != null ? (
      <ErrorState error={error} onRetry={() => void load()} />
    ) : (
      <div className="flex justify-center py-10">
        <Loader2 aria-hidden className="size-5 text-muted-foreground motion-safe:animate-spin" />
        <span className="sr-only">{t('common.loading')}</span>
      </div>
    );
  }

  const others = data.collections.filter((c) => !c.is_own);
  return (
    <div className="space-y-4">
      <PrivacyNote />
      {data.binding === 'off' && <Notice tone="warn" text={t('agentSources.binding.off')} />}
      {data.binding === 'unreadable' && <Notice tone="warn" text={t('agentSources.binding.unreadable')} />}
      <OwnSection data={data} agentName={agentName} opening={opening} onAdd={() => void openAdd()} onChanged={() => void load()} />
      <SharedSection collections={others} self={{ id: agentId, name: agentName }} />
      {creating && (
        <CreateOwnDialog
          agentId={agentId}
          agentName={agentName}
          defaults={data.default_readers}
          onClose={() => setCreating(false)}
          onReady={(c, bindingChanged) => {
            setCreating(false);
            if (bindingChanged) toast.info(t('agentSources.create.bindingChanged'));
            setAdding(c);
            void load();
          }}
        />
      )}
      {adding && (
        <AddSourceDialog
          collection={adding}
          onClose={() => setAdding(null)}
          onAdded={() => void load()}
        />
      )}
    </div>
  );
}

function PrivacyNote() {
  const intl = useIntl();
  return (
    <div className="flex items-start gap-2 rounded-xl border border-surface-border bg-muted/40 p-3 text-sm" role="note">
      <Info aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
      <p className="text-muted-foreground">{intl.formatMessage({ id: 'agentSources.privacy' })}</p>
    </div>
  );
}

function Notice({ tone, text }: { tone: 'warn' | 'info'; text: string }) {
  return (
    <p
      role="status"
      className={
        tone === 'warn'
          ? 'flex items-start gap-2 rounded-lg bg-amber-500/10 p-3 text-sm text-amber-800 dark:text-amber-300'
          : 'flex items-start gap-2 rounded-lg bg-muted p-3 text-sm text-muted-foreground'
      }
    >
      <AlertTriangle aria-hidden className="mt-0.5 size-4 shrink-0" />
      {text}
    </p>
  );
}

/** Why the signed-in person cannot change files, in plain words. */
function blockedText(intl: ReturnType<typeof useIntl>, data: KbAgentDataSources): string | null {
  switch (data.manage_blocked) {
    case 'archived':
      return intl.formatMessage({ id: 'agentSources.blocked.archived' });
    case 'settings_unreadable':
      return intl.formatMessage({ id: 'agentSources.blocked.settingsUnreadable' });
    case 'not_collection_editor':
      return intl.formatMessage({ id: 'agentSources.blocked.notCollectionEditor' });
    default:
      return null;
  }
}

function OwnSection({
  data,
  agentName,
  opening,
  onAdd,
  onChanged,
}: {
  data: KbAgentDataSources;
  agentName: string;
  opening: boolean;
  onAdd: () => void;
  onChanged: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const navigate = useNavigate();
  const own = data.own;
  const blocked = blockedText(intl, data);
  const addButton = data.can_manage && (
    <Button variant="brand" size="sm" onClick={onAdd} disabled={opening}>
      <Plus aria-hidden />
      {t('agentSources.add')}
    </Button>
  );
  return (
    <Card>
      <CardHeader className="flex flex-row flex-wrap items-center justify-between gap-2">
        <CardTitle className="text-base">{t('agentSources.own.title', { name: agentName })}</CardTitle>
        {own && addButton}
      </CardHeader>
      <CardContent className="space-y-3">
        {blocked && <Notice tone="info" text={blocked} />}
        {!own ? (
          <Empty
            icon={FileText}
            title={t('agentSources.own.empty')}
            description={data.can_manage ? t('agentSources.own.emptyManage') : t('agentSources.own.emptyView')}
            action={addButton || undefined}
          />
        ) : (
          <>
            <div className="flex flex-wrap items-center gap-2 text-sm">
              <span className="font-medium">{own.collection.name}</span>
              <SensitivityBadge value={own.collection.sensitivity} />
              {own.archived && <Badge variant="ghost">{t('agentSources.own.archived')}</Badge>}
              {!own.archived && !own.collection.in_use && (
                <Badge variant="ghost">{t('agentSources.shared.notInUse')}</Badge>
              )}
            </div>
            <ReadersLine c={own.collection} self={{ id: data.agent_id, name: agentName }} />
            {own.sources === null ? (
              <p className="text-sm text-muted-foreground">{t('agentSources.own.notReader')}</p>
            ) : own.sources.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t('agentSources.own.noFiles')}</p>
            ) : (
              <OwnFiles sources={own.sources} canManage={data.can_manage} onChanged={onChanged} />
            )}
            {own.collection.my_role && (
              <Button variant="link" size="sm" className="px-0" onClick={() => navigate(`/knowledge-base/${encodeURIComponent(own.collection.id)}`)}>
                <Library aria-hidden />
                {t('agentSources.own.manage')}
                <ArrowRight aria-hidden />
              </Button>
            )}
          </>
        )}
      </CardContent>
    </Card>
  );
}

function ReadersLine({ c, self }: { c: KbAgentTabCollection; self?: { id: string; name: string } }) {
  const intl = useIntl();
  return (
    <div className="space-y-1">
      <p className="text-xs text-muted-foreground">{readersSentence(intl, c.readers, c.reader_counts, self)}</p>
      {hasChannelReader(c.readers, c.reader_counts) && (
        <p className="text-xs text-amber-800 dark:text-amber-300">{intl.formatMessage({ id: 'agentSources.readers.channelWarning' })}</p>
      )}
    </div>
  );
}

function SharedSection({ collections, self }: { collections: KbAgentTabCollection[]; self: { id: string; name: string } }) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const navigate = useNavigate();
  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-base">{t('agentSources.shared.title')}</CardTitle>
      </CardHeader>
      <CardContent>
        {collections.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t('agentSources.shared.empty')}</p>
        ) : (
          <ul className="divide-y divide-surface-border" data-testid="agent-shared-collections">
            {collections.map((c) => (
              <li key={c.id} className="space-y-1 py-3 first:pt-0 last:pb-0">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-sm font-medium">{c.name}</span>
                  <SensitivityBadge value={c.sensitivity} />
                  {!c.in_use && <Badge variant="ghost">{t('agentSources.shared.notInUse')}</Badge>}
                  <span className="text-xs text-muted-foreground">{t('kb.card.sources', { count: c.source_count })}</span>
                </div>
                <ReadersLine c={c} self={self} />
                {c.my_role && (
                  <Button variant="link" size="sm" className="h-auto px-0" onClick={() => navigate(`/knowledge-base/${encodeURIComponent(c.id)}`)}>
                    {t('agentSources.shared.open')}
                    <ArrowRight aria-hidden />
                  </Button>
                )}
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
