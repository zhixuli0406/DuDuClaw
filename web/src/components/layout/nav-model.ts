import {
  LayoutDashboard,
  Bot,
  KanbanSquare,
  GitFork,
  Network,
  Puzzle,
  Radio,
  Wallet,
  Brain,
  BookOpen,
  Shield,
  Settings,
  FileText,
  MessageCircle,
  BarChart3,
  CreditCard,
  KeyRound,
  Building2,
  Users,
  Plug,
  Globe,
  Store,
  Handshake,
  Cpu,
  Scale,
  Activity,
} from 'lucide-react';
import type { UserRole } from '@/stores/auth-store';

export type NavItem = {
  to: string;
  icon: typeof LayoutDashboard;
  /** i18n message id for the item label. */
  label: string;
  /** Minimum role required to see this item. Omit for all roles. */
  minRole?: UserRole;
};

export type NavGroup = {
  /** i18n message id for the group header. */
  label: string;
  items: NavItem[];
};

/**
 * Single source of truth for sidebar navigation (DESIGN.md §5). Six groups,
 * ordered by usage frequency within each group. Role-gating preserved per item;
 * a group hides entirely when the user can see none of its items.
 */
export const navGroups: NavGroup[] = [
  {
    label: 'navGroup.overview',
    items: [
      { to: '/', icon: LayoutDashboard, label: 'nav.dashboard' },
      { to: '/webchat', icon: MessageCircle, label: 'nav.webchat' },
    ],
  },
  {
    label: 'navGroup.agents',
    items: [
      { to: '/agents', icon: Bot, label: 'nav.agents' },
      { to: '/tasks', icon: KanbanSquare, label: 'nav.tasks' },
      { to: '/forks', icon: GitFork, label: 'nav.forks' },
      { to: '/org', icon: Network, label: 'nav.org', minRole: 'manager' },
      { to: '/memory', icon: Brain, label: 'nav.memory' },
    ],
  },
  {
    label: 'navGroup.knowledge',
    items: [
      { to: '/wiki', icon: BookOpen, label: 'nav.wiki' },
      { to: '/shared-wiki', icon: Globe, label: 'nav.sharedWiki' },
      { to: '/skills', icon: Puzzle, label: 'nav.skills' },
      { to: '/marketplace', icon: Store, label: 'nav.marketplace' },
    ],
  },
  {
    label: 'navGroup.integrations',
    items: [
      { to: '/channels', icon: Radio, label: 'nav.channels', minRole: 'admin' },
      { to: '/mcp', icon: Plug, label: 'nav.mcp', minRole: 'admin' },
      { to: '/mcp-keys', icon: KeyRound, label: 'nav.mcpKeys', minRole: 'admin' },
      { to: '/odoo', icon: Building2, label: 'nav.odoo', minRole: 'admin' },
      { to: '/inference', icon: Cpu, label: 'nav.inference', minRole: 'admin' },
    ],
  },
  {
    label: 'navGroup.operations',
    items: [
      { to: '/accounts', icon: Wallet, label: 'nav.accounts', minRole: 'admin' },
      { to: '/billing', icon: CreditCard, label: 'nav.billing', minRole: 'manager' },
      { to: '/reports', icon: BarChart3, label: 'nav.reports', minRole: 'manager' },
      { to: '/license', icon: KeyRound, label: 'nav.license', minRole: 'manager' },
      { to: '/partner', icon: Handshake, label: 'nav.partner', minRole: 'manager' },
    ],
  },
  {
    label: 'navGroup.system',
    items: [
      { to: '/security', icon: Shield, label: 'nav.security', minRole: 'admin' },
      { to: '/governance', icon: Scale, label: 'nav.governance', minRole: 'admin' },
      { to: '/wiki-trust', icon: Shield, label: 'nav.wikiTrust', minRole: 'admin' },
      { to: '/reliability', icon: Activity, label: 'nav.reliability', minRole: 'admin' },
      { to: '/users', icon: Users, label: 'nav.users', minRole: 'admin' },
      { to: '/logs', icon: FileText, label: 'nav.logs', minRole: 'manager' },
      { to: '/settings', icon: Settings, label: 'nav.settings', minRole: 'admin' },
    ],
  },
];
