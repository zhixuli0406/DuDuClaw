import { describe, it, expect } from 'vitest';
import {
  navGroups,
  personalPrimaryItems,
  personalAdvancedGroup,
  navGroupsForEdition,
  primaryItemsForEdition,
} from './nav-model';

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
