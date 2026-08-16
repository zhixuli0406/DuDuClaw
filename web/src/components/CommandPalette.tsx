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
  Loader2,
  MessagesSquare,
  FileText,
  Brain,
  BookOpen,
  type LucideIcon,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { isImeComposing } from '@/lib/keyboard';
import { fuzzyMatch, highlightSegments } from '@/lib/fuzzy';
import { api, type SearchHit } from '@/lib/api';
import { looksLikeIdQuery, findIdMatch, idMatchRoute } from '@/lib/id-lookup';
import { useDataScope, useVisibleAgents } from '@/lib/data-scope';
import {
  allManageNav,
  conversationsEntry,
  inboxEntry,
  manageEntry,
  navGroups,
  navGroupsForEdition,
  personalAdvancedGroup,
  primaryItemsForEdition,
  staffEntry,
  type NavItem,
} from '@/components/layout/nav-model';
import { hasMinRole } from '@/lib/roles';
import { isVisible } from '@/lib/nav-visibility';
import { isTauri } from '@/lib/gateway-picker';
import { useForksExist } from '@/hooks/useForksExist';
import { CharacterAvatar } from '@/components/character';
import { useCommandPaletteStore } from '@/stores/command-palette-store';
import { useSystemStore } from '@/stores/system-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useConversationsStore } from '@/stores/conversations-store';
import { useTasksStore } from '@/stores/tasks-store';
import { useAuthStore } from '@/stores/auth-store';
import { useThemeStore } from '@/stores/theme-store';
import { useLocaleStore, localeNames } from '@/i18n';

/** How long to let the user keep typing/pasting before spending the id-lookup
 *  RPC round-trip (debounce for `IdDirectJump`). */
const ID_LOOKUP_DEBOUNCE_MS = 200;

/** I-5: debounce before spending the `search.query` round trip. Slightly
 *  longer than id-lookup — content search fans out across four surfaces
 *  server-side, so it's worth waiting a beat longer for typing to settle. */
const CONTENT_SEARCH_DEBOUNCE_MS = 250;
/** Below this length a content search is almost certainly not intentional
 *  yet (still typing the first word) — skip the round trip entirely. */
const CONTENT_SEARCH_MIN_CHARS = 2;
/** Per-source cap for content-search hits shown in the palette — this list
 *  already carries nav/entity results, so content hits stay a supplement, not
 *  the whole picture (open the dedicated page for the full result set). */
const CONTENT_SEARCH_LIMIT = 5;

/** Icon per `SearchHit.source` for the content-search result rows (I-5). */
function iconForSearchSource(source: SearchHit['source']): LucideIcon {
  switch (source) {
    case 'conversation':
      return MessagesSquare;
    case 'artifact':
      return FileText;
    case 'memory':
      return Brain;
    case 'wiki':
    case 'shared_wiki':
      return BookOpen;
    default:
      return Search;
  }
}

/**
 * I-5: navigate to the page that owns a content-search hit. Each source has
 * its own destination shape (`SearchHit.jump`, documented in
 * `search_index.rs`) since the four surfaces have nothing in common:
 *
 * - `conversation` resumes the actual session and lands on `/chat` — reusing
 *   the exact resume flow the ID-lookup flow below already uses, so a
 *   content-search hit opens the conversation the same way pasting its id
 *   would.
 * - `artifact` jumps to `/files` pre-scoped to the hit's agent/task — both
 *   params `FilesPage` already reads from the URL (P11 state-as-URL), so no
 *   other page needs to change for this to work.
 * - `memory` and `wiki`/`shared_wiki` land on the matching `MemoryPage` tab.
 *   `MemoryPage` does not (yet) read an `?agent=` param, so this cannot
 *   pre-select the hit's specific agent/entry — carrying the original query
 *   into `?q=` (which the memories tab DOES read) is the honest next-best
 *   thing rather than a silent no-op.
 */
function jumpToSearchHit(hit: SearchHit, query: string, navigate: (to: string) => void): void {
  const jump = hit.jump ?? {};
  const str = (v: unknown): string | undefined => (typeof v === 'string' && v ? v : undefined);

  switch (hit.source) {
    case 'conversation': {
      const sessionId = str(jump.session_id);
      if (!sessionId) {
        navigate('/conversations');
        break;
      }
      void useConversationsStore
        .getState()
        .fetch()
        .then(() => {
          const session = useConversationsStore.getState().sessions.find((s) => s.session_id === sessionId);
          if (!session) {
            navigate('/conversations');
            return;
          }
          return useConversationsStore
            .getState()
            .resume(session)
            .then((ok) => {
              if (ok) navigate('/chat');
            });
        })
        .catch(() => navigate('/conversations'));
      break;
    }
    case 'artifact': {
      const params = new URLSearchParams();
      const agentId = str(jump.agent_id);
      const taskId = str(jump.task_id);
      if (agentId) params.set('agent', agentId);
      if (taskId) params.set('task', taskId);
      const qs = params.toString();
      navigate(`/files${qs ? `?${qs}` : ''}`);
      break;
    }
    case 'memory': {
      const params = new URLSearchParams({ tab: 'memories' });
      if (query.trim()) params.set('q', query.trim());
      navigate(`/memory?${params.toString()}`);
      break;
    }
    case 'wiki':
      navigate('/memory?tab=wiki');
      break;
    case 'shared_wiki':
      navigate('/memory?tab=shared');
      break;
    default:
      break;
  }
}

