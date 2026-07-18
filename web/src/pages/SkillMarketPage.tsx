import { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { cn } from '@/lib/utils';
import { api, type SkillIndexEntry, type SharedSkillInfo, type SkillInfo, type SkillLeaderboardEntry } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { departmentsOf } from '@/lib/agents';
import { CustomSkillsSection } from '@/components/skills/CustomSkillsSection';
import { toast, formatError } from '@/lib/toast';
import { glyphText } from '@/lib/agent-glyph';
import {
  CollectionPageHeader,
  CollectionPageState,
  Card,
  CardContent,
  Button,
  Badge,
  Input,
  Segmented,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  SelectGroup,
  SelectLabel,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
  ActorAvatar,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  type SegmentedOption,
} from '@/components/mds';
import {
  Search,
  Tag,
  ExternalLink,
  Download,
  Shield,
  ShieldCheck,
  ShieldAlert,
  AlertTriangle,
  CheckCircle,
  Loader2,
  Share2,
  Sparkles,
  Trophy,
  MoreHorizontal,
  Link as LinkIcon,
  Wrench,
} from 'lucide-react';

interface VetResult {
  passed: boolean;
  findings: Array<{ severity: string; category: string; description: string }>;
  score: number;
}

interface VetResponse {
  skill_name: string;
  content: string;
  vet_result: VetResult;
  passed: boolean;
}

type SkillTab = 'market' | 'shared' | 'mySkills' | 'leaderboard';

/**
 * SkillMarketPage — the Multica "技能" collection (spec §5.2 + §4/§5.5). A
 * CollectionPageHeader + a Segmented section switcher (market / team / mine /
 * leaderboard) with a search field on the right. Installing, sharing, adopting,
 * the vet-findings dialog and the self-built-skill wizard entry are unchanged;
 * only the surface is re-skinned onto MDS. The Calm-Glass primitives are gone.
 */
export function SkillMarketPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const [activeTab, setActiveTab] = useState<SkillTab>('market');

  // Market search is lifted to the page so its Input can live in the header
  // control row (spec §5.2) alongside the section switcher.
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<SkillIndexEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [searched, setSearched] = useState(false);
  const [installSkill, setInstallSkill] = useState<SkillIndexEntry | null>(null);
  const [showUrlImport, setShowUrlImport] = useState(false);

  // "我的技能" client-side name filter (shares the same header Input).
  const [mineFilter, setMineFilter] = useState('');

  const runSearch = useCallback(
    async (q: string) => {
      if (!q.trim()) return;
      setLoading(true);
      setSearched(true);
      try {
        const res = await api.skillMarket.search(q);
        setResults(res?.skills ?? []);
      } catch (e) {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
        setResults([]);
      } finally {
        setLoading(false);
      }
    },
    [intl],
  );

  const tabOptions: SegmentedOption<SkillTab>[] = [
    { value: 'market', label: intl.formatMessage({ id: 'skills.tab.market' }) },
    { value: 'shared', label: intl.formatMessage({ id: 'skills.tab.shared' }) },
    { value: 'mySkills', label: intl.formatMessage({ id: 'skills.tab.mySkills' }) },
    { value: 'leaderboard', label: intl.formatMessage({ id: 'skills.tab.leaderboard' }) },
  ];

  const showSearch = activeTab === 'market' || activeTab === 'mySkills';

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader
        hideTrigger
        icon={Wrench}
        title={intl.formatMessage({ id: 'nav.skills' })}
        description={intl.formatMessage({ id: 'skills.market.title' })}
        action={
          // Primary CTA: build a skill — coexists with the "install from market"
          // flow on this one converged /skills page. `/skills/new` route stays
          // for deep-link compatibility.
          <Button variant="brand" size="sm" onClick={() => navigate('/skills/new')}>
            <Sparkles />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'skills.new.title' })}</span>
          </Button>
        }
      />

      {/* Control row: section switcher + search (spec §5.2). */}
      <div className="flex h-12 shrink-0 items-center gap-2 overflow-x-auto border-b border-surface-border px-4">
        <Segmented
          value={activeTab}
          onValueChange={setActiveTab}
          options={tabOptions}
          aria-label={intl.formatMessage({ id: 'nav.skills' })}
        />
        {showSearch && (
          <div className="relative ml-auto shrink-0">
            <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={activeTab === 'market' ? query : mineFilter}
              onChange={(e) =>
                activeTab === 'market' ? setQuery(e.target.value) : setMineFilter(e.target.value)
              }
              onKeyDown={(e) => {
                if (activeTab === 'market' && e.key === 'Enter') runSearch(query);
              }}
              placeholder={intl.formatMessage({
                id: activeTab === 'market' ? 'skills.market.searchPlaceholder' : 'skills.my.filterPlaceholder',
              })}
              className="w-44 pl-8 sm:w-64"
            />
          </div>
        )}
        {activeTab === 'market' && (
          <Button
            variant="outline"
            size="sm"
            className={showSearch ? undefined : 'ml-auto'}
            onClick={() => setShowUrlImport(true)}
          >
            <LinkIcon />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'skills.import.fromUrl' })}</span>
          </Button>
        )}
      </div>

      <div className="flex flex-1 flex-col p-4 md:p-6">
        {activeTab === 'market' && (
          <MarketTab
            query={query}
            onCategory={(cat) => {
              setQuery(cat);
              runSearch(cat);
            }}
            results={results}
            loading={loading}
            searched={searched}
            onInstall={setInstallSkill}
          />
        )}
        {activeTab === 'shared' && <SharedSkillsTab />}
        {activeTab === 'mySkills' && <MySkillsTab filter={mineFilter} />}
        {activeTab === 'leaderboard' && <LeaderboardTab />}
      </div>

      {installSkill && <InstallDialog skill={installSkill} onClose={() => setInstallSkill(null)} />}
      {showUrlImport && <UrlImportDialog onClose={() => setShowUrlImport(false)} onInstall={setInstallSkill} />}
    </div>
  );
}

