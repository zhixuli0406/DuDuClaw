import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { api, type GalleryCard as GalleryCardData } from '@/lib/api';
import { useAssignStore } from '@/stores/assign-store';
import {
  CollectionPageHeader,
  CollectionPageState,
  ErrorState,
  Card,
  CardContent,
  Badge,
  Button,
} from '@/components/mds';
import { Images, Sparkles, Users, ArrowRight } from 'lucide-react';

/** WP-ORG catalog section order — same slugs `experts.catalog` uses, so the
 *  gallery groups its cards exactly the way the AI-team catalog does. */
const CATEGORY_ORDER = ['health', 'professional', 'retail', 'lifestyle', 'education', 'other'] as const;

/** First clause of a team task example (split on the common zh-TW list
 *  separators) — used as the card's short headline. Falls back to the whole
 *  string when no delimiter is found, capped at 24 code points (CJK-safe:
 *  `Array.from` splits by codepoint, never by UTF-16 code unit) so one very
 *  long single-clause worker summary cannot blow out the card layout. */
function cardTitle(example: string): string {
  const seg = example.split(/[、，；;,]/)[0]?.trim() || example.trim();
  const chars = Array.from(seg);
  return chars.length > 24 ? `${chars.slice(0, 24).join('')}…` : seg;
}

/**
 * GalleryPage (`/gallery`, P2-b MVP) — "靈感畫廊": curated showcase cards, one
 * per team task example (`gallery.list`, a straight fan-out of the same
 * `team.toml` data `/experts` reads — no new storage). Each card's "做同款"
 * action opens the single 交辦 panel (`useAssignStore`) prefilled with an
 * editable description + an outcome-style acceptance criterion, so trying an
 * idea costs one click instead of a blank page.
 *
 * Deliberately out of scope for this wave (tracked in
 * `DESIGN-dashboard-ux-workbuddy-2026-08.md` §3.5): a "我的" tab of a user's
 * own completed goal runs, and any cross-user/public sharing — both depend on
 * artifact objectification (I-2b), which has not shipped yet.
 */
export function GalleryPage() {
  const intl = useIntl();
  const t = useCallback((id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values), [intl]);
  const openAssign = useAssignStore((s) => s.openAssign);

  const [cards, setCards] = useState<GalleryCardData[] | null>(null);
  const [deployed, setDeployed] = useState(false);
  const [presentButLocked, setPresentButLocked] = useState(false);
  const [loadError, setLoadError] = useState<unknown>(null);

  const load = useCallback(async () => {
    try {
      const res = await api.gallery.list();
      setDeployed(res.deployed);
      setPresentButLocked(res.present_but_locked);
      setCards(res.cards ?? []);
      setLoadError(null);
    } catch (e) {
      setLoadError(e);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const handleRemake = (card: GalleryCardData) => {
    openAssign({
      agentId: card.lead_agent_name ?? undefined,
      description: t('gallery.remake.description', { example: card.example }),
      acceptanceCriteria: t('gallery.remake.acceptance', { example: card.example }),
      mode: 'assign',
    });
  };

  return (
    <div className="flex h-full flex-col">
      <CollectionPageHeader
        icon={Images}
        title={t('gallery.title')}
        count={cards?.length}
        description={t('gallery.subtitle')}
      />

      <div className="flex-1 overflow-y-auto p-5">
        {cards === null && loadError != null ? (
          <ErrorState icon={Images} error={loadError} onRetry={() => void load()} />
        ) : cards === null ? (
          <CollectionPageState state="loading" title={t('gallery.title')} />
        ) : !deployed || presentButLocked ? (
          <CollectionPageState
            state="empty"
            icon={Images}
            title={t('gallery.empty.title')}
            description={presentButLocked ? t('gallery.locked') : t('gallery.notDeployed')}
          />
        ) : cards.length === 0 ? (
          <CollectionPageState
            state="empty"
            icon={Images}
            title={t('gallery.empty.title')}
            description={t('gallery.empty.desc')}
          />
        ) : (
          CATEGORY_ORDER.map((category) => {
            const entries = cards.filter(
              (c) => (CATEGORY_ORDER.includes(c.category as (typeof CATEGORY_ORDER)[number]) ? c.category : 'other') === category,
            );
            if (entries.length === 0) return null;
            return (
              <section key={category} className="mb-6 space-y-2">
                <h3 className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                  {t(`experts.category.${category}`)}
                </h3>
                <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
                  {entries.map((card) => (
                    <GalleryCardView key={card.id} card={card} onRemake={() => handleRemake(card)} />
                  ))}
                </div>
              </section>
            );
          })
        )}
      </div>
    </div>
  );
}

function GalleryCardView({ card, onRemake }: { card: GalleryCardData; onRemake: () => void }) {
  const intl = useIntl();
  const navigate = useNavigate();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const fitFor =
    card.departments.length > 0
      ? t('gallery.card.fitForDept', { depts: intl.formatList(card.departments, { type: 'conjunction' }) })
      : t('gallery.card.fitForGeneric', { team: card.team_label });

  return (
    <Card className="gap-3">
      <CardContent className="space-y-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <Sparkles className="size-3.5 shrink-0 text-brand" aria-hidden="true" />
            <h3 className="truncate text-sm font-medium">{cardTitle(card.example)}</h3>
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-1.5">
            <Badge variant="ghost">{card.team_label}</Badge>
          </div>
        </div>

        <p className="text-xs text-muted-foreground">
          {t('gallery.card.outcome', { example: card.example })}
        </p>

        <p className="flex items-start gap-1.5 text-xs text-muted-foreground">
          <Users className="mt-0.5 size-3 shrink-0" aria-hidden="true" />
          {fitFor}
        </p>

        <div className="flex items-center justify-end">
          {card.team_installed ? (
            <Button variant="brand" size="sm" onClick={onRemake}>
              <Sparkles />
              {t('gallery.card.remake')}
            </Button>
          ) : (
            <Button variant="outline" size="sm" onClick={() => navigate('/experts')}>
              {t('gallery.card.joinTeamFirst')}
              <ArrowRight />
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
