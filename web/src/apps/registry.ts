import { Settings, KanbanSquare, Users, Radio, Brain, FolderOpen, Activity } from 'lucide-react';
import type { UserRole } from '@/platform';

/**
 * App registry (N-1, `commercial/docs/DESIGN-agent-os-native-apps-2026-08.md`
 * §2 L3 + §3 C8). The seven apps the L3 table splits the dashboard into. This
 * is the single source of truth an app's identity is defined from — the L2
 * `nav-model.ts` sidebar projection and the `/app/:appId` deep-link route seam
 * both read from here rather than re-declaring the same seven apps.
 *
 * N-1 shipped `defaultPath` pointing at each app's existing, unmoved canonical
 * route, with `/app/:appId/*` (see `AppRouteRedirect.tsx`) as a redirect seam
 * into that route tree rather than a new render target. N-3 (§5) began
 * relocating pages for real, app by app: `system` went first — its six
 * settings pages now render directly under `/app/system/*`
 * (`SystemHomePage.tsx` + `App.tsx`), and every one of their old routes
 * (`/manage/accounts`, `/device`, …) is a `<LegacyRouteRedirect>` back to the
 * new canonical path, query string preserved. The other six apps are
 * untouched by N-3 and still work exactly as N-1 left them — `defaultPath`
 * for those still points at the existing, unmoved page.
 */
export type AppId = 'system' | 'workbench' | 'staff' | 'comms' | 'memory' | 'files' | 'monitor';

export interface AppManifest {
  readonly id: AppId;
  /** i18n message id for the app's display name (launcher card / future shell
   *  window title). Convention mirrors `NavItem.label`: `app.<id>.name`. */
  readonly nameId: string;
  /** i18n message id for a one-line description (launcher card subtitle).
   *  Convention mirrors `NavItem.desc`: `app.<id>.desc`. */
  readonly descId: string;
  readonly icon: typeof Settings;
  /** The app's route namespace — `/app/<id>` (design doc §2 L2 "Deep link"). */
  readonly routePrefix: string;
  /** The existing, unmoved route this app lands on when opened with no
   *  sub-path (`/app/<id>` bare, or `openApp(id)` with no `path`). */
  readonly defaultPath: string;
  /** Floor role to see this app at all (mirrors `manageEntry`'s pattern in
   *  `nav-model.ts`: the loosest gate among the app's pages, not the
   *  strictest — individual destination routes re-gate themselves exactly as
   *  they do today; this is presentation only, same disclaimer as
   *  `nav-visibility.ts`). Absent = every authenticated role. */
  readonly minRole?: UserRole;
  /**
   * True when this app is meaningful ONLY on the DuDuClaw appliance image.
   * None of the seven qualify yet — the appliance-only surface today is one
   * page inside "system" (`/device`, gated via `requiresData: 'appliance'`
   * in `nav-visibility.ts`), not a whole app. The field exists per the L3
   * schema for when an app becomes appliance-exclusive (see N-3/N-4).
   */
  readonly applianceOnly?: boolean;
  /** 新功能 chip convention (`nav-model.ts`'s `isNewFeature` doc) — set only
   *  when the WHOLE app is a new surface, not when one page inside it is. */
  readonly newIn?: string;
}

