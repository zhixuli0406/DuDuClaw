import { describe, it, expect } from 'vitest';
import { filterVisible } from '@/lib/nav-visibility';
import { LAUNCHER_CARDS } from './launcher-model';

describe('launcher visibility', () => {
  it('admin on enterprise sees every card', () => {
    const visible = filterVisible(LAUNCHER_CARDS, 'admin', false);
    expect(visible.length).toBe(LAUNCHER_CARDS.length);
  });

  it('employee never sees admin-gated cards (odoo/inference/channels/mcp)', () => {
    const visible = filterVisible(LAUNCHER_CARDS, 'employee', false).map((c) => c.id);
    expect(visible).not.toContain('odoo');
    expect(visible).not.toContain('inference');
    expect(visible).not.toContain('channels');
    expect(visible).not.toContain('mcp');
    // …but still sees open cards
    expect(visible).toContain('claw');
    expect(visible).toContain('tasks');
  });

  it('no launcher card is enterprise-gated, so personal admin keeps all', () => {
    // The launcher intentionally has no `enterprise` cards; personal edition
    // should therefore not drop anything for an admin.
    const enterprise = filterVisible(LAUNCHER_CARDS, 'admin', false);
    const personal = filterVisible(LAUNCHER_CARDS, 'admin', true);
    expect(personal.length).toBe(enterprise.length);
  });

  it('coming-soon cards have no destination route', () => {
    for (const card of LAUNCHER_CARDS) {
      if (card.status === 'coming-soon') expect(card.to).toBe('');
      else expect(card.to.length).toBeGreaterThan(0);
    }
  });
});