// ── Leaderboard Tab (WP10-T10.1 — honour roll, §5.5 ranking cards) ──────────

function LeaderboardTab() {
  const intl = useIntl();
  const [entries, setEntries] = useState<SkillLeaderboardEntry[]>([]);
  const [note, setNote] = useState<string>('');
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    setLoading(true);
    (async () => {
      try {
        const res = await api.skills.leaderboard();
        setEntries(res?.leaderboard ?? []);
        setNote(res?.note ?? '');
      } catch (e) {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
        setEntries([]);
      } finally {
        setLoading(false);
      }
    })();
  }, [intl]);

  const max = useMemo(
    () => entries.reduce((m, e) => Math.max(m, e.estimated_minutes_saved), 0),
    [entries],
  );

  if (loading) return <CollectionPageState state="loading" />;
  if (entries.length === 0) {
    return (
      <CollectionPageState
        state="empty"
        icon={Trophy}
        title={intl.formatMessage({ id: 'skills.leaderboard.empty' })}
        description={intl.formatMessage({ id: 'skills.leaderboard.subtitle' })}
      />
    );
  }

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">
        {intl.formatMessage({ id: 'skills.leaderboard.subtitle' })}
      </p>
      <Card>
        <CardContent className="divide-y divide-surface-border">
          {entries.map((entry, i) => {
            const pct = max > 0 ? Math.max(4, Math.round((entry.estimated_minutes_saved / max) * 100)) : 0;
            return (
              <div key={`${entry.owner}/${entry.skill}`} className="flex items-center gap-3 py-3 first:pt-0 last:pb-0">
                <span className="w-5 shrink-0 text-center font-mono text-xs tabular-nums text-muted-foreground">
                  {i + 1}
                </span>
                <div className="min-w-0 flex-1">
                  <div className="flex items-center justify-between gap-2">
                    <span className="truncate text-sm font-medium text-foreground" title={entry.display_name || entry.skill}>
                      {entry.display_name || entry.skill}
                    </span>
                    <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
                      {intl.formatMessage(
                        { id: 'skills.leaderboard.minutesSaved' },
                        { minutes: entry.estimated_minutes_saved },
                      )}
                    </span>
                  </div>
                  <div className="mt-1.5 h-2 overflow-hidden rounded-full bg-muted">
                    <div className="h-full rounded-full bg-chart-1" style={{ width: `${pct}%` }} />
                  </div>
                  {entry.owner && (
                    <div className="mt-1.5 flex items-center gap-1.5 text-xs text-muted-foreground">
                      <ActorAvatar actorType="agent" size="xs" name={entry.owner} />
                      <span className="truncate">{entry.owner}</span>
                      {entry.scope && <Badge variant="secondary">{entry.scope}</Badge>}
                    </div>
                  )}
                </div>
              </div>
            );
          })}
        </CardContent>
      </Card>
      {note && <p className="text-xs text-muted-foreground">{note}</p>}
    </div>
  );
}

