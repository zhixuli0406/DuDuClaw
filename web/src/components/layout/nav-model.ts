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
  /**
   * i18n message id for a one-line description shown under the label in the
   * sidebar (and as a subtitle in the command palette). By convention this is
   * `${label}.desc`. Keeps the nav self-explanatory — no guessing from icons.
   */
  desc: string;
  /** Minimum role required to see this item. Omit for all roles. */
  minRole?: UserRole;
  /**
   * Enterprise-only management surface (multi-seat / compliance / reseller).
   * Hidden when the active EditionProfile is `personal`. Does NOT gate the
   * underlying feature — only the dashboard entry point.
   */
  enterprise?: boolean;
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
      { to: '/', icon: LayoutDashboard, label: 'nav.dashboard', desc: 'nav.dashboard.desc' },
      { to: '/webchat', icon: MessageCircle, label: 'nav.webchat', desc: 'nav.webchat.desc' },
    ],
  },
  {
    label: 'navGroup.agents',
    items: [
      { to: '/agents', icon: Bot, label: 'nav.agents', desc: 'nav.agents.desc' },
      { to: '/tasks', icon: KanbanSquare, label: 'nav.tasks', desc: 'nav.tasks.desc' },
      { to: '/forks', icon: GitFork, label: 'nav.forks', desc: 'nav.forks.desc' },
      { to: '/org', icon: Network, label: 'nav.org', desc: 'nav.org.desc', minRole: 'manager', enterprise: true },
      { to: '/memory', icon: Brain, label: 'nav.memory', desc: 'nav.memory.desc' },
    ],
  },
  {
    label: 'navGroup.knowledge',
    items: [
      { to: '/wiki', icon: BookOpen, label: 'nav.wiki', desc: 'nav.wiki.desc' },
      { to: '/shared-wiki', icon: Globe, label: 'nav.sharedWiki', desc: 'nav.sharedWiki.desc' },
      { to: '/skills', icon: Puzzle, label: 'nav.skills', desc: 'nav.skills.desc' },
      { to: '/marketplace', icon: Store, label: 'nav.marketplace', desc: 'nav.marketplace.desc' },
    ],
  },
  {
    label: 'navGroup.integrations',
    items: [
      { to: '/channels', icon: Radio, label: 'nav.channels', desc: 'nav.channels.desc', minRole: 'admin' },
      { to: '/mcp', icon: Plug, label: 'nav.mcp', desc: 'nav.mcp.desc', minRole: 'admin' },
      { to: '/mcp-keys', icon: KeyRound, label: 'nav.mcpKeys', desc: 'nav.mcpKeys.desc', minRole: 'admin' },
      { to: '/odoo', icon: Building2, label: 'nav.odoo', desc: 'nav.odoo.desc', minRole: 'admin' },
      { to: '/inference', icon: Cpu, label: 'nav.inference', desc: 'nav.inference.desc', minRole: 'admin' },
    ],
  },
  {
    label: 'navGroup.operations',
    items: [
      { to: '/accounts', icon: Wallet, label: 'nav.accounts', desc: 'nav.accounts.desc', minRole: 'admin' },
      { to: '/billing', icon: CreditCard, label: 'nav.billing', desc: 'nav.billing.desc', minRole: 'manager' },
      { to: '/reports', icon: BarChart3, label: 'nav.reports', desc: 'nav.reports.desc', minRole: 'manager' },
      { to: '/license', icon: KeyRound, label: 'nav.license', desc: 'nav.license.desc', minRole: 'manager' },
      { to: '/partner', icon: Handshake, label: 'nav.partner', desc: 'nav.partner.desc', minRole: 'manager', enterprise: true },
    ],
  },
  {
    label: 'navGroup.system',
    items: [
      { to: '/security', icon: Shield, label: 'nav.security', desc: 'nav.security.desc', minRole: 'admin' },
      { to: '/governance', icon: Scale, label: 'nav.governance', desc: 'nav.governance.desc', minRole: 'admin', enterprise: true },
      { to: '/wiki-trust', icon: Shield, label: 'nav.wikiTrust', desc: 'nav.wikiTrust.desc', minRole: 'admin', enterprise: true },
      { to: '/reliability', icon: Activity, label: 'nav.reliability', desc: 'nav.reliability.desc', minRole: 'admin' },
      { to: '/users', icon: Users, label: 'nav.users', desc: 'nav.users.desc', minRole: 'admin', enterprise: true },
      { to: '/logs', icon: FileText, label: 'nav.logs', desc: 'nav.logs.desc', minRole: 'manager' },
      { to: '/settings', icon: Settings, label: 'nav.settings', desc: 'nav.settings.desc', minRole: 'admin' },
    ],
  },
];
