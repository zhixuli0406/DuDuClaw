import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { FileText, Library, Plus, ShieldAlert } from 'lucide-react';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CollectionPageHeader,
  CollectionPageState,
  ErrorState,
} from '@/components/mds';
import { EmbeddingStatusBanner } from '@/components/knowledge/EmbeddingStatusBanner';
import { useAuthStore } from '@/stores/auth-store';
import { knowledgeApi, type KbCollection } from '@/lib/knowledge-api';
import { CreateCollectionDialog } from './knowledge/CreateCollectionDialog';
import { DeniedReadsSheet } from './knowledge/DeniedReadsSheet';
import { SensitivityBadge } from './knowledge/labels';

/**
 * 文件知識庫 — the collections ("notebooks") this person can see. Each card
 * opens the collection: its sources, a search preview, sharing and the
 * access record. What is listed is decided by the gateway per collection
 * grant, so a person only ever sees collections shared with them.
 */
export function KnowledgeBasePage() {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const navigate = useNavigate();
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  const [collections, setCollections] = useState<KbCollection[] | null>(null);
  const [canCreate, setCanCreate] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [creating, setCreating] = useState(false);
  const [showDenied, setShowDenied] = useState(false);

  const load = useCallback(async () => {
    try {
      setError(null);
      const r = await knowledgeApi.listCollections();
      setCollections(r.collections);
      setCanCreate(r.can_create);
    } catch (e) {
      setError(e);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const createButton = canCreate ? (
    <Button variant="brand" size="sm" onClick={() => setCreating(true)}>
      <Plus aria-hidden />
      <span className="sr-only sm:not-sr-only">{t('kb.create')}</span>
    </Button>
  ) : null;

  return (
    <div className="flex h-full flex-col">
      <CollectionPageHeader
        icon={Library}
        title={t('nav.knowledgeBase')}
        count={collections?.length}
        description={t('kb.subtitle')}
        action={
          <>
            {isAdmin && (
              <Button variant="ghost" size="sm" onClick={() => setShowDenied(true)}>
                <ShieldAlert aria-hidden />
                <span className="sr-only sm:not-sr-only">{t('kb.denied.open')}</span>
              </Button>
            )}
            {createButton}
          </>
        }
      />
      <div className="flex-1 space-y-4 overflow-y-auto px-4 py-5 sm:px-6">
        <EmbeddingStatusBanner />
        {collections === null && error != null ? (
          <ErrorState icon={Library} error={error} onRetry={() => void load()} />
        ) : collections === null ? (
          <CollectionPageState state="loading" title={t('nav.knowledgeBase')} />
        ) : collections.length === 0 ? (
          <CollectionPageState
            state="empty"
            icon={Library}
            title={t('kb.empty.title')}
            description={canCreate ? t('kb.empty.descCreate') : t('kb.empty.descAsk')}
            action={createButton}
          />
        ) : (
          <ul className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
            {collections.map((c) => (
              <li key={c.id}>
                <CollectionCard collection={c} onOpen={() => navigate(`/knowledge-base/${encodeURIComponent(c.id)}`)} />
              </li>
            ))}
          </ul>
        )}
      </div>
      {creating && (
        <CreateCollectionDialog
          onClose={() => setCreating(false)}
          onCreated={(c) => {
            setCreating(false);
            navigate(`/knowledge-base/${encodeURIComponent(c.id)}`);
          }}
        />
      )}
      {isAdmin && <DeniedReadsSheet open={showDenied} onClose={() => setShowDenied(false)} />}
    </div>
  );
}

function CollectionCard({ collection: c, onOpen }: { collection: KbCollection; onOpen: () => void }) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const pending = c.source_count - c.ready_count;
  return (
    <Card className="relative h-full gap-3 transition-colors hover:bg-surface-hover">
      <CardContent className="flex h-full flex-col gap-3">
        <div className="min-w-0">
          <h2 className="text-base font-medium leading-snug">
            {/* The whole card is clickable through this one real button. */}
            <button
              type="button"
              onClick={onOpen}
              className="w-full truncate rounded-sm text-left outline-none after:absolute after:inset-0 focus-visible:ring-3 focus-visible:ring-ring/50"
              title={c.name}
            >
              {c.name}
            </button>
          </h2>
          {c.description && <p className="mt-1 line-clamp-2 text-sm text-muted-foreground">{c.description}</p>}
        </div>
        <div className="mt-auto flex flex-wrap items-center gap-1.5">
          <SensitivityBadge value={c.sensitivity} />
          {c.my_role && <Badge variant="outline">{t(`kb.role.${c.my_role}`)}</Badge>}
          {c.department && <Badge variant="ghost">{c.department}</Badge>}
        </div>
        <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <FileText aria-hidden className="size-3.5" />
          {t('kb.card.sources', { count: c.source_count })}
          {pending > 0 && <span>· {t('kb.card.pending', { count: pending })}</span>}
        </p>
      </CardContent>
    </Card>
  );
}