// ── Market Tab ──────────────────────────────────────────────

function MarketTab({
  query,
  onCategory,
  results,
  loading,
  searched,
  onInstall,
}: {
  query: string;
  onCategory: (cat: string) => void;
  results: SkillIndexEntry[];
  loading: boolean;
  searched: boolean;
  onInstall: (skill: SkillIndexEntry) => void;
}) {
  const intl = useIntl();

  if (loading) return <CollectionPageState state="loading" />;

  if (searched && results.length === 0) {
    return (
      <CollectionPageState
        state="empty"
        icon={Search}
        title={intl.formatMessage({ id: 'skills.market.noResults' })}
        description={query ? `"${query}"` : undefined}
      />
    );
  }

  if (results.length > 0) {
    return (
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {results.map((skill) => (
          <SkillCard key={skill.name} skill={skill} onInstall={() => onInstall(skill)} />
        ))}
      </div>
    );
  }

  // Not yet searched — category browse.
  return (
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'skills.market.subtitle' })}</p>
      <h2 className="text-xs font-medium text-muted-foreground">
        {intl.formatMessage({ id: 'skills.market.categories' })}
      </h2>
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        {['utility', 'communication', 'code', 'data', 'security', 'ai', 'media', 'automation'].map((cat) => (
          <button
            key={cat}
            type="button"
            onClick={() => onCategory(cat)}
            className="flex items-center gap-2 rounded-xl border border-surface-border bg-surface px-4 py-3 text-sm text-foreground shadow-[var(--surface-shadow)] transition-colors hover:bg-surface-hover"
          >
            <Tag className="size-4 text-brand" />
            {cat}
          </button>
        ))}
      </div>
    </div>
  );
}

function SkillCard({ skill, onInstall }: { skill: SkillIndexEntry; onInstall: () => void }) {
  const intl = useIntl();
  const hasUrl = skill.url && /^https?:\/\//i.test(skill.url);
  return (
    <Card className="gap-3">
      <CardContent className="flex min-w-0 flex-1 flex-col gap-3">
        <div className="flex items-start justify-between gap-2">
          <h3 className="min-w-0 truncate text-base font-medium text-foreground" title={skill.name}>
            {skill.name}
          </h3>
          {hasUrl && (
            <a
              href={skill.url}
              target="_blank"
              rel="noopener noreferrer"
              className="shrink-0 text-muted-foreground hover:text-brand"
              aria-label="GitHub"
            >
              <ExternalLink className="size-4" />
            </a>
          )}
        </div>
        <p className="line-clamp-2 text-sm text-muted-foreground">
          {skill.description || intl.formatMessage({ id: 'common.noData' })}
        </p>
        {skill.tags.length > 0 && (
          <div className="flex flex-wrap gap-1.5">
            {skill.tags.map((tag) => (
              <Badge key={tag} variant="secondary">{tag}</Badge>
            ))}
          </div>
        )}
      </CardContent>
      <div className="flex items-center justify-between gap-2 border-t border-surface-border px-4 pt-3">
        {skill.author ? (
          <span className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
            <ActorAvatar actorType="user" size="xs" name={skill.author} />
            <span className="truncate">{skill.author}</span>
          </span>
        ) : (
          <span />
        )}
        <Button variant="brand" size="sm" onClick={onInstall}>
          <Download />
          {intl.formatMessage({ id: 'skills.market.install' })}
        </Button>
      </div>
    </Card>
  );
}

// ── Import from GitHub / URL ────────────────────────────────
// Collects a source URL, then reuses InstallDialog — the vet RPC fetches the
// content server-side (SSRF-gated), scans it, and install re-scans fail-closed.

