import { useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { Settings, HardDrive, Download, LogIn, Shield, KeyRound } from 'lucide-react';
import { CollectionPageHeader, Card, CardContent } from '@/components/mds';

/**
 * SystemHomePage — `/app/system` bare index (N-3,
 * `DESIGN-agent-os-native-apps-2026-08.md` §5 WP N-3: "/app/system 給一個設定
 * 首頁（分區卡片列表，MDS 慣例，類 macOS 系統設定的入口感）"). The registry's
 * `system.defaultPath` now points here instead of at `SettingsPage` — this is
 * a real landing page, not a redirect stand-in.
 *
 * Card content deliberately reuses the SAME i18n keys the pages already carry
 * as `manage.*`/`nav.device` nav-model entries (label + `.desc`) — the six
 * pages did not get new names when they moved app, so this hub should not
 * invent second ones. Only the section headers below are new copy (there was
 * no existing "group of settings pages" concept to reuse).
 */

interface SystemCardDef {
  readonly to: string;
  readonly icon: typeof Settings;
  readonly titleId: string;
  readonly descId: string;
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
              {section.cards.map((card) => (
                <SystemCard key={card.to} card={card} onOpen={() => navigate(card.to)} />
              ))}
            </div>
          </section>
        ))}
      </div>
    </div>
  );
}

function SystemCard({ card, onOpen }: { card: SystemCardDef; onOpen: () => void }) {
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
        <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-brand/10 text-brand">
          <Icon className="size-4.5" aria-hidden="true" />
        </span>
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
