import { describe, it, expect } from 'vitest';
import {
  navGroups,
  employeeNavItems,
  isEmployeeSidebar,
  mobileNavItems,
  mobileNavItemsForRole,
  type UserRole,
  personalPrimaryItems,
  manageAdvancedNav,
  manageNav,
  personalAdvancedGroup,
  navGroupsForEdition,
  primaryItemsForEdition,
} from './nav-model';
import { filterVisible, isVisible } from '@/lib/nav-visibility';
import { crumbsFor } from './nav-model';

describe('machine console visibility', () => {
  it('shows the machine entry only to administrators in both editions', () => {
    const entry = primaryItemsForEdition(false).find(i => i.to === '/console')!;
    expect(entry).toBeDefined();
    for (const personal of [false, true]) {
      expect(isVisible(entry, 'employee', personal)).toBe(false);
      expect(isVisible(entry, 'manager', personal)).toBe(false);
      expect(isVisible(entry, 'admin', personal)).toBe(true);
    }
  });
});

describe('own conversation-view receipts', () => {
  it('is reachable by an enterprise employee and hidden on Personal', () => {
    const entry = navGroups.flatMap(g => g.items).find(i => i.to === '/my/history-views')!;
    expect(isVisible(entry, 'employee', false)).toBe(true);
    expect(isVisible(entry, 'employee', true)).toBe(false);
    expect(entry.ownScope).toBe(true);
    expect(crumbsFor('/my/history-views')).toContainEqual({ labelId: 'nav.myHistoryViews' });
  });
});

// P2-a-nav (2026-08-15, `DESIGN-dashboard-ux-workbuddy-2026-08.md` §3.4):
// `/experts` ("AI 團隊", formerly "專家包") was `minRole: 'admin'` and buried
// at the tail of the Personal edition's collapsed 進階 group — invisible to
// every non-admin viewer and two clicks deep for everyone else (design doc
// walkthrough 5). This promotes it to the 一般層 on both editions.
describe('nav-model — P2-a-nav /experts promotion', () => {
  it('drops the admin-only gate so every role sees it (一般層, like its 公司-group peers)', () => {
    const company = navGroups[1];
    const experts = company.items.find((i) => i.to === '/experts');
    expect(experts).toBeDefined();
    expect(experts?.minRole).toBeUndefined();
  });

  it('stays inside the Enterprise 公司 group (navGroups[1])', () => {
    expect(navGroups[0].items.some((i) => i.to === '/experts')).toBe(false);
    expect(navGroups[1].items.some((i) => i.to === '/experts')).toBe(true);
    expect(navGroups[2].items.some((i) => i.to === '/experts')).toBe(false);
  });

  it('is added to the Personal primary rail and removed from the Personal 進階 group — exactly once total, not duplicated', () => {
    const inPrimary = personalPrimaryItems.filter((i) => i.to === '/experts').length;
    const inAdvanced = personalAdvancedGroup.items.filter((i) => i.to === '/experts').length;
    expect(inPrimary).toBe(1);
    expect(inAdvanced).toBe(0);
  });

  it('a Personal-edition viewer sees /experts exactly once across the whole nav surface', () => {
    const primary = primaryItemsForEdition(true);
    const groups = navGroupsForEdition(true);
    const occurrences =
      primary.filter((i) => i.to === '/experts').length +
      groups.flatMap((g) => g.items).filter((i) => i.to === '/experts').length;
    expect(occurrences).toBe(1);
  });

  it('an Enterprise-edition viewer sees /experts exactly once across the whole nav surface', () => {
    const primary = primaryItemsForEdition(false);
    const groups = navGroupsForEdition(false);
    const occurrences =
      primary.filter((i) => i.to === '/experts').length +
      groups.flatMap((g) => g.items).filter((i) => i.to === '/experts').length;
    expect(occurrences).toBe(1);
  });

  it('is not tagged newIn — this is an existing page promoted/renamed, not a new feature (per team convention)', () => {
    const fromPersonal = personalPrimaryItems.find((i) => i.to === '/experts');
    const fromEnterprise = navGroups[1].items.find((i) => i.to === '/experts');
    expect(fromPersonal?.newIn).toBeUndefined();
    expect(fromEnterprise?.newIn).toBeUndefined();
    // Both editions share the SAME NavItem object (nav-model looks items up
    // by route via `itemByPath`), so this also guards against a future edit
    // tagging just one edition's reference.
    expect(fromPersonal).toBe(fromEnterprise);
  });
});

