import { describe, it, expect } from 'vitest';
import {
  navGroups,
  personalPrimaryItems,
  personalAdvancedGroup,
  navGroupsForEdition,
  primaryItemsForEdition,
} from './nav-model';
import { isVisible } from '@/lib/nav-visibility';
import { crumbsFor } from './nav-model';

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
