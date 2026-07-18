import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useLocation } from 'react-router';
import {
  Search,
  CornerDownLeft,
  ArrowUp,
  ArrowDown,
  Sun,
  Moon,
  Monitor,
  Languages,
  LogOut,
  Bot,
  ClipboardList,
  type LucideIcon,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { fuzzyMatch, highlightSegments } from '@/lib/fuzzy';
import { dailyItems, navGroups, manageNav, manageEntry, type NavItem } from '@/components/layout/nav-model';
import { hasMinRole } from '@/lib/roles';
import { isVisible } from '@/lib/nav-visibility';
import { useForksExist } from '@/hooks/useForksExist';
import { CharacterAvatar } from '@/components/character';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useSystemStore } from '@/stores/system-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useTasksStore } from '@/stores/tasks-store';
import { useAuthStore } from '@/stores/auth-store';
import { useThemeStore } from '@/stores/theme-store';
import { useLocaleStore, localeNames } from '@/i18n';

interface Command {
  readonly id: string;
  readonly groupLabel: string;
  readonly label: string;
  /** One-line description shown under the label (nav commands). */
  readonly subtitle?: string;
  /** Extra Latin/alias tokens so CJK labels are reachable by English typing. */
  readonly keywords: string;
  readonly icon: LucideIcon;
  /** When set, the result row leads with the AI-staff character avatar for this
   *  agent id instead of the lucide icon (T2.3). */
  readonly avatarAgentId?: string;
  readonly perform: () => void;
  /** For nav commands: highlight active route + power "recent". */
  readonly route?: string;
}

interface ScoredCommand extends Command {
  readonly score: number;
  readonly indices: readonly number[];
}

/** Score a command against the query across label + keywords; keep label hits for highlight. */
function scoreCommand(query: string, cmd: Command): ScoredCommand | null {
  const labelMatch = fuzzyMatch(query, cmd.label);
  const keywordMatch = query.trim() ? fuzzyMatch(query, cmd.keywords) : null;
  if (!labelMatch && !keywordMatch) return null;
  const score = Math.max(labelMatch?.score ?? -Infinity, keywordMatch?.score ?? -Infinity);
  return { ...cmd, score, indices: labelMatch?.indices ?? [] };
}