// P2-b (2026-08-15, `DESIGN-dashboard-ux-workbuddy-2026-08.md` §3.5): the new
// 靈感畫廊 page must land in BOTH the Enterprise 公司 group and the Personal
// primary rail — Personal is a separately-maintained `pickItems([...])` array
// and a missed addition there means Personal never sees the new page at all.
describe('nav-model — P2-b /gallery addition', () => {
  it('is added to the Enterprise 公司 group (navGroups[1]) only', () => {
    expect(navGroups[0].items.some((i) => i.to === '/gallery')).toBe(false);
    expect(navGroups[1].items.some((i) => i.to === '/gallery')).toBe(true);
    expect(navGroups[2].items.some((i) => i.to === '/gallery')).toBe(false);
  });

  it('is added to the Personal primary rail (independent pickItems array)', () => {
    expect(personalPrimaryItems.some((i) => i.to === '/gallery')).toBe(true);
    expect(personalAdvancedGroup.items.some((i) => i.to === '/gallery')).toBe(false);
  });

  it('a viewer on either edition sees /gallery exactly once', () => {
    for (const isPersonal of [true, false]) {
      const primary = primaryItemsForEdition(isPersonal);
      const groups = navGroupsForEdition(isPersonal);
      const occurrences =
        primary.filter((i) => i.to === '/gallery').length +
        groups.flatMap((g) => g.items).filter((i) => i.to === '/gallery').length;
      expect(occurrences).toBe(1);
    }
  });

  it('is tagged newIn 1.60.0 — a genuinely new page (per the newIn convention)', () => {
    const fromPersonal = personalPrimaryItems.find((i) => i.to === '/gallery');
    const fromEnterprise = navGroups[1].items.find((i) => i.to === '/gallery');
    expect(fromPersonal?.newIn).toBe('1.60.0');
    expect(fromEnterprise?.newIn).toBe('1.60.0');
    // Both editions share the SAME NavItem object.
    expect(fromPersonal).toBe(fromEnterprise);
  });
});

// P2-d Agent Mail (2026-08-15): `/mail` is a genuinely new page, so it must
// carry `newIn` and must be registered in BOTH nav lists — the Personal
// edition's arrays are maintained independently, and a missed addition there
// means Personal users never see the mailbox at all.
describe('nav-model — P2-d /mail addition', () => {
  it('is added to the Enterprise 工作 group (navGroups[0]) only', () => {
    expect(navGroups[0].items.some((i) => i.to === '/mail')).toBe(true);
    expect(navGroups[1].items.some((i) => i.to === '/mail')).toBe(false);
    expect(navGroups[2].items.some((i) => i.to === '/mail')).toBe(false);
  });

  it('is added to the Personal 進階 group (independent pickItems array)', () => {
    // 進階 rather than the primary rail: it is manager-gated (same as
    // /timeline + /reports), and the 2026-08-04 client-annotated primary
    // order is fixed.
    expect(personalAdvancedGroup.items.some((i) => i.to === '/mail')).toBe(true);
    expect(personalPrimaryItems.some((i) => i.to === '/mail')).toBe(false);
  });

  it('a viewer on either edition sees /mail exactly once', () => {
    for (const isPersonal of [true, false]) {
      const primary = primaryItemsForEdition(isPersonal);
      const groups = navGroupsForEdition(isPersonal);
      const occurrences =
        primary.filter((i) => i.to === '/mail').length +
        groups.flatMap((g) => g.items).filter((i) => i.to === '/mail').length;
      expect(occurrences).toBe(1);
    }
  });

  it('is tagged newIn 1.60.0 and manager-gated to match the mail.* RPCs', () => {
    const item = navGroups[0].items.find((i) => i.to === '/mail');
    expect(item?.newIn).toBe('1.60.0');
    expect(item?.minRole).toBe('manager');
    // Both editions share the SAME NavItem object (pickItems looks it up).
    expect(personalAdvancedGroup.items.find((i) => i.to === '/mail')).toBe(item);
  });

  it('has i18n label/desc ids following the `${label}.desc` convention', () => {
    const item = navGroups[0].items.find((i) => i.to === '/mail');
    expect(item?.label).toBe('nav.mail');
    expect(item?.desc).toBe('nav.mail.desc');
  });
});

