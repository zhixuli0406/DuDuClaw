import { useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { Settings, HardDrive, Download, LogIn, Shield, KeyRound, GitBranch, FlaskConical, Database } from 'lucide-react';
import { CollectionPageHeader, Card, CardContent, Badge } from '@/components/mds';
import { useSystemStore } from '@/stores/system-store';
import { useDecisionEnabled } from '@/hooks/useDecisionEnabled';
import { isNewFeature } from '@/apps/registry';

/**
 * SystemHomePage — `/app/system` bare index (N-3,
 * `DESIGN-agent-os-native-apps-2026-08.md` §5 WP N-3: "/app/system 給一個設定
 * 首頁（分區卡片列表，MDS 慣例，類 macOS 系統設定的入口感）"). The registry's
 * `system.defaultPath` now points here instead of at `SettingsPage` — this is
 * a real landing page, not a redirect stand-in.
 *
 * Card content deliberately reuses the SAME i18n keys the pages already carry
 * as `manage.*`/`nav.device` nav-model entries (label + `.desc`) — the six
 * migrated pages did not get new names when they moved app, so this hub should not
 * invent second ones. Only the section headers below are new copy (there was
 * no existing "group of settings pages" concept to reuse).
 */

interface SystemCardDef {
  readonly to: string;
  readonly icon: typeof Settings;
  readonly titleId: string;
  readonly descId: string;
  /** 新功能 chip convention (`nav-model.ts`'s `isNewFeature` doc) — set only
   * on the release a card is added in; never mutate an existing card's value. */
  readonly newIn?: string;
  /** Operator kill switch, mirroring `Gated.requiresFeature` in
   * `nav-visibility.ts`: `'decision'` hides the card while `config.toml
   * [decision] enabled = false`. Fail-open — an older gateway that reports no
   * `decision_enabled` keeps the card (see `useDecisionEnabled`). */
  readonly requiresFeature?: 'decision';
}

interface SystemSection {
  readonly labelId: string;
  readonly cards: readonly SystemCardDef[];
}

const SECTIONS: readonly SystemSection[] = [
  {
    labelId: 'systemHome.section.hardware',
    cards: [
      { to: '/app/system/device', icon: HardDrive, titleId: 'nav.device', descId: 'nav.device.desc' },
      { to: '/app/system/updates', icon: Download, titleId: 'manage.updates', descId: 'manage.updates.desc' },
    ],
  },
  {
    labelId: 'systemHome.section.access',
    cards: [
      { to: '/app/system/accounts', icon: LogIn, titleId: 'manage.accounts', descId: 'manage.accounts.desc' },
      { to: '/app/system/security', icon: Shield, titleId: 'manage.security', descId: 'manage.security.desc' },
      { to: '/app/system/causal', icon: GitBranch, titleId: 'causalCuration.title', descId: 'causalCuration.desc', newIn: '1.66.0' },
      { to: '/app/system/decision-lab', icon: FlaskConical, titleId: 'decisionLab.title', descId: 'decisionLab.cardDescription', newIn: '1.66.0', requiresFeature: 'decision' },
      { to: '/app/system/ccr', icon: Database, titleId: 'ccrDashboard.title', descId: 'ccrDashboard.cardDescription', newIn: '1.66.0' },
      { to: '/app/system/license', icon: KeyRound, titleId: 'manage.license', descId: 'manage.license.desc' },
    ],
  },
  {
    labelId: 'systemHome.section.general',
    cards: [
      { to: '/app/system/settings', icon: Settings, titleId: 'manage.system', descId: 'manage.system.desc' },
    ],
  },
];

export function SystemHomePage() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const navigate = useNavigate();
  const version = useSystemStore((s) => s.status?.version ?? null);
  // X1 (2026-09-29): a card for a surface the operator turned off would only
  // lead to 404s, so it leaves the grid with its nav row.
  const decisionEnabled = useDecisionEnabled();
  const cardVisible = (card: SystemCardDef) =>
    !(card.requiresFeature === 'decision' && !decisionEnabled);

  return (
    <div className="flex h-full flex-col">
      <CollectionPageHeader icon={Settings} title={t('app.system.name')} description={t('app.system.desc')} />

      <div className="flex-1 space-y-6 overflow-y-auto p-5">
        {SECTIONS.map((section) => (
          <section key={section.labelId} aria-labelledby={section.labelId}>
            <h2
              id={section.labelId}
              className="mb-2 px-0.5 text-xs font-medium tracking-wide text-muted-foreground uppercase"
            >
              {t(section.labelId)}
            </h2>
            <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
              {section.cards.filter(cardVisible).map((card) => (
                <SystemCard
                  key={card.to}
                  card={card}
                  showNew={isNewFeature(card.newIn, version)}
                  onOpen={() => navigate(card.to)}
                />
              ))}
            </div>
          </section>
        ))}
      </div>
    </div>
  );
}

function SystemCard({
  card,
  showNew,
  onOpen,
}: {
  card: SystemCardDef;
  showNew: boolean;
  onOpen: () => void;
}) {
  const intl = useIntl();
  const Icon = card.icon;
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      onOpen();
    }
  };
  return (
    <Card
      role="button"
      tabIndex={0}
      onClick={onOpen}
      onKeyDown={onKeyDown}
      className="cursor-pointer gap-3 transition-colors hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand"
    >
      <CardContent className="flex flex-col items-start gap-2">
        <div className="flex w-full items-center justify-between gap-2">
          <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-brand/10 text-brand">
            <Icon className="size-4.5" aria-hidden="true" />
          </span>
          {showNew && (
            <Badge variant="ghost" className="shrink-0 bg-brand/15 text-brand">
              {intl.formatMessage({ id: 'nav.badge.new' })}
            </Badge>
          )}
        </div>
        <div className="min-w-0">
          <h3 className="truncate text-sm font-medium">{intl.formatMessage({ id: card.titleId })}</h3>
          <p className="mt-0.5 line-clamp-2 text-xs text-muted-foreground">
            {intl.formatMessage({ id: card.descId })}
          </p>
        </div>
      </CardContent>
    </Card>
  );
}