export function CommandPalette() {
  const intl = useIntl();
  const navigate = useNavigate();
  const location = useLocation();

  const open = useCommandPaletteStore((s) => s.open);
  const closePalette = useCommandPaletteStore((s) => s.closePalette);
  const toggle = useCommandPaletteStore((s) => s.toggle);
  const recent = useCommandPaletteStore((s) => s.recent);

  const status = useSystemStore((s) => s.status);
  const user = useAuthStore((s) => s.user);
  const bindings = useAuthStore((s) => s.bindings);
  const agents = useAgentsStore((s) => s.agents);
  const tasks = useTasksStore((s) => s.tasks);
  const logout = useAuthStore((s) => s.logout);
  // Operator/owner binding gates sensitive `operatorOnly` commands (fail-closed).
  const hasOperatorAccess = bindings.some(
    (b) => b.access_level === 'owner' || b.access_level === 'operator',
  );
  // Progressive disclosure for /forks — same signal the Sidebar uses.
  const forksExist = useForksExist(hasMinRole(user?.role, 'manager'));
  const setTheme = useThemeStore((s) => s.setTheme);
  const setLocale = useLocaleStore((s) => s.setLocale);

  const [query, setQuery] = useState('');
  const [activeIndex, setActiveIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const t = useCallback((id: string) => intl.formatMessage({ id }), [intl]);

  // Global ⌘K / Ctrl+K toggle (works even when the palette is closed).
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.altKey && (e.key === 'k' || e.key === 'K')) {
        e.preventDefault();
        toggle();
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [toggle]);

  // Reset transient state whenever the palette opens; focus the input.
  useEffect(() => {
    if (open) {
      setQuery('');
      setActiveIndex(0);
      // Focus after paint so the dialog is mounted.
      requestAnimationFrame(() => inputRef.current?.focus());
    }
  }, [open]);

  const isPersonal = status?.edition_profile === 'personal';

  // Build the full command set (nav + actions), role/edition gated like the sidebar.
  const commands = useMemo<Command[]>(() => {
    // The three collapsible groups (工作 / 公司 / 設定) live in `navGroups`; the
    // flat daily row sits outside it — fold it back in so ⌘K reaches every
    // destination (T1.5). `staffEntry` / `manageEntry` are already inside
    // `navGroups`, so they must NOT be appended again (duplicate route id).
    const navSources: Array<{ item: NavItem; groupLabel: string }> = [
      ...dailyItems.map((item) => ({ item, groupLabel: 'navGroup.daily' })),
      ...navGroups.flatMap((group) => group.items.map((item) => ({ item, groupLabel: group.label }))),
    ];
    const visibilityCtx = { hasOperatorAccess, forksExist };
    const navCommands: Command[] = navSources
      .filter(({ item }) => isVisible(item, user?.role, isPersonal, visibilityCtx))
      .map(({ item, groupLabel }) => ({
        id: `nav:${item.to}`,
        groupLabel: t(groupLabel),
        label: t(item.label),
        subtitle: t(item.desc),
        // Latin alias from the i18n id (e.g. "nav.settings" → "settings") + route
        // + the localized description so users can search by what a page does.
        keywords: `${item.label.replace(/^nav\./, '')} ${item.to} ${t(item.desc)}`,
        icon: item.icon,
        route: item.to,
        perform: () => navigate(item.to),
      }));

    // Zone D management pages live behind a single sidebar entry, so ⌘K is the
    // primary way to reach them directly (dashboard-redesign §3.1, T1.3).
    const manageCommands: Command[] = manageNav
      .filter((item) => isVisible(item, user?.role, isPersonal, visibilityCtx))
      .map((item) => ({
        id: `nav:${item.to}`,
        groupLabel: t(manageEntry.label),
        label: t(item.label),
        subtitle: t(item.desc),
        keywords: `${item.label.replace(/^manage\./, '')} ${item.to} ${t(item.desc)} manage 管理`,
        icon: item.icon,
        route: item.to,
        perform: () => navigate(item.to),
      }));

    // Entity search (T1.3) — jump straight to a specific AI staff detail page.
    const agentCommands: Command[] = agents.map((a) => ({
      id: `entity:agent:${a.name}`,
      groupLabel: t('cmdk.group.agents'),
      label: a.display_name,
      subtitle: a.name,
      keywords: `${a.name} ${a.display_name} staff 員工`,
      icon: Bot,
      avatarAgentId: a.name,
      route: `/agents/${a.name}`,
      perform: () => navigate(`/agents/${encodeURIComponent(a.name)}`),
    }));

    // Entity search (T1.5) — jump to a task detail by fuzzy title (CJK-safe via
    // the shared `fuzzyMatch`). Sourced from whatever the tasks store holds.
    const taskCommands: Command[] = tasks.map((task) => ({
      id: `entity:task:${task.id}`,
      groupLabel: t('cmdk.group.tasks'),
      label: task.title,
      subtitle: task.id,
      keywords: `${task.title} ${task.id} task 任務`,
      icon: ClipboardList,
      route: `/tasks/${task.id}`,
      perform: () => navigate(`/tasks/${encodeURIComponent(task.id)}`),
    }));

    const actionGroup = t('cmdk.group.actions');
    const themeActions: Command[] = (['light', 'dark', 'system'] as const).map((th) => ({
      id: `action:theme:${th}`,
      groupLabel: actionGroup,
      label: t(`cmdk.action.theme.${th}`),
      keywords: `theme appearance ${th} dark light 主題 外觀`,
      icon: th === 'light' ? Sun : th === 'dark' ? Moon : Monitor,
      perform: () => setTheme(th),
    }));

    const localeActions: Command[] = Object.entries(localeNames).map(([code, name]) => ({
      id: `action:locale:${code}`,
      groupLabel: actionGroup,
      label: t('cmdk.action.language') + ' — ' + name,
      keywords: `language locale ${code} ${name} 語言 言語`,
      icon: Languages,
      perform: () => setLocale(code),
    }));

    const logoutAction: Command = {
      id: 'action:logout',
      groupLabel: actionGroup,
      label: t('auth.logout'),
      keywords: 'logout sign out 登出',
      icon: LogOut,
      perform: () => logout(),
    };

    return [...navCommands, ...manageCommands, ...agentCommands, ...taskCommands, ...themeActions, ...localeActions, logoutAction];
  }, [t, user?.role, hasOperatorAccess, forksExist, agents, tasks, isPersonal, navigate, setTheme, setLocale, logout]);

  // Empty query → recent routes first, then all commands in natural order.
  const results = useMemo<ScoredCommand[]>(() => {
    if (query.trim() === '') {
      const byRoute = new Map(commands.filter((c) => c.route).map((c) => [c.route!, c]));
      const recentCmds = recent
        .map((r) => byRoute.get(r))
        .filter((c): c is Command => Boolean(c))
        .map((c) => ({ ...c, score: 0, indices: [] as number[], groupLabel: t('cmdk.group.recent') }));
      const recentRoutes = new Set(recent);
      const rest = commands
        .filter((c) => !(c.route && recentRoutes.has(c.route)))
        .map((c) => ({ ...c, score: 0, indices: [] as number[] }));
      return [...recentCmds, ...rest];
    }
    return commands
      .map((c) => scoreCommand(query, c))
      .filter((c): c is ScoredCommand => c !== null)
      .sort((a, b) => b.score - a.score);
  }, [query, commands, recent, t]);

  // Keep the active index in range as results shrink/grow.
  useEffect(() => {
    setActiveIndex((i) => (i >= results.length ? Math.max(0, results.length - 1) : i));
  }, [results.length]);

  // Group results for section headers while preserving flat index for keyboard nav.
  const grouped = useMemo(() => {
    const order: string[] = [];
    const map = new Map<string, { cmd: ScoredCommand; index: number }[]>();
    results.forEach((cmd, index) => {
      if (!map.has(cmd.groupLabel)) {
        map.set(cmd.groupLabel, []);
        order.push(cmd.groupLabel);
      }
      map.get(cmd.groupLabel)!.push({ cmd, index });
    });
    return order.map((label) => ({ label, items: map.get(label)! }));
  }, [results]);

  const run = useCallback(
    (cmd: ScoredCommand | undefined) => {
      if (!cmd) return;
      closePalette();
      cmd.perform();
    },
    [closePalette]
  );

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      closePalette();
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      setActiveIndex((i) => (results.length === 0 ? 0 : (i + 1) % results.length));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setActiveIndex((i) => (results.length === 0 ? 0 : (i - 1 + results.length) % results.length));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      run(results[activeIndex]);
    }
  };

  // Scroll the active option into view on keyboard movement.
  useEffect(() => {
    if (!open) return;
    const el = listRef.current?.querySelector<HTMLElement>(`[data-cmdk-index="${activeIndex}"]`);
    el?.scrollIntoView({ block: 'nearest' });
  }, [activeIndex, open]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-[100] flex items-start justify-center px-4 pt-[20vh]"
      role="dialog"
      aria-modal="true"
      aria-label={t('cmdk.title')}
    >
      {/* Scrim (spec §4 Dialog overlay) */}
      <button
        type="button"
        aria-hidden="true"
        tabIndex={-1}
        onClick={closePalette}
        className="absolute inset-0 cursor-default bg-black/10 backdrop-blur-xs"
      />

      <div
        className="relative flex w-full max-w-[calc(100%-2rem)] flex-col overflow-hidden rounded-xl bg-surface-raised text-surface-foreground shadow-[var(--floating-shadow)] ring-1 ring-surface-border sm:max-w-xl"
        onKeyDown={onKeyDown}
      >
        {/* Search input row (spec §5.7) */}
        <div className="flex items-center gap-3 border-b border-surface-border px-4 py-3">
          <Search className="size-5 shrink-0 text-muted-foreground" aria-hidden="true" />
          <input
            ref={inputRef}
            type="text"
            role="combobox"
            aria-expanded="true"
            aria-controls="cmdk-listbox"
            aria-activedescendant={results[activeIndex] ? `cmdk-opt-${activeIndex}` : undefined}
            aria-autocomplete="list"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setActiveIndex(0);
            }}
            placeholder={t('cmdk.placeholder')}
            className="flex-1 bg-transparent text-sm text-foreground placeholder:text-muted-foreground focus:outline-none"
            autoComplete="off"
            spellCheck={false}
          />
          <kbd className="hidden shrink-0 rounded border border-border px-1.5 py-0.5 font-mono text-[10px] leading-none text-muted-foreground sm:inline-block">
            ESC
          </kbd>
        </div>

        {/* Results */}
        <div
          ref={listRef}
          id="cmdk-listbox"
          role="listbox"
          aria-label={t('cmdk.title')}
          className="max-h-[min(400px,50vh)] overflow-y-auto overscroll-contain p-2"
        >
          {results.length === 0 ? (
            <div className="px-3 py-10 text-center text-sm text-muted-foreground">
              {t('cmdk.empty')}
            </div>
          ) : (
            grouped.map((group) => (
              <div key={group.label} className="mb-1 last:mb-0">
                <div className="px-3 pb-1 pt-2 text-xs font-medium text-muted-foreground">
                  {group.label}
                </div>
                {group.items.map(({ cmd, index }) => {
                  const isActive = index === activeIndex;
                  const isCurrent = cmd.route && cmd.route === location.pathname;
                  const Icon = cmd.icon;
                  return (
                    <div
                      key={cmd.id}
                      id={`cmdk-opt-${index}`}
                      data-cmdk-index={index}
                      data-selected={isActive || undefined}
                      role="option"
                      aria-selected={isActive}
                      onClick={() => run(cmd)}
                      onMouseMove={() => setActiveIndex(index)}
                      className={cn(
                        'flex cursor-pointer items-start gap-3 rounded-lg px-3 py-2.5 text-sm transition-colors',
                        isActive ? 'bg-accent text-accent-foreground' : 'text-foreground',
                      )}
                    >
                      {cmd.avatarAgentId ? (
                        <span className="mt-0.5 shrink-0">
                          <CharacterAvatar agentId={cmd.avatarAgentId} name={cmd.label} size={20} />
                        </span>
                      ) : (
                        <Icon
                          className={cn(
                            'mt-0.5 size-[1.125rem] shrink-0',
                            isActive ? 'text-foreground' : 'text-muted-foreground',
                          )}
                          aria-hidden="true"
                        />
                      )}
                      <span className="min-w-0 flex-1">
                        <span className="block truncate leading-tight">
                          {highlightSegments(cmd.label, cmd.indices).map((seg, i) =>
                            seg.hit ? (
                              <mark key={i} className="bg-transparent font-medium text-brand">
                                {seg.text}
                              </mark>
                            ) : (
                              <span key={i}>{seg.text}</span>
                            )
                          )}
                        </span>
                        {cmd.subtitle && (
                          <span className="mt-0.5 block truncate text-xs leading-tight text-muted-foreground">
                            {cmd.subtitle}
                          </span>
                        )}
                      </span>
                      {isCurrent && (
                        <span className="mt-1 shrink-0 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
                          {t('cmdk.current')}
                        </span>
                      )}
                      {isActive && (
                        <CornerDownLeft className="mt-1 size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
                      )}
                    </div>
                  );
                })}
              </div>
            ))
          )}
        </div>

        {/* Footer hints */}
        <div className="flex items-center gap-4 border-t border-surface-border bg-surface-hover/70 px-4 py-2 text-xs text-muted-foreground">
          <span className="flex items-center gap-1">
            <ArrowUp className="size-3" />
            <ArrowDown className="size-3" />
            {t('cmdk.hint.navigate')}
          </span>
          <span className="flex items-center gap-1">
            <CornerDownLeft className="size-3" />
            {t('cmdk.hint.select')}
          </span>
        </div>
      </div>
    </div>
  );
}