// 1.76.0 文件知識庫 (document knowledge base): a new page, so it carries
// `newIn` and sits in BOTH editions' lists, right after 記憶.
describe('nav-model — 1.76.0 /knowledge-base addition', () => {
  it('follows /memory in the Enterprise 公司 group and the Personal primary rail', () => {
    const company = navGroups[1].items.map((i) => i.to);
    expect(company.indexOf('/knowledge-base')).toBe(company.indexOf('/memory') + 1);
    const personal = personalPrimaryItems.map((i) => i.to);
    expect(personal.indexOf('/knowledge-base')).toBe(personal.indexOf('/memory') + 1);
  });

  it('appears exactly once per edition, tagged newIn 1.76.0, open to every role', () => {
    for (const isPersonal of [true, false]) {
      const all = [...primaryItemsForEdition(isPersonal), ...navGroupsForEdition(isPersonal).flatMap((g) => g.items)];
      const rows = all.filter((i) => i.to === '/knowledge-base');
      expect(rows).toHaveLength(1);
      expect(rows[0].newIn).toBe('1.76.0');
      expect(rows[0].minRole).toBeUndefined();
      expect(rows[0].desc).toBe(`${rows[0].label}.desc`);
    }
  });
});

// 1.76.0 我的團隊 (human org, PR-E): a new manager+ page in a new 組織 group.
// Enterprise-only — the Personal edition's nav lists are maintained
// separately and must NOT gain it (users.my_team is refused there).
describe('nav-model — 1.76.0 /team (我的團隊) addition', () => {
  const findTeam = () => navGroups.flatMap((g) => g.items).find((i) => i.to === '/team');

  it('lives in the 組織 group, appended after the three fixed groups', () => {
    expect(navGroups[3].label).toBe('navGroup.org');
    expect(navGroups[3].items.map((i) => i.to)).toEqual(['/me/connections', '/team', '/team/conversations']);
    for (const g of navGroups.slice(0, 3)) {
      expect(g.items.some((i) => i.to === '/team')).toBe(false);
    }
  });

  it('is manager+, Enterprise-only and tagged newIn 1.76.0', () => {
    const item = findTeam();
    expect(item?.minRole).toBe('manager');
    expect(item?.enterprise).toBe(true);
    expect(item?.newIn).toBe('1.76.0');
    expect(item?.desc).toBe(`${item?.label}.desc`);
  });

  it('never appears in the Personal edition nav', () => {
    const all = [...primaryItemsForEdition(true), ...navGroupsForEdition(true).flatMap((g) => g.items)];
    expect(all.some((i) => i.to === '/team')).toBe(false);
    expect(personalPrimaryItems.some((i) => i.to === '/team')).toBe(false);
    expect(personalAdvancedGroup.items.some((i) => i.to === '/team')).toBe(false);
  });

  it('is visible to a manager and an admin on Enterprise, hidden from an employee and on Personal', () => {
    const item = findTeam()!;
    expect(isVisible(item, 'manager', false)).toBe(true);
    expect(isVisible(item, 'admin', false)).toBe(true);
    expect(isVisible(item, 'employee', false)).toBe(false);
    expect(isVisible(item, 'admin', true)).toBe(false);
  });
});

describe('nav-model — page titles for the people pages', () => {
  it('uses the specific leaf for team conversations and matches route segments', () => {
    expect(crumbsFor('/team/conversations').at(-1)?.labelId).toBe('nav.teamHistory');
    expect(crumbsFor('/team/conversations/detail').at(-1)?.labelId).toBe('nav.teamHistory');
    expect(crumbsFor('/teamwork')).toEqual([]);
  });

  it('gives /team and the members page (canonical and legacy path) their own label', () => {
    expect(crumbsFor('/team').at(-1)?.labelId).toBe('nav.myTeam');
    expect(crumbsFor('/manage/users').at(-1)?.labelId).toBe('manage.users');
    expect(crumbsFor('/users').at(-1)?.labelId).toBe('manage.users');
  });
});