export const APPS: readonly AppManifest[] = [
  {
    id: 'system',
    nameId: 'app.system.name',
    descId: 'app.system.desc',
    icon: Settings,
    routePrefix: '/app/system',
    // N-3 (`DESIGN-agent-os-native-apps-2026-08.md` §5 WP N-3): the system app
    // is the first of the seven to actually relocate its pages — `/app/system`
    // is now a real settings-hub route (`SystemHomePage`), not a redirect
    // stand-in for `/manage/system` (see `AppRouteRedirect.tsx` + `App.tsx`).
    defaultPath: '/app/system',
    minRole: 'admin',
    applianceOnly: false,
  },
  {
    id: 'workbench',
    nameId: 'app.workbench.name',
    descId: 'app.workbench.desc',
    icon: KanbanSquare,
    routePrefix: '/app/workbench',
    defaultPath: '/chat',
    applianceOnly: false,
  },
  {
    id: 'staff',
    nameId: 'app.staff.name',
    descId: 'app.staff.desc',
    icon: Users,
    routePrefix: '/app/staff',
    defaultPath: '/agents',
    applianceOnly: false,
  },
  {
    id: 'comms',
    nameId: 'app.comms.name',
    descId: 'app.comms.desc',
    icon: Radio,
    routePrefix: '/app/comms',
    defaultPath: '/manage/channels',
    minRole: 'admin',
    applianceOnly: false,
  },
  {
    id: 'memory',
    nameId: 'app.memory.name',
    descId: 'app.memory.desc',
    icon: Brain,
    routePrefix: '/app/memory',
    defaultPath: '/memory',
    applianceOnly: false,
  },
  {
    id: 'files',
    nameId: 'app.files.name',
    descId: 'app.files.desc',
    icon: FolderOpen,
    routePrefix: '/app/files',
    defaultPath: '/files',
    applianceOnly: false,
  },
  {
    id: 'monitor',
    nameId: 'app.monitor.name',
    descId: 'app.monitor.desc',
    icon: Activity,
    routePrefix: '/app/monitor',
    defaultPath: '/reports',
    minRole: 'manager',
    applianceOnly: false,
  },
];

/** O(1) lookup by id, derived from `APPS` (never hand-maintained separately —
 *  a second, independently-edited map is exactly the kind of drift §3 C4
 *  warns about). */
export const APPS_BY_ID: Readonly<Record<AppId, AppManifest>> = Object.fromEntries(
  APPS.map((app) => [app.id, app]),
) as Record<AppId, AppManifest>;

const ROLE_LEVELS: Record<UserRole, number> = { admin: 3, manager: 2, employee: 1 };

/** Extra facts needed to evaluate an app's visibility — same fail-closed
 *  posture as `nav-visibility.ts`'s `VisibilityContext` (an unproven fact
 *  hides the surface, never shows it). */
export interface AppVisibilityContext {
  readonly isAppliance?: boolean;
}

/** Whether `app` should appear in the launcher grid for `userRole`. Mirrors
 *  `nav-visibility.ts::isVisible`'s role/appliance checks (not imported from
 *  there directly: that module's `Gated` also carries `enterprise` /
 *  `personalHidden` / `ownScope` / `operatorOnly` / `requiresData: 'forks' |
 *  'org'` — concepts that don't apply at the whole-app granularity this
 *  registry works at). */
export function isAppVisible(
  app: AppManifest,
  userRole: UserRole | undefined,
  ctx?: AppVisibilityContext,
): boolean {
  if (app.minRole) {
    if (!userRole) return false;
    if (ROLE_LEVELS[userRole] < ROLE_LEVELS[app.minRole]) return false;
  }
  if (app.applianceOnly && !ctx?.isAppliance) return false;
  return true;
}

/** Filter the registry down to what `userRole` may see. */
export function visibleApps(
  userRole: UserRole | undefined,
  ctx?: AppVisibilityContext,
): AppManifest[] {
  return APPS.filter((app) => isAppVisible(app, userRole, ctx));
}

/**
 * 新功能 chip convention (2026-08-14, documented in full on `nav-model.ts`'s
 * original declaration). Lives here — not duplicated in `nav-model.ts` — so
 * the sidebar's per-page chips and the launcher's per-app chips are
 * driven by literally the same comparison (§3 C8: "manifest 帶 newIn 欄位，
 * nav 投影與 launcher 同源渲染，規範自動兩處生效"). `nav-model.ts` re-exports
 * this symbol unchanged so every existing import of `isNewFeature` from
 * `./nav-model` keeps working.
 */
export function isNewFeature(newIn: string | undefined, currentVersion: string | null): boolean {
  if (!newIn) return false;
  const parse = (v: string): [number, number] | null => {
    const m = v.trim().replace(/^v/, '').match(/^(\d+)\.(\d+)/);
    return m ? [Number(m[1]), Number(m[2])] : null;
  };
  const target = parse(newIn);
  if (!target) return false;
  const current = currentVersion ? parse(currentVersion) : null;
  if (!current) return true;
  return current[0] < target[0] || (current[0] === target[0] && current[1] <= target[1]);
}
