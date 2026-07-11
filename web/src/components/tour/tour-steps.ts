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

// Targets are the `data-tour="nav:<route>"` markers the Sidebar stamps on each
// nav row (see Sidebar `NavRow` + the 員工 zone's "全部員工 →" link). The routes
// track the v2「嘟嘟事務所」navigation, not the retired v1 IA — so the walk
// visits the daily items, the staff roster, and the company section in order.
const STEPS: ReadonlyArray<TourStep> = [
  { route: '/', target: '[data-tour="nav:/"]', titleKey: 'tour.step.dashboard.title', bodyKey: 'tour.step.dashboard.body' },
  { route: '/chat', target: '[data-tour="nav:/chat"]', titleKey: 'tour.step.chat.title', bodyKey: 'tour.step.chat.body' },
  { route: '/inbox', target: '[data-tour="nav:/inbox"]', titleKey: 'tour.step.inbox.title', bodyKey: 'tour.step.inbox.body' },
  { route: '/tasks', target: '[data-tour="nav:/tasks"]', titleKey: 'tour.step.tasks.title', bodyKey: 'tour.step.tasks.body' },
  { route: '/agents', target: '[data-tour="nav:/agents"]', titleKey: 'tour.step.agents.title', bodyKey: 'tour.step.agents.body' },
  { route: '/memory', target: '[data-tour="nav:/memory"]', titleKey: 'tour.step.memory.title', bodyKey: 'tour.step.memory.body' },
  { route: '/skills', target: '[data-tour="nav:/skills"]', titleKey: 'tour.step.skills.title', bodyKey: 'tour.step.skills.body' },
  { route: '/knowledge', target: '[data-tour="nav:/knowledge"]', titleKey: 'tour.step.knowledge.title', bodyKey: 'tour.step.knowledge.body' },
  { route: '/growth', target: '[data-tour="nav:/growth"]', titleKey: 'tour.step.growth.title', bodyKey: 'tour.step.growth.body' },
  { route: '/manage', target: '[data-tour="nav:/manage"]', titleKey: 'tour.step.manage.title', bodyKey: 'tour.step.manage.body', minRole: 'manager' },
] as const;

/** The tour stops the given role can actually reach, in order. */
export function visibleTourSteps(role: UserRole | undefined): TourStep[] {
  return STEPS.filter((s) => hasMinRole(role, s.minRole));
}
