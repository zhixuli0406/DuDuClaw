import { describe, expect, it } from 'vitest';
import { visibleTourSteps } from './tour-steps';

describe('guided tour task hiding', () => {
  it.each(['admin', 'manager', 'employee'] as const)('does not lead %s into hidden task entries', role => {
    const routes = visibleTourSteps(role).map(step => step.route);
    expect(routes).toContain('/chat');
    for (const route of ['/tasks', '/goals', '/assign']) expect(routes).not.toContain(route);
  });
});