describe('nav RPC role alignment', () => {
  it('cron and unified audit pages require admin in both editions', () => {
    const routines = navGroups.flatMap(g => g.items).find(i => i.to === '/routines')!;
    expect(routines.minRole).toBe('admin');
    expect(manageAdvancedNav.find(i => i.to === '/manage/logs')?.minRole).toBe('admin');
    expect(navGroups.flatMap(g => g.items).find(i => i.to === '/gallery')?.minRole).toBe('admin');
    expect(isVisible(routines, 'employee', false)).toBe(false);
    expect(isVisible(routines, 'manager', false)).toBe(false);
    expect(isVisible(routines, 'admin', false)).toBe(true);
  });
});

describe('self-service account navigation', () => {
  it('appears once in both editions with a version badge and no role gate', () => {
    for (const personal of [false, true]) {
      const items = [...primaryItemsForEdition(personal), ...navGroupsForEdition(personal).flatMap(g => g.items)];
      const entries = items.filter(i => i.to === '/me/account');
      expect(entries).toHaveLength(1);
      expect(entries[0].newIn).toBe('1.78.0');
      expect(entries[0].minRole).toBeUndefined();
      expect(entries[0].enterprise).toBeUndefined();
      for (const role of ['admin', 'manager', 'employee'] as const) {
        expect(isVisible(entries[0], role, personal)).toBe(true);
      }
    }
    expect(crumbsFor('/me/account')).toEqual([{ labelId: 'nav.account' }]);
  });
});


describe('enterprise employee navigation', () => {
  it('shows exactly the daily employee destinations in the agreed order', () => {
    expect(employeeNavItems().map((item) => item.to)).toEqual([
      '/', '/inbox', '/routines', '/skills', '/memory', '/world', '/org',
    ]);
    expect(employeeNavItems(false)).toEqual(employeeNavItems());
    expect(employeeNavItems().filter(item => item.newIn).map(item => [item.to, item.newIn])).toEqual([['/routines', '1.80.0'], ['/org', '1.80.0']]);
    expect(employeeNavItems().find((item) => item.to === '/inbox')?.badge).toBe('inbox');
  });

  it('keeps task rows hidden when enabled, retaining responsibility and foresight gates', () => {
    expect(employeeNavItems(true).map((item) => item.to)).toEqual([
      '/', '/inbox', '/routines', '/skills', '/memory', '/world', '/org', '/responsibilities',
    ]);
    expect(employeeNavItems(true).filter(item => ['/routines', '/org'].includes(item.to))).toHaveLength(2);
    expect(employeeNavItems(true).filter(item => item.newIn).every(item => item.newIn === '1.80.0')).toBe(true);
  });

  it('applies only to employees outside the Personal edition', () => {
    for (const role of ['admin', 'manager', 'employee', undefined] as const) {
      expect(isEmployeeSidebar(role, true)).toBe(false);
      expect(isEmployeeSidebar(role, false)).toBe(role === 'employee');
    }
  });

  it('keeps mobile employee entries within the simplified sidebar or conversation history', () => {
    for (const enabled of [false, true]) {
      const allowed = [...employeeNavItems(enabled).map((item) => item.to), '/conversations'];
      const mobile = mobileNavItemsForRole('employee', false, enabled).map((item) => item.to);
      expect(mobile.every((path) => allowed.includes(path))).toBe(true);
      expect(mobile.includes('/chat')).toBe(false);
      expect(mobile.includes('/tasks')).toBe(false);
    }
  });

  it('preserves the previous mobile array for admins, managers and Personal', () => {
    expect(mobileNavItems.map((item) => item.to)).toEqual(['/', '/chat', '/conversations', '/skills']);
    for (const role of ['admin', 'manager', 'employee'] as const) {
      for (const personal of [false, true]) {
        if (isEmployeeSidebar(role, personal)) continue;
        for (const enabled of [false, true]) {
          expect(mobileNavItemsForRole(role, personal, enabled)).toBe(mobileNavItems);
        }
      }
    }
  });
});