function UrlImportDialog({
  onClose,
  onInstall,
}: {
  onClose: () => void;
  onInstall: (skill: SkillIndexEntry) => void;
}) {
  const intl = useIntl();
  const [url, setUrl] = useState('');

  const trimmed = url.trim();
  const valid = /^https?:\/\/\S+$/i.test(trimmed);

  const handleContinue = () => {
    if (!valid) return;
    let name = trimmed;
    try {
      const u = new URL(trimmed);
      name = `${u.hostname}${u.pathname}`.replace(/\/+$/, '');
    } catch {
      /* keep raw url as the display name */
    }
    onClose();
    onInstall({ name, description: trimmed, tags: [], author: '', url: trimmed, compatible: [] });
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'skills.import.title' })}</DialogTitle>
          <DialogDescription>{intl.formatMessage({ id: 'skills.import.desc' })}</DialogDescription>
        </DialogHeader>
        <div className="space-y-2">
          <label className="text-xs font-medium text-muted-foreground">URL</label>
          <Input
            type="url"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') handleContinue();
            }}
            placeholder="https://github.com/user/repo"
            autoFocus
          />
          <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'skills.import.hint' })}</p>
        </div>
        <DialogFooter>
          <DialogClose
            render={<Button variant="outline">{intl.formatMessage({ id: 'common.cancel' })}</Button>}
          />
          <Button variant="brand" onClick={handleContinue} disabled={!valid}>
            <Shield />
            {intl.formatMessage({ id: 'skills.import.continue' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// ── Shared (Team) Skills Tab (Phase 4) ──────────────────────

function SharedSkillsTab() {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  const [sharedSkills, setSharedSkills] = useState<SharedSkillInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [adoptTarget, setAdoptTarget] = useState<SharedSkillInfo | null>(null);
  const [adoptAgent, setAdoptAgent] = useState('');
  const [adoptSuccess, setAdoptSuccess] = useState<string | null>(null);

  useEffect(() => {
    fetchAgents();
    (async () => {
      setLoading(true);
      try {
        const result = await api.sharedSkills.list();
        setSharedSkills(result?.skills ?? []);
      } catch (e) {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
        setSharedSkills([]);
      } finally {
        setLoading(false);
      }
    })();
  }, [fetchAgents, intl]);

  const adoptTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (adoptTimerRef.current) clearTimeout(adoptTimerRef.current);
    },
    [],
  );

  const handleAdopt = useCallback(async () => {
    if (!adoptTarget || !adoptAgent) return;
    try {
      await api.sharedSkills.adopt(adoptTarget.name, adoptAgent);
      setAdoptSuccess(intl.formatMessage({ id: 'skills.shared.adoptSuccess' }, { agent: adoptAgent }));
      setSharedSkills((prev) =>
        prev.map((s) =>
          s.name === adoptTarget.name
            ? { ...s, adopted_by: [...s.adopted_by, adoptAgent], usage_count: s.usage_count + 1 }
            : s,
        ),
      );
      adoptTimerRef.current = setTimeout(() => {
        setAdoptTarget(null);
        setAdoptSuccess(null);
      }, 1500);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  }, [adoptTarget, adoptAgent, intl]);

  if (loading) return <CollectionPageState state="loading" />;
  if (sharedSkills.length === 0) {
    return (
      <CollectionPageState
        state="empty"
        icon={Share2}
        title={intl.formatMessage({ id: 'skills.shared.empty' })}
        description={intl.formatMessage({ id: 'skills.shared.subtitle' })}
      />
    );
  }

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'skills.shared.subtitle' })}</p>
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {sharedSkills.map((skill) => (
          <Card key={skill.name} className="gap-3">
            <CardContent className="flex flex-1 flex-col gap-3">
              <h3 className="truncate text-base font-medium text-foreground" title={skill.name}>
                {skill.name}
              </h3>
              <p className="line-clamp-2 text-sm text-muted-foreground">{skill.description || '—'}</p>
              {skill.tags.length > 0 && (
                <div className="flex flex-wrap gap-1.5">
                  {skill.tags.map((tag) => (
                    <Badge key={tag} variant="secondary">{tag}</Badge>
                  ))}
                </div>
              )}
              <div className="mt-auto space-y-1.5 text-xs text-muted-foreground">
                <div className="flex items-center gap-1.5">
                  <ActorAvatar actorType="agent" size="xs" name={skill.shared_by} />
                  <span className="truncate">
                    {intl.formatMessage({ id: 'skills.shared.sharedBy' })}: {skill.shared_by}
                  </span>
                </div>
                <div className="flex items-center gap-1.5">
                  <Download className="size-3" />
                  <span>
                    {intl.formatMessage({ id: 'skills.shared.usageCount' })}:{' '}
                    <span className="font-mono tabular-nums">{skill.usage_count}</span>
                  </span>
                </div>
              </div>
            </CardContent>
            <div className="border-t border-surface-border px-4 pt-3">
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  setAdoptTarget(skill);
                  setAdoptAgent(agents[0]?.name ?? '');
                  setAdoptSuccess(null);
                }}
              >
                <Download />
                {intl.formatMessage({ id: 'skills.shared.adopt' })}
              </Button>
            </div>
          </Card>
        ))}
      </div>

      {/* Adopt dialog */}
      <Dialog
        open={adoptTarget !== null}
        onOpenChange={(o) => {
          if (!o) {
            setAdoptTarget(null);
            setAdoptSuccess(null);
          }
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{intl.formatMessage({ id: 'skills.shared.adopt' })}</DialogTitle>
            <DialogDescription>
              <strong className="text-foreground">{adoptTarget?.name}</strong> →{' '}
              {intl.formatMessage({ id: 'skills.shared.adoptTo' })}
            </DialogDescription>
          </DialogHeader>
          <AgentSelect
            value={adoptAgent}
            onValueChange={setAdoptAgent}
            agents={agents}
            placeholder={intl.formatMessage({ id: 'skills.shared.adoptTo' })}
          />
          {adoptSuccess && (
            <div className="flex items-center gap-2 rounded-lg bg-success/10 px-3 py-2 text-sm text-success">
              <CheckCircle className="size-4 shrink-0" />
              {adoptSuccess}
            </div>
          )}
          <DialogFooter>
            <DialogClose
              render={<Button variant="outline">{intl.formatMessage({ id: 'common.cancel' })}</Button>}
            />
            <Button variant="brand" onClick={handleAdopt} disabled={!adoptAgent || !!adoptSuccess}>
              {intl.formatMessage({ id: 'skills.shared.adopt' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

// ── My Skills Tab ───────────────────────────────────────────

/** Security status → semantic dot + Badge tone (installed-skill data model). */
function securityDot(status?: string): string {
  if (status === 'pass') return 'bg-success';
  if (status === 'warn') return 'bg-warning';
  if (status === 'fail') return 'bg-destructive';
  return 'bg-muted-foreground';
}

const MY_SKILLS_COLUMNS = 'minmax(0,1fr) auto 2.5rem';

function MySkillsTab({ filter }: { filter: string }) {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  const [selectedAgent, setSelectedAgent] = useState<string>('');
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const [shareSuccess, setShareSuccess] = useState<string | null>(null);

  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);
  useEffect(() => {
    if (agents.length > 0 && !selectedAgent) setSelectedAgent(agents[0].name);
  }, [agents, selectedAgent]);

  useEffect(() => {
    if (!selectedAgent) return;
    setLoading(true);
    (async () => {
      try {
        const result = await api.skills.list(selectedAgent);
        setSkills(result?.skills ?? []);
      } catch (e) {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
        setSkills([]);
      } finally {
        setLoading(false);
      }
    })();
  }, [selectedAgent, intl]);

  const handleShare = useCallback(
    async (skillName: string) => {
      if (!selectedAgent) return;
      try {
        await api.sharedSkills.share(selectedAgent, skillName);
        setShareSuccess(skillName);
        setTimeout(() => setShareSuccess(null), 2000);
      } catch (e) {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      }
    },
    [selectedAgent, intl],
  );

  const q = filter.trim().toLowerCase();
  const visible = useMemo(
    () => (q ? skills.filter((s) => s.name.toLowerCase().includes(q)) : skills),
    [skills, q],
  );

  return (
    <div className="space-y-6">
      {/* Self-built skills (V13 / T13.2), filtered by the same header search. */}
      <CustomSkillsSection filter={filter} />

      {/* Installed skills per agent (spec §4 ListGrid, single-row h-12). */}
      <section className="space-y-3">
        <div className="flex items-center justify-between gap-2">
          <h2 className="text-sm font-medium text-foreground">
            {intl.formatMessage({ id: 'skills.my.installed' })}
          </h2>
          <AgentSelect
            value={selectedAgent}
            onValueChange={setSelectedAgent}
            agents={agents}
            placeholder={intl.formatMessage({ id: 'skills.my.selectAgent' })}
          />
        </div>

        {loading ? (
          <CollectionPageState state="loading" />
        ) : visible.length === 0 ? (
          <CollectionPageState
            state="empty"
            icon={Sparkles}
            title={intl.formatMessage({ id: q ? 'skills.market.noResults' : 'common.noData' })}
          />
        ) : (
          <div className="overflow-hidden rounded-xl border border-surface-border">
            <ListGridContainer
              columns={MY_SKILLS_COLUMNS}
              className="!h-auto [&>[aria-hidden]]:hidden"
              header={
                <ListGridHeader>
                  <ListGridHeaderCell>{intl.formatMessage({ id: 'skills.my.col.name' })}</ListGridHeaderCell>
                  <ListGridHeaderCell>{intl.formatMessage({ id: 'skills.my.col.security' })}</ListGridHeaderCell>
                  <ListGridHeaderCell aria-hidden />
                </ListGridHeader>
              }
            >
              {visible.map((skill) => (
                <InstalledSkillRow
                  key={skill.name}
                  skill={skill}
                  shared={shareSuccess === skill.name}
                  onShare={() => handleShare(skill.name)}
                />
              ))}
            </ListGridContainer>
          </div>
        )}
      </section>
    </div>
  );
}

function InstalledSkillRow({
  skill,
  shared,
  onShare,
}: {
  skill: SkillInfo;
  shared: boolean;
  onShare: () => void;
}) {
  const intl = useIntl();
  const status = skill.security_status;
  return (
    <ListGridRow className="cursor-default">
      <ListGridCell className="gap-2">
        <Sparkles className="size-4 shrink-0 text-muted-foreground" />
        <span className="truncate text-sm font-medium text-foreground" title={skill.name}>
          {skill.name}
        </span>
      </ListGridCell>
      <ListGridCell>
        <span className={cn('mr-2 size-1.5 shrink-0 rounded-full', securityDot(status))} />
        <span className="truncate text-sm text-muted-foreground">
          {status
            ? intl.formatMessage({ id: `skills.my.security.${status}` })
            : intl.formatMessage({ id: 'skills.my.security.unknown' })}
        </span>
      </ListGridCell>
      <ListGridCell className="justify-end">
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={intl.formatMessage({ id: 'skills.my.moreActions' })}
                data-stop-row-nav
                onClick={(e) => e.stopPropagation()}
              />
            }
          >
            <MoreHorizontal />
          </DropdownMenuTrigger>
          <DropdownMenuContent>
            <DropdownMenuItem onClick={onShare} disabled={shared}>
              {shared ? <CheckCircle /> : <Share2 />}
              {shared
                ? intl.formatMessage({ id: 'skills.my.shared' })
                : intl.formatMessage({ id: 'skills.shared.share' })}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </ListGridCell>
    </ListGridRow>
  );
}

/** Small agent picker shared by the install/adopt/my-skills surfaces. */
function AgentSelect({
  value,
  onValueChange,
  agents,
  placeholder,
}: {
  value: string;
  onValueChange: (v: string) => void;
  agents: ReadonlyArray<{ name: string; display_name: string; icon?: string }>;
  placeholder?: string;
}) {
  const current = agents.find((a) => a.name === value);
  return (
    <Select value={value} onValueChange={(v) => onValueChange(String(v))}>
      <SelectTrigger className="w-52">
        <SelectValue placeholder={placeholder}>
          {current ? `${glyphText(current.icon)} ${current.display_name}` : placeholder}
        </SelectValue>
      </SelectTrigger>
      <SelectContent>
        {agents.map((a) => (
          <SelectItem key={a.name} value={a.name}>
            {glyphText(a.icon) + ' ' + a.display_name}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

// ── Install dialog + vet findings ───────────────────────────

/** Finding severity → MDS Badge styling (spec §5.4: low=success/medium=warning/
 *  high & critical=destructive). */
function severityBadgeClass(sev: string): { variant: 'secondary' | 'destructive'; className?: string } {
  switch (sev) {
    case 'info':
      return { variant: 'secondary' };
    case 'low':
      return { variant: 'secondary', className: 'bg-success/15 text-success' };
    case 'warning':
    case 'medium':
      return { variant: 'secondary', className: 'bg-warning/15 text-warning' };
    case 'high':
    case 'critical':
    default:
      return { variant: 'destructive' };
  }
}

function InstallDialog({ skill, onClose }: { skill: SkillIndexEntry; onClose: () => void }) {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  const [scope, setScope] = useState('global');
  const [scanning, setScanning] = useState(false);
  const [scanResult, setScanResult] = useState<VetResponse | null>(null);
  const [installing, setInstalling] = useState(false);
  const [installed, setInstalled] = useState(false);
  const [requested, setRequested] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);

  const handleScan = useCallback(async () => {
    if (!skill.url) return;
    setScanning(true);
    setError(null);
    setScanResult(null);
    try {
      const result = await api.skills.vet(skill.url);
      setScanResult(result);
    } catch (e) {
      setError(String(e));
    } finally {
      setScanning(false);
    }
  }, [skill.url]);

  const handleInstall = useCallback(async () => {
    if (!scanResult?.passed || !skill.url) return;
    setInstalling(true);
    setError(null);
    try {
      if (isAdmin) {
        await api.skills.install(skill.url, scope, scanResult.content);
        setInstalled(true);
      } else {
        // Non-admin: file an approval request (manager → admin chain).
        const res = await api.skills.installRequest(skill.url, scope, scanResult.content);
        setRequested(res.stage);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setInstalling(false);
    }
  }, [scanResult, skill.url, scope, isAdmin]);

  const scanPassed = scanResult?.passed === true;
  const scanFailed = scanResult !== null && !scanResult.passed;

  // WP7 — departments already in use, for the `department:<dept>` scope option.
  // Hidden in the Personal edition (no departments there).
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  const departmentOptions: string[] = isPersonal ? [] : departmentsOf(agents);

  const scopeLabel = (s: string): string => {
    if (s === 'global') return intl.formatMessage({ id: 'skills.install.scopeGlobal' });
    if (s.startsWith('department:')) {
      return intl.formatMessage({ id: 'skills.install.scopeDept' }, { dept: s.slice('department:'.length) });
    }
    const agent = agents.find((a) => a.name === s);
    return intl.formatMessage({ id: 'skills.install.scopeAgent' }, { agent: agent?.display_name || s });
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'skills.install.title' })}</DialogTitle>
          <DialogDescription className="truncate">
            <span className="font-medium text-foreground">{skill.name}</span>
            {skill.description ? ` — ${skill.description}` : ''}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          {/* Scope selector — company / department / individual (WP7). */}
          <div className="space-y-1.5">
            <label className="text-xs font-medium text-muted-foreground">
              {intl.formatMessage({ id: 'skills.install.scope' })}
            </label>
            <Select value={scope} onValueChange={(v) => setScope(String(v))}>
              <SelectTrigger className="w-full">
                <SelectValue>{scopeLabel(scope)}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="global">
                  {intl.formatMessage({ id: 'skills.install.scopeGlobal' })}
                </SelectItem>
                {departmentOptions.length > 0 && (
                  <SelectGroup>
                    <SelectLabel>{intl.formatMessage({ id: 'skills.install.scopeDeptGroup' })}</SelectLabel>
                    {departmentOptions.map((dept) => (
                      <SelectItem key={dept} value={`department:${dept}`}>
                        {intl.formatMessage({ id: 'skills.install.scopeDept' }, { dept })}
                      </SelectItem>
                    ))}
                  </SelectGroup>
                )}
                <SelectGroup>
                  <SelectLabel>{intl.formatMessage({ id: 'skills.install.scopeAgentGroup' })}</SelectLabel>
                  {agents.map((agent) => (
                    <SelectItem key={agent.name} value={agent.name}>
                      {intl.formatMessage(
                        { id: 'skills.install.scopeAgent' },
                        { agent: agent.display_name || agent.name },
                      )}
                    </SelectItem>
                  ))}
                </SelectGroup>
              </SelectContent>
            </Select>
          </div>

          {/* Security scan */}
          <Button variant="outline" onClick={handleScan} disabled={scanning || !skill.url}>
            {scanning ? <Loader2 className="animate-spin" /> : <Shield />}
            {scanning
              ? intl.formatMessage({ id: 'skills.install.scanning' })
              : intl.formatMessage({ id: 'skills.install.scan' })}
          </Button>

          {/* Scan results */}
          {scanPassed && scanResult && (
            <div className="space-y-3">
              <div className="flex items-center gap-2 text-success">
                <ShieldCheck className="size-5" />
                <span className="text-sm font-medium">
                  {intl.formatMessage({ id: 'skills.install.scanPassed' })}
                </span>
                <span className="ml-auto font-mono text-xs tabular-nums text-muted-foreground">
                  {intl.formatMessage({ id: 'skills.install.score' }, { score: scanResult.vet_result.score })}
                </span>
              </div>
              {scanResult.vet_result.findings.length > 0 ? (
                <FindingsList findings={scanResult.vet_result.findings} />
              ) : (
                <p className="text-sm text-muted-foreground">
                  {intl.formatMessage({ id: 'skills.install.noFindings' })}
                </p>
              )}
            </div>
          )}

          {scanFailed && scanResult && (
            <div className="space-y-3">
              <div className="flex items-center gap-2 text-destructive">
                <ShieldAlert className="size-5" />
                <span className="text-sm font-medium">
                  {intl.formatMessage({ id: 'skills.install.scanFailed' })}
                </span>
                <span className="ml-auto font-mono text-xs tabular-nums text-muted-foreground">
                  {intl.formatMessage({ id: 'skills.install.score' }, { score: scanResult.vet_result.score })}
                </span>
              </div>
              {scanResult.vet_result.findings.length > 0 && (
                <FindingsList findings={scanResult.vet_result.findings} />
              )}
            </div>
          )}

          {error && (
            <div className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</div>
          )}

          {!isAdmin && scanPassed && !requested && (
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'install.request.nonAdminNotice' })}
            </p>
          )}

          {installed && scanResult && (
            <div className="flex items-center gap-2 rounded-lg bg-success/10 px-3 py-2 text-sm text-success">
              <CheckCircle className="size-4 shrink-0" />
              {intl.formatMessage(
                { id: 'skills.install.success' },
                { name: scanResult.skill_name || skill.name },
              )}
            </div>
          )}

          {requested && (
            <div className="flex items-center gap-2 rounded-lg bg-warning/10 px-3 py-2 text-sm text-warning">
              <CheckCircle className="size-4 shrink-0" />
              {intl.formatMessage({
                id: requested === 'awaiting_manager' ? 'install.request.filedManager' : 'install.request.filedAdmin',
              })}
            </div>
          )}
        </div>

        <DialogFooter>
          <DialogClose
            render={
              <Button variant="outline">
                {intl.formatMessage({ id: requested || installed ? 'common.close' : 'common.cancel' })}
              </Button>
            }
          />
          {!installed && !requested && (
            <Button
              variant="brand"
              onClick={handleInstall}
              disabled={!scanPassed || installing}
              title={!scanPassed ? intl.formatMessage({ id: 'skills.install.requireScan' }) : undefined}
            >
              {installing ? <Loader2 className="animate-spin" /> : <Download />}
              {installing
                ? intl.formatMessage({ id: isAdmin ? 'skills.install.installing' : 'install.request.submitting' })
                : intl.formatMessage({ id: isAdmin ? 'skills.install.installBtn' : 'install.request.submit' })}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function FindingsList({
  findings,
}: {
  findings: ReadonlyArray<{ severity: string; category: string; description: string }>;
}) {
  const intl = useIntl();
  return (
    <div className="space-y-2">
      <p className="text-xs font-medium text-muted-foreground">
        {intl.formatMessage({ id: 'skills.install.findings' })}
      </p>
      <ul className="space-y-1.5">
        {findings.map((f, i) => {
          const sev = f.severity.toLowerCase();
          const badge = severityBadgeClass(sev);
          return (
            <li
              key={i}
              className="flex items-start gap-2 rounded-lg border border-surface-border bg-surface p-2.5 text-sm"
            >
              <AlertTriangle className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
              <div className="min-w-0 space-y-1">
                <div className="flex items-center gap-1.5">
                  <Badge variant={badge.variant} className={badge.className}>
                    {intl.formatMessage({ id: `skills.install.severity.${sev}` })}
                  </Badge>
                  <span className="text-xs text-muted-foreground">{f.category}</span>
                </div>
                <p className="text-sm text-foreground">{f.description}</p>
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
