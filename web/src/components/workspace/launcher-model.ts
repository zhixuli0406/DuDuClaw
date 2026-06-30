import {
  MessageCircle,
  Bot,
  Presentation,
  FileText,
  Table,
  Puzzle,
  Building2,
  Cpu,
  LayoutDashboard,
  KanbanSquare,
  Brain,
  BookOpen,
  Store,
  Radio,
  Plug,
  Globe,
} from 'lucide-react';
import type { ComponentType } from 'react';
import type { Gated } from '@/lib/nav-visibility';

/** Controlled icon-accent palette (DESIGN.md §2 launcher exception). The only
 *  place colour beyond amber is allowed — and only on launcher icons. */
export type LauncherAccent =
  | 'amber'
  | 'blue'
  | 'emerald'
  | 'violet'
  | 'rose'
  | 'sky'
  | 'orange'
  | 'cyan';

/** Logical groups, rendered top-to-bottom in this order. */
export type LauncherGroupKey =
  | 'employee'
  | 'office'
  | 'build'
  | 'tools'
  | 'integrations';

export const LAUNCHER_GROUP_ORDER: LauncherGroupKey[] = [
  'employee',
  'office',
  'build',
  'tools',
  'integrations',
];

export interface LauncherCardModel extends Gated {
  readonly id: string;
  readonly group: LauncherGroupKey;
  readonly icon: ComponentType<{ className?: string }>;
  readonly accent: LauncherAccent;
  /** Destination route. Empty for `coming-soon` cards. */
  readonly to: string;
  /** `ready` cards navigate; `coming-soon` cards are inert (greyed + badge). */
  readonly status: 'ready' | 'coming-soon';
}

/**
 * Workspace launcher cards (TODO-genspark-workspace-shell §1 / §P3.1).
 * Maps Genspark's tool grid onto DuDuClaw's existing surfaces. Office-suite
 * (slides/docs/sheets) ship as `coming-soon` per §0 — shell only, no result
 * factory this phase. i18n keys: `launcher.<id>.label` / `launcher.<id>.desc`.
 */
export const LAUNCHER_CARDS: LauncherCardModel[] = [
  // — AI 員工 —
  { id: 'claw', group: 'employee', icon: MessageCircle, accent: 'orange', to: '/webchat', status: 'ready' },
  { id: 'agents', group: 'employee', icon: Bot, accent: 'amber', to: '/agents', status: 'ready' },

  // — 辦公套件 (coming soon) —
  { id: 'slides', group: 'office', icon: Presentation, accent: 'rose', to: '', status: 'coming-soon' },
  { id: 'docs', group: 'office', icon: FileText, accent: 'sky', to: '', status: 'coming-soon' },
  { id: 'sheets', group: 'office', icon: Table, accent: 'emerald', to: '', status: 'coming-soon' },

  // — 建構 —
  { id: 'skills', group: 'build', icon: Puzzle, accent: 'violet', to: '/skills', status: 'ready' },
  { id: 'odoo', group: 'build', icon: Building2, accent: 'blue', to: '/odoo', status: 'ready', minRole: 'admin' },
  { id: 'inference', group: 'build', icon: Cpu, accent: 'cyan', to: '/inference', status: 'ready', minRole: 'admin' },
  { id: 'dashboard', group: 'build', icon: LayoutDashboard, accent: 'amber', to: '/', status: 'ready' },

  // — 工具 —
  { id: 'tasks', group: 'tools', icon: KanbanSquare, accent: 'blue', to: '/tasks', status: 'ready' },
  { id: 'memory', group: 'tools', icon: Brain, accent: 'violet', to: '/memory', status: 'ready' },
  { id: 'wiki', group: 'tools', icon: BookOpen, accent: 'emerald', to: '/wiki', status: 'ready' },
  { id: 'marketplace', group: 'tools', icon: Store, accent: 'orange', to: '/marketplace', status: 'ready' },

  // — 整合 —
  { id: 'channels', group: 'integrations', icon: Radio, accent: 'sky', to: '/channels', status: 'ready', minRole: 'admin' },
  { id: 'mcp', group: 'integrations', icon: Plug, accent: 'cyan', to: '/mcp', status: 'ready', minRole: 'admin' },
  { id: 'sharedWiki', group: 'integrations', icon: Globe, accent: 'blue', to: '/shared-wiki', status: 'ready' },
];

/** Tailwind classes for each accent (icon foreground + soft tile background). */
export const ACCENT_CLASS: Record<LauncherAccent, string> = {
  amber: 'text-amber-500 bg-amber-500/10',
  blue: 'text-blue-500 bg-blue-500/10',
  emerald: 'text-emerald-500 bg-emerald-500/10',
  violet: 'text-violet-500 bg-violet-500/10',
  rose: 'text-rose-500 bg-rose-500/10',
  sky: 'text-sky-500 bg-sky-500/10',
  orange: 'text-orange-500 bg-orange-500/10',
  cyan: 'text-cyan-500 bg-cyan-500/10',
};