// Expected visible routes, including intended 1.80.0 additions and task-entry hiding.
// All progressive-disclosure facts are enabled to make every prior entry observable.
const previousNavigation = {
  "admin:enterprise": {
    "primary": [
      "/chat",
      "/",
      "/console"
    ],
    "groups": [
      [
        "/routines",
        "/plans",
        "/responsibilities",
        "/runs",
        "/canvas",
        "/files",
        "/meeting-transcripts",
        "/mail",
        "/timeline",
        "/reports",
        "/employee-health",
        "/foresight",
        "/forks",
        "/os",
        "/presets",
        "/app/system/device",
      ],
      [
        "/skills",
        "/memory",
        "/knowledge-base",
        "/agents",
        "/templates",
        "/world",
        "/growth",
        "/org",
        "/experts",
        "/gallery",
        "/widgets",
        "/pet-studio"
      ],
      [
        "/me/account",
        "/my/history-views",
        "/manage",
        "/app/system/settings/company",
        "/app/system/settings/advanced",
        "/about"
      ],
      [
        "/me/connections",
        "/team",
        "/team/conversations"
      ]
    ]
  },
  "admin:personal": {
    "primary": [
      "/chat",
      "/",
      "/console",
      "/routines",
      "/responsibilities",
      "/foresight",
      "/skills",
      "/memory",
      "/knowledge-base",
      "/agents",
      "/world",
      "/templates",
      "/experts",
      "/gallery",
      "/pet-studio"
    ],
    "groups": [
      [
        "/me/account",
        "/manage",
        "/app/system/settings/company",
        "/app/system/settings/advanced",
        "/about"
      ],
      [
        "/plans",
        "/runs",
        "/canvas",
        "/files",
        "/meeting-transcripts",
        "/mail",
        "/timeline",
        "/reports",
        "/employee-health",
        "/forks",
        "/os",
        "/presets",
        "/app/system/device",
        "/growth",
        "/org",
        "/widgets"
      ]
    ]
  },
  "manager:enterprise": {
    "primary": [
      "/chat",
      "/"
    ],
    "groups": [
      [
        "/plans",
        "/responsibilities",
        "/runs",
        "/canvas",
        "/files",
        "/meeting-transcripts",
        "/mail",
        "/timeline",
        "/reports",
        "/employee-health",
        "/foresight",
        "/forks",
      ],
      [
        "/skills",
        "/memory",
        "/knowledge-base",
        "/agents",
        "/world",
        "/growth",
        "/org",
        "/experts",
        "/widgets",
        "/pet-studio"
      ],
      [
        "/me/account",
        "/my/history-views",
        "/manage",
        "/about"
      ],
      [
        "/me/connections",
        "/team",
        "/team/conversations"
      ]
    ]
  },
  "manager:personal": {
    "primary": [
      "/chat",
      "/",
      "/responsibilities",
      "/foresight",
      "/skills",
      "/memory",
      "/knowledge-base",
      "/agents",
      "/world",
      "/experts",
      "/pet-studio"
    ],
    "groups": [
      [
        "/me/account",
        "/manage",
        "/about"
      ],
      [
        "/plans",
        "/runs",
        "/canvas",
        "/files",
        "/meeting-transcripts",
        "/mail",
        "/timeline",
        "/reports",
        "/employee-health",
        "/forks",
        "/growth",
        "/org",
        "/widgets"
      ]
    ]
  },
  "employee:personal": {
    "primary": [
      "/chat",
      "/",
      "/responsibilities",
      "/skills",
      "/memory",
      "/knowledge-base",
      "/agents",
      "/world",
      "/experts",
      "/pet-studio"
    ],
    "groups": [
      [
        "/me/account",
        "/about"
      ],
      [
        "/plans",
        "/runs",
        "/canvas",
        "/files",
        "/meeting-transcripts",
        "/growth",
        "/widgets"
      ]
    ]
  }
} as const;

describe('previous admin, manager and Personal navigation snapshots', () => {
  it.each(Object.keys(previousNavigation))('preserves %s', (key) => {
    const [role, edition] = key.split(':') as [UserRole, string];
    const personal = edition === 'personal';
    const ctx = { hasOperatorAccess: true, forksExist: true, agentCount: 100, isDesktop: true, isAppliance: true };
    const routes = (items: typeof personalPrimaryItems) =>
      filterVisible(items, role, personal, ctx).map((item) => item.to);
    expect({
      primary: routes(primaryItemsForEdition(personal)),
      groups: navGroupsForEdition(personal).map((group) => routes(group.items)),
    }).toEqual(previousNavigation[key as keyof typeof previousNavigation]);
  });
});