interface Command {
  readonly id: string;
  readonly groupLabel: string;
  /** Route the group's section header navigates to when clicked (X10: "分組
   *  標題可一鍵前往該分組"). Undefined for utility groups with no representative
   *  page (最近 / 動作) — their header stays plain, non-interactive text. */
  readonly groupRoute?: string;
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

/**
 * Landing route for a nav-model group's section header (X10 audit fix — the
 * header showed which group a result belonged to but offered no way to jump
 * to that group as a whole). A group lands on its first, highest-priority
 * item's route — with one exception: 每日's first item is 新對話, an *action*
 * (clears the chat view) rather than a page, so 每日 lands on 儀表板 instead.
 */
function groupLandingRoute(rawGroupLabel: string): string | undefined {
  if (rawGroupLabel === 'navGroup.daily') return '/';
  if (rawGroupLabel === personalAdvancedGroup.label) return personalAdvancedGroup.items[0]?.to;
  return navGroups.find((g) => g.label === rawGroupLabel)?.items[0]?.to;
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
  // I-5: content search scoping — mirrors FilesPage's convention. Admins
  // (`scope === 'all'`) may omit `agent_id` (the gateway then searches
  // conversations/artifacts across every agent; memory/wiki still degrade to
  // zero hits without one — see `handle_search_query`'s doc comment); every
  // other role must supply one, so this defaults to the first agent the
  // viewer can see.
  const dataScope = useDataScope();
  const visibleAgentsForSearch = useVisibleAgents();
  const searchAgentId = visibleAgentsForSearch[0]?.name;
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
  // I-5: cross-source content search — `search.query` results, appended after
  // the local nav/entity matches (see the `results` memo below). Kept
  // separate from `results` itself because it resolves asynchronously.
  const [contentHits, setContentHits] = useState<readonly SearchHit[]>([]);
  const [contentSearching, setContentSearching] = useState(false);
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
      setContentHits([]);
      setContentSearching(false);
      // Focus after paint so the dialog is mounted.
      requestAnimationFrame(() => inputRef.current?.focus());
    }
  }, [open]);

  // I-5: debounced cross-source content search. Skipped entirely for a
  // non-admin viewer with no visible agent (the RPC would just reject it —
  // see the doc comment on `handle_search_query` — so this fails quiet on the
  // client rather than spending a round trip that can only error).
  useEffect(() => {
    const trimmed = query.trim();
    if (!open || trimmed.length < CONTENT_SEARCH_MIN_CHARS) {
      setContentHits([]);
      setContentSearching(false);
      return;
    }
    if (dataScope !== 'all' && !searchAgentId) {
      setContentHits([]);
      setContentSearching(false);
      return;
    }
    let cancelled = false;
    setContentSearching(true);
    const timer = setTimeout(() => {
      api.search
        .query({
          q: trimmed,
          ...(searchAgentId ? { agent_id: searchAgentId } : {}),
          limit: CONTENT_SEARCH_LIMIT,
        })
        .then((res) => {
          if (!cancelled) setContentHits(res?.hits ?? []);
        })
        .catch(() => {
          if (!cancelled) setContentHits([]);
        })
        .finally(() => {
          if (!cancelled) setContentSearching(false);
        });
    }, CONTENT_SEARCH_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [open, query, dataScope, searchAgentId]);

  const isPersonal = status?.edition_profile === 'personal';

  // Build the full command set (nav + actions), role/edition gated like the sidebar.
  const commands = useMemo<Command[]>(() => {
    // The three collapsible groups (工作 / 公司 / 設定) live in `navGroups`; the
    // flat daily row sits outside it — fold it back in so ⌘K reaches every
    // destination (T1.5). `staffEntry` / `manageEntry` are already inside
    // `navGroups`, so they must NOT be appended again (duplicate route id).
    const navSources: Array<{ item: NavItem; groupLabel: string }> = [
      ...primaryItemsForEdition(isPersonal).map((item) => ({ item, groupLabel: 'navGroup.daily' })),
      // 收件匣 and 對話紀錄 left / never joined the standing rail (2026-08-04,
      // D17) — ⌘K is how you reach them by name, so fold them back in here.
      { item: inboxEntry, groupLabel: 'navGroup.daily' },
      { item: conversationsEntry, groupLabel: 'navGroup.daily' },
      ...navGroupsForEdition(isPersonal).flatMap((group) =>
        group.items.map((item) => ({ item, groupLabel: group.label })),
      ),
    ];
    // D6: mirrors AppSidebar's ctx — `agents` here already feeds the
    // agent-jump commands below, so the org chart's progressive-disclosure
    // gate is a free read, not a second fetch.
    const visibilityCtx = { hasOperatorAccess, forksExist, isDesktop: isTauri(), agentCount: agents.length };
    const navCommands: Command[] = navSources
      .filter(({ item }) => isVisible(item, user?.role, isPersonal, visibilityCtx))
      .map(({ item, groupLabel }) => ({
        id: `nav:${item.to}`,
        groupLabel: t(groupLabel),
        groupRoute: groupLandingRoute(groupLabel),
        label: t(item.label),
        subtitle: t(item.desc),
        // Latin alias from the i18n id (e.g. "nav.settings" → "settings") + route
        // + the localized description so users can search by what a page does.
        keywords: `${item.label.replace(/^nav\./, '')} ${item.to} ${t(item.desc)}`,
        icon: item.icon,
        route: item.to,
        // Action rows (新對話) clear the chat view first, so ⌘K behaves exactly
        // like clicking the sidebar row — see `NavItem.action`.
        perform: () => {
          if (item.action === 'newConversation') useConversationsStore.getState().startNew();
          navigate(item.to);
        },
      }));

    // Zone D management pages live behind a single sidebar entry, so ⌘K is the
    // primary way to reach them directly (dashboard-redesign §3.1, T1.3).
    const manageCommands: Command[] = allManageNav
      .filter((item) => isVisible(item, user?.role, isPersonal, visibilityCtx))
      .map((item) => ({
        id: `nav:${item.to}`,
        groupLabel: t(manageEntry.label),
        groupRoute: manageEntry.to,
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
      groupRoute: staffEntry.to,
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
      groupRoute: '/tasks',
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

    // D4-A: the Personal edition signs itself back in automatically on the next
    // page load, so a logout command there is a button that undoes itself.
    // Dropped rather than shown-and-broken; password protection is a config
    // switch (`local_auto_login`), surfaced in account settings.
    const logoutAction: Command[] = isPersonal
      ? []
      : [{
          id: 'action:logout',
          groupLabel: actionGroup,
          label: t('auth.logout'),
          keywords: 'logout sign out 登出',
          icon: LogOut,
          perform: () => logout(),
        }];

    return [...navCommands, ...manageCommands, ...agentCommands, ...taskCommands, ...themeActions, ...localeActions, ...logoutAction];
  }, [t, user?.role, hasOperatorAccess, forksExist, agents, tasks, isPersonal, navigate, setTheme, setLocale, logout]);

  // Empty query → recent routes first, then all commands in natural order.
  const results = useMemo<ScoredCommand[]>(() => {
    let base: ScoredCommand[];
    if (query.trim() === '') {
      const byRoute = new Map(commands.filter((c) => c.route).map((c) => [c.route!, c]));
      const recentCmds = recent
        .map((r) => byRoute.get(r))
        .filter((c): c is Command => Boolean(c))
        // 最近 is a transient re-labeling of commands pulled from various groups —
        // clear the inherited `groupRoute` so its header doesn't point at
        // whichever original group happened to contribute the first item.
        .map((c) => ({ ...c, score: 0, indices: [] as number[], groupLabel: t('cmdk.group.recent'), groupRoute: undefined }));
      const recentRoutes = new Set(recent);
      const rest = commands
        .filter((c) => !(c.route && recentRoutes.has(c.route)))
        .map((c) => ({ ...c, score: 0, indices: [] as number[] }));
      base = [...recentCmds, ...rest];
    } else {
      base = commands
        .map((c) => scoreCommand(query, c))
        .filter((c): c is ScoredCommand => c !== null)
        .sort((a, b) => b.score - a.score);
    }
    // I-5: content-search hits are already server-matched (no local fuzzy
    // scoring), so they're appended as-is rather than folded into the scored
    // sort above. Grouped-by-source below via `groupLabel`, same mechanism as
    // every other section (X10).
    if (query.trim().length >= CONTENT_SEARCH_MIN_CHARS && contentHits.length > 0) {
      const contentCmds: ScoredCommand[] = contentHits.map((hit) => ({
        id: `content:${hit.source}:${hit.id}`,
        groupLabel: t(`cmdk.group.content.${hit.source}`),
        label: hit.title || hit.snippet || hit.id,
        subtitle: hit.snippet,
        keywords: '',
        icon: iconForSearchSource(hit.source),
        perform: () => jumpToSearchHit(hit, query, navigate),
        score: 0,
        indices: [] as number[],
      }));
      return [...base, ...contentCmds];
    }
    return base;
  }, [query, commands, recent, t, contentHits, navigate]);

  // Keep the active index in range as results shrink/grow.
  useEffect(() => {
    setActiveIndex((i) => (i >= results.length ? Math.max(0, results.length - 1) : i));
  }, [results.length]);

  // ── ID direct-jump (W3-3, Stripe pattern B4): pasting any object id — task
  // / approval / install / agent / conversation — jumps straight to its
  // detail view. Local nav + entity commands above already cover the common
  // case (agents/tasks already loaded into their stores render as exact
  // fuzzy hits), so this only spends the RPC round-trip when every local
  // command already missed AND the query is shaped like a pasted id, not a
  // search phrase (`looksLikeIdQuery`). Queries every object-kind list RPC in
  // parallel and jumps on the first — and only ever — hit; an all-miss shows
  // "not found" instead of the generic empty state.
  const [idLookup, setIdLookup] = useState<'idle' | 'searching' | 'notFound'>('idle');
  useEffect(() => {
    if (!open || !looksLikeIdQuery(query) || results.length > 0) {
      setIdLookup('idle');
      return;
    }
    let cancelled = false;
    setIdLookup('searching');
    const q = query.trim();
    const timer = setTimeout(() => {
      Promise.all([
        api.tasks.list().catch(() => null),
        api.approvals.list().catch(() => null),
        api.installRequests.list().catch(() => null),
        useConversationsStore
          .getState()
          .fetch()
          .then(() => useConversationsStore.getState().sessions)
          .catch(() => []),
      ]).then(([tasksRes, approvalsRes, installRes, conversations]) => {
        if (cancelled) return;
        const match = findIdMatch(q, {
          tasks: tasksRes?.tasks,
          approvals: approvalsRes?.approvals,
          installs: installRes?.requests,
          agents,
          conversations,
        });
        if (!match) {
          setIdLookup('notFound');
          return;
        }
        setIdLookup('idle');
        closePalette();
        if (match.kind === 'conversation') {
          const session = conversations.find((c) => c.session_id === match.id);
          if (session) {
            void useConversationsStore
              .getState()
              .resume(session)
              .then((ok) => {
                if (ok) navigate('/chat');
              });
          }
          return;
        }
        const route = idMatchRoute(match);
        if (route) navigate(route);
      });
    }, ID_LOOKUP_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [open, query, results.length, agents, navigate, closePalette]);

  // Group results for section headers while preserving flat index for keyboard nav.
  const grouped = useMemo(() => {
    const order: string[] = [];
    const map = new Map<string, { cmd: ScoredCommand; index: number }[]>();
    const routeByLabel = new Map<string, string | undefined>();
    results.forEach((cmd, index) => {
      if (!map.has(cmd.groupLabel)) {
        map.set(cmd.groupLabel, []);
        order.push(cmd.groupLabel);
        routeByLabel.set(cmd.groupLabel, cmd.groupRoute);
      }
      map.get(cmd.groupLabel)!.push({ cmd, index });
    });
    return order.map((label) => ({ label, items: map.get(label)!, route: routeByLabel.get(label) }));
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
    } else if (e.key === 'Enter' && !isImeComposing(e)) {
      // Skip while a CJK IME is composing the filter text — Enter confirms the
      // candidate, it must not fire the highlighted command.
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
            <div className="flex flex-col items-center gap-2 px-3 py-10 text-center text-sm text-muted-foreground">
              {idLookup === 'searching' ? (
                <>
                  <Loader2 className="size-4 animate-spin" aria-hidden="true" />
                  {t('cmdk.idLookup.searching')}
                </>
              ) : idLookup === 'notFound' ? (
                intl.formatMessage({ id: 'cmdk.idLookup.notFound' }, { query })
              ) : contentSearching ? (
                <>
                  <Loader2 className="size-4 animate-spin" aria-hidden="true" />
                  {t('cmdk.contentSearch.searching')}
                </>
              ) : (
                t('cmdk.empty')
              )}
            </div>
          ) : (
            grouped.map((group) => (
              <div key={group.label} className="mb-1 last:mb-0">
                {group.route ? (
                  // X10 fix: a group header with a representative page (every
                  // nav-model group + the 員工/任務 entity groups) is a real
                  // shortcut to that page, not just a label.
                  <button
                    type="button"
                    onClick={() => {
                      closePalette();
                      navigate(group.route!);
                    }}
                    className="block w-full rounded px-3 pb-1 pt-2 text-left text-xs font-medium text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
                  >
                    {group.label}
                  </button>
                ) : (
                  <div className="px-3 pb-1 pt-2 text-xs font-medium text-muted-foreground">
                    {group.label}
                  </div>
                )}
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
