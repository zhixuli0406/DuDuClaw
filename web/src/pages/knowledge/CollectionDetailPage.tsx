import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useParams, useSearchParams } from 'react-router';
import { Library } from 'lucide-react';
import { Badge, BreadcrumbHeader, ErrorState, Tabs, TabsList, TabsPanel, TabsTab } from '@/components/mds';
import { CollectionPageState } from '@/components/mds';
import { knowledgeApi, type KbCollection } from '@/lib/knowledge-api';
import { SensitivityBadge } from './labels';
import { SourcesTab } from './SourcesTab';
import { SearchPreviewTab } from './SearchPreviewTab';
import { PermissionsTab } from './PermissionsTab';
import { AccessLogTab } from './AccessLogTab';
import { CollectionSettingsTab } from './CollectionSettingsTab';

const TABS = ['sources', 'search', 'permissions', 'log', 'settings'] as const;
type Tab = (typeof TABS)[number];
const OWNER_TABS: ReadonlySet<Tab> = new Set(['log', 'settings']);

/** `?tab=` → a tab this person may open (fallback: 來源). */
export function resolveTab(raw: string | null, isOwner: boolean): Tab {
  const tab = (TABS as readonly string[]).includes(raw ?? '') ? (raw as Tab) : 'sources';
  return OWNER_TABS.has(tab) && !isOwner ? 'sources' : tab;
}

/**
 * One collection (notebook): its sources, a search preview, who it is
 * shared with, who has read it, and its settings. Tabs the person's role
 * does not cover are not offered; the gateway refuses them anyway.
 */
export function CollectionDetailPage() {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const navigate = useNavigate();
  const { id = '' } = useParams<{ id: string }>();
  const [params, setParams] = useSearchParams();
  const [collection, setCollection] = useState<KbCollection | null>(null);
  const [error, setError] = useState<unknown>(null);

  const load = useCallback(async () => {
    try {
      setError(null);
      const r = await knowledgeApi.getCollection(id);
      setCollection(r.collection);
    } catch (e) {
      setError(e);
    }
  }, [id]);

  useEffect(() => {
    void load();
  }, [load]);

  const role = collection?.my_role ?? null;
  const isOwner = role === 'owner';
  const canEdit = role === 'owner' || role === 'editor';
  const tab = resolveTab(params.get('tab'), isOwner);
  const setTab = (next: string) => {
    const p = new URLSearchParams(params);
    p.set('tab', next);
    setParams(p, { replace: true });
  };

  const segments = useMemo(
    () => [
      { label: t('nav.knowledgeBase'), onClick: () => navigate('/knowledge-base'), href: '/knowledge-base' },
      { label: collection?.name ?? '…' },
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [collection?.name, intl],
  );

  if (error != null && !collection) {
    return (
      <div className="flex h-full flex-col">
        <BreadcrumbHeader segments={segments} />
        <div className="p-4 sm:p-6">
          <ErrorState icon={Library} title={t('kb.detail.loadError')} error={error} onRetry={() => void load()} />
        </div>
      </div>
    );
  }
  if (!collection) {
    return (
      <div className="flex h-full flex-col">
        <BreadcrumbHeader segments={segments} />
        <CollectionPageState state="loading" title={t('nav.knowledgeBase')} />
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      <BreadcrumbHeader segments={segments} />
      <div className="flex-1 overflow-y-auto">
        <div className="border-b border-surface-border px-4 pt-4 pb-3 sm:px-6">
          <div className="mx-auto w-full max-w-5xl">
            <h1 className="text-xl font-semibold break-words sm:text-2xl">{collection.name}</h1>
            {collection.description && (
              <p className="mt-1 text-sm text-muted-foreground">{collection.description}</p>
            )}
            <div className="mt-2 flex flex-wrap items-center gap-1.5">
              <SensitivityBadge value={collection.sensitivity} />
              {role && <Badge variant="outline">{t(`kb.role.${role}`)}</Badge>}
              <span className="font-mono text-xs tabular-nums text-muted-foreground">
                {t('kb.card.sources', { count: collection.source_count })}
              </span>
            </div>
          </div>
        </div>
        <Tabs value={tab} onValueChange={(v) => setTab(String(v))} variant="line" className="gap-0">
          <div className="border-b border-surface-border px-4 sm:px-6">
            <div className="mx-auto w-full max-w-5xl overflow-x-auto">
              <TabsList className="h-11">
                <TabsTab value="sources">{t('kb.tab.sources')}</TabsTab>
                <TabsTab value="search">{t('kb.tab.search')}</TabsTab>
                <TabsTab value="permissions">{t('kb.tab.permissions')}</TabsTab>
                {isOwner && <TabsTab value="log">{t('kb.tab.log')}</TabsTab>}
                {isOwner && <TabsTab value="settings">{t('kb.tab.settings')}</TabsTab>}
              </TabsList>
            </div>
          </div>
          <div className="px-4 py-5 sm:px-6">
            <div className="mx-auto w-full max-w-5xl">
              <TabsPanel value="sources">
                <SourcesTab collection={collection} canEdit={canEdit} onChanged={() => void load()} />
              </TabsPanel>
              <TabsPanel value="search">
                <SearchPreviewTab collection={collection} />
              </TabsPanel>
              <TabsPanel value="permissions">
                <PermissionsTab collection={collection} isOwner={isOwner} onChanged={() => void load()} />
              </TabsPanel>
              {isOwner && (
                <TabsPanel value="log">
                  <AccessLogTab collectionId={collection.id} />
                </TabsPanel>
              )}
              {isOwner && (
                <TabsPanel value="settings">
                  <CollectionSettingsTab
                    collection={collection}
                    onSaved={setCollection}
                    onDeleted={() => navigate('/knowledge-base', { replace: true })}
                  />
                </TabsPanel>
              )}
            </div>
          </div>
        </Tabs>
      </div>
    </div>
  );
}
