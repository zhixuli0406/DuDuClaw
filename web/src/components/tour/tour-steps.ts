import type { UserRole } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';

/**
 * One stop on the guided tour. `target` is a CSS selector for the element to
 * spotlight (sidebar nav links carry `data-tour="nav:<route>"`); when it can't
 * be found the tour degrades to a centered card. `minRole` drops stops the
 * user can't reach, so a viewer never gets walked to an admin-only page.
 */
export interface TourStep {
  readonly route: string;
  readonly target?: string;
  readonly titleKey: string;
  readonly bodyKey: string;
  readonly minRole?: UserRole;
}

const STEPS: ReadonlyArray<TourStep> = [
  { route: '/', target: '[data-tour="nav:/"]', titleKey: 'tour.step.dashboard.title', bodyKey: 'tour.step.dashboard.body' },
  { route: '/webchat', target: '[data-tour="nav:/webchat"]', titleKey: 'tour.step.webchat.title', bodyKey: 'tour.step.webchat.body' },
  { route: '/agents', target: '[data-tour="nav:/agents"]', titleKey: 'tour.step.agents.title', bodyKey: 'tour.step.agents.body' },
  { route: '/channels', target: '[data-tour="nav:/channels"]', titleKey: 'tour.step.channels.title', bodyKey: 'tour.step.channels.body', minRole: 'admin' },
  { route: '/accounts', target: '[data-tour="nav:/accounts"]', titleKey: 'tour.step.accounts.title', bodyKey: 'tour.step.accounts.body', minRole: 'admin' },
  { route: '/inference', target: '[data-tour="nav:/inference"]', titleKey: 'tour.step.inference.title', bodyKey: 'tour.step.inference.body', minRole: 'admin' },
  { route: '/memory', target: '[data-tour="nav:/memory"]', titleKey: 'tour.step.memory.title', bodyKey: 'tour.step.memory.body' },
  { route: '/wiki', target: '[data-tour="nav:/wiki"]', titleKey: 'tour.step.knowledge.title', bodyKey: 'tour.step.knowledge.body' },
  { route: '/settings', target: '[data-tour="nav:/settings"]', titleKey: 'tour.step.settings.title', bodyKey: 'tour.step.settings.body', minRole: 'admin' },
] as const;

/** The tour stops the given role can actually reach, in order. */
export function visibleTourSteps(role: UserRole | undefined): TourStep[] {
  return STEPS.filter((s) => hasMinRole(role, s.minRole));
}