describe('admin setup checklist navigation', () => {
  it('is registered in management with a version badge and edition/role gates', () => {
    const entry = manageNav.find(i => i.to === '/setup')!;
    expect(entry).toBeDefined();
    expect(entry.newIn).toBe('1.79.0');
    expect(isVisible(entry, 'admin', false)).toBe(true);
    for (const role of ['employee', 'manager'] as const) {
      expect(isVisible(entry, role, false)).toBe(false);
      expect(employeeNavItems(true).some(i => i.to === '/setup')).toBe(false);
    }
    expect(isVisible(entry, 'admin', true)).toBe(false);
    expect(crumbsFor('/setup')).toEqual([{ labelId: 'nav.manage', to: '/manage' }, { labelId: 'setup.title' }]);
  });
});


describe('task navigation hidden for all roles and editions', () => {
  it.each(['admin', 'manager', 'employee'] as const)('hides task entries for %s without removing responsibilities or foresight', role => {
    const taskEntries = navGroups.flatMap(group => group.items).filter(item => ['/tasks', '/goals'].includes(item.to));
    expect(taskEntries).toHaveLength(2);
    for (const personal of [false, true]) {
      for (const entry of taskEntries) expect(isVisible(entry, role, personal)).toBe(false);
      expect(mobileNavItemsForRole(role, personal, true).some(item => ['/tasks', '/goals', '/assign'].includes(item.to))).toBe(false);
    }
    expect(navGroups.flatMap(group => group.items).some(item => item.to === '/responsibilities')).toBe(true);
    expect(navGroups.flatMap(group => group.items).some(item => item.to === '/foresight')).toBe(true);
  });
});

describe('advanced and company settings navigation', () => {
  it('shows one Admin-only versioned entry per page in both editions', () => {
    for (const personal of [false, true]) {
      const items = navGroupsForEdition(personal).flatMap((group) => group.items);
      for (const page of ['advanced', 'company']) {
        const entries = items.filter((item) => item.to === `/app/system/settings/${page}`);
        expect(entries).toHaveLength(1);
        expect(entries[0].newIn).toBe('1.80.0');
        expect(isVisible(entries[0], 'admin', personal)).toBe(true);
        expect(isVisible(entries[0], 'manager', personal)).toBe(false);
        expect(isVisible(entries[0], 'employee', personal)).toBe(false);
      }
    }
  });

  it('uses the new page label rather than the parent settings breadcrumb', () => {
    expect(crumbsFor('/app/system/settings/advanced').at(-1)?.labelId).toBe('advancedSettings.title');
    expect(crumbsFor('/app/system/settings/company').at(-1)?.labelId).toBe('companyInfo.title');
  });
});

describe('employee health visibility', () => {
  it('keeps the new page in both editions with manager access and the release badge', () => {
    const team = navGroups.flatMap(group => group.items).find(item => item.to === '/employee-health')!;
    const personal = personalAdvancedGroup.items.find(item => item.to === '/employee-health')!;
    for (const entry of [team, personal]) {
      expect(entry).toBeDefined();
      expect(entry.newIn).toBe('1.80.0');
      for (const isPersonal of [false, true]) {
        expect(isVisible(entry, 'manager', isPersonal)).toBe(true);
        expect(isVisible(entry, 'admin', isPersonal)).toBe(true);
        expect(isVisible(entry, 'employee', isPersonal)).toBe(false);
      }
    }
  });
});

describe('member management visibility', () => {
  it('keeps member management restricted to Enterprise admins', () => {
    const entry = manageAdvancedNav.find(item => item.to === '/manage/users')!;
    expect(entry).toBeDefined();
    for (const personal of [false, true]) {
      for (const role of ['admin', 'manager', 'employee'] as const) {
        expect(isVisible(entry, role, personal)).toBe(role === 'admin' && !personal);
      }
    }
  });
});


describe('employee template navigation', () => {
  it('keeps templates Admin-only in both editions and out of the employee sidebar', () => {
    for (const personal of [false, true]) {
      const entries = [...primaryItemsForEdition(personal), ...navGroupsForEdition(personal).flatMap(group => group.items)]
        .filter(item => item.to === '/templates');
      expect(entries).toHaveLength(1);
      for (const role of ['admin', 'manager', 'employee'] as const) {
        expect(isVisible(entries[0], role, personal)).toBe(role === 'admin');
      }
    }
    for (const enabled of [false, true]) {
      expect(employeeNavItems(enabled).some(item => item.to === '/templates')).toBe(false);
    }
  });
});
