import { useIntl } from 'react-intl';
import { LayoutGrid } from 'lucide-react';
import { useAuthStore } from '@/platform';
import { useSystemStore } from '@/stores/system-store';
import { useOpenApp } from '@/platform/navigate';
import { APPS, isAppVisible, isNewFeature, type AppManifest } from '@/apps/registry';
import { CollectionPageHeader, CollectionPageState, Card, CardContent, Badge } from '@/components/mds';

/**
 * Launcher (`/launcher`, N-1 — `DESIGN-agent-os-native-apps-2026-08.md` §2 L3
 * + §5 WP N-1). An app grid driven entirely by the registry: every card here
 * is one `AppManifest` from `@/apps/registry`, filtered by the viewer's role
 * (§3 C8 role gating, mirroring `nav-visibility.ts`'s fail-closed posture) and
 * opened via the shared `openApp` deep-link helper — the exact same seam N-2's
 * on-box shell will use for its per-app windows.
 *
 * Deliberately NOT a nav item (per this task's scope: "launcher 不是 nav 項，
 * 是 shell 桌面的前身") — it's reachable at `/launcher` for anyone who types
 * the URL or follows a future entry point, and works identically on remote
 * web today. Whether it becomes the appliance's default landing page is a
 * separate decision (N-2).
 *
 * `apps` is overridable for testing (default: the live registry) — this file
 * ships zero apps with `newIn` set today, so the badge-render path needs a
 * synthetic manifest to exercise in tests.
 */
export function LauncherPage({ apps = APPS }: { apps?: readonly AppManifest[] } = {}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const role = useAuthStore((s) => s.user?.role);
  const version = useSystemStore((s) => s.status?.version ?? null);
  const openApp = useOpenApp();

  // None of the seven apps are appliance-only yet (registry.ts's applianceOnly
  // doc comment) — every current filter decision rests on `minRole` alone, so
  // this page doesn't need to probe `device.status` (unlike `useIsAppliance`'s
  // callers) just to render the grid.
  const visible = apps.filter((app) => isAppVisible(app, role));

  return (
    <div className="flex h-full flex-col">
      <CollectionPageHeader
        icon={LayoutGrid}
        title={t('launcher.title')}
        count={visible.length}
        description={t('launcher.subtitle')}
      />

      <div className="flex-1 overflow-y-auto p-5">
        {visible.length === 0 ? (
          <CollectionPageState
            state="empty"
            icon={LayoutGrid}
            title={t('launcher.empty.title')}
            description={t('launcher.empty.desc')}
          />
        ) : (
          <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
            {visible.map((app) => (
              <AppCard
                key={app.id}
                app={app}
                showNew={isNewFeature(app.newIn, version)}
                onOpen={() => openApp(app.id)}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function AppCard({ app, showNew, onOpen }: { app: AppManifest; showNew: boolean; onOpen: () => void }) {
  const intl = useIntl();
  const Icon = app.icon;
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
          <h3 className="truncate text-sm font-medium">{intl.formatMessage({ id: app.nameId })}</h3>
          <p className="mt-0.5 line-clamp-2 text-xs text-muted-foreground">
            {intl.formatMessage({ id: app.descId })}
          </p>
        </div>
      </CardContent>
    </Card>
  );
}
