import { useState, useEffect, useCallback, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useSearchParams } from 'react-router';
import { isImeComposing } from '@/lib/keyboard';
import { api, type SkillIndexEntry, type SkillLeaderboardEntry } from '@/lib/api';
import { parseSkillTab, sortAgentsByName, type SkillTab } from '@/lib/skill-library';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { departmentsOf } from '@/lib/agents';
import { MySkillsTab } from '@/components/skills/MySkillsTab';
import { SharedSkillsTab } from '@/components/skills/SharedSkillsTab';
import { AutonomyNote } from '@/components/AutonomyNote';
import { toast, formatError } from '@/lib/toast';
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
  ActorAvatar,
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
  Sparkles,
  Trophy,
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
  // B8: the open tab lives in the URL (`?tab=`) so a link or a reload lands
  // on the same section.
  const [searchParams, setSearchParams] = useSearchParams();
  const activeTab = parseSkillTab(searchParams.get('tab'));
  const setActiveTab = useCallback(
    (tab: SkillTab) => {
      const next = new URLSearchParams(searchParams);
      if (tab === 'market') next.delete('tab');
      else next.set('tab', tab);
      setSearchParams(next, { replace: true });
    },
    [searchParams, setSearchParams],
  );

  // Market search is lifted to the page so its Input can live in the header
  // control row (spec §5.2) alongside the section switcher.
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<SkillIndexEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [searched, setSearched] = useState(false);
  // How many entries the server-side market index holds. Zero after a search
  // means the GitHub index never loaded (offline / rate-limited) rather than
  // "nothing matched" — the refresh failure is swallowed server-side.
  const [indexed, setIndexed] = useState<number | null>(null);
  const [installSkill, setInstallSkill] = useState<SkillIndexEntry | null>(null);
  const [showUrlImport, setShowUrlImport] = useState(false);

  // Team / mine client-side filter (name, description, tags, owner) — shares
  // the header Input; each tab keeps its own text.
  const [mineFilter, setMineFilter] = useState('');
  const [sharedFilter, setSharedFilter] = useState('');

  const runSearch = useCallback(
    async (q: string) => {
      if (!q.trim()) return;
      setLoading(true);
      setSearched(true);
      try {
        const res = await api.skillMarket.search(q);
        setResults(res?.skills ?? []);
        setIndexed(res?.total_indexed ?? null);
      } catch (e) {
        console.warn('[api]', e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
        setResults([]);
        setIndexed(null);
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

  const showSearch = activeTab !== 'leaderboard';
  const filterValue = activeTab === 'market' ? query : activeTab === 'shared' ? sharedFilter : mineFilter;
  const setFilterValue = (v: string) => {
    if (activeTab === 'market') setQuery(v);
    else if (activeTab === 'shared') setSharedFilter(v);
    else setMineFilter(v);
  };

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
              value={filterValue}
              onChange={(e) => setFilterValue(e.target.value)}
              onKeyDown={(e) => {
                if (activeTab === 'market' && e.key === 'Enter' && !isImeComposing(e)) runSearch(query);
              }}
              placeholder={intl.formatMessage({
                id:
                  activeTab === 'market'
                    ? 'skills.market.searchPlaceholder'
                    : activeTab === 'shared'
                      ? 'skills.shared.filterPlaceholder'
                      : 'skills.my.filterPlaceholder',
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
            indexed={indexed}
            onInstall={setInstallSkill}
          />
        )}
        {activeTab === 'shared' && <SharedSkillsTab filter={sharedFilter} />}
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
  indexed,
  onInstall,
}: {
  query: string;
  onCategory: (cat: string) => void;
  results: SkillIndexEntry[];
  loading: boolean;
  searched: boolean;
  indexed: number | null;
  onInstall: (skill: SkillIndexEntry) => void;
}) {
  const intl = useIntl();

  if (loading) return <CollectionPageState state="loading" />;

  if (searched && results.length === 0) {
    // The gateway refreshes the market index from GitHub on a best-effort
    // basis and drops the error on the floor. Offline or rate-limited, the
    // index stays at zero entries and every search "finds nothing" — which
    // reads as an empty market rather than a failed fetch.
    const indexUnavailable = indexed === 0;
    return (
      <CollectionPageState
        state={indexUnavailable ? 'error' : 'empty'}
        icon={Search}
        title={intl.formatMessage({
          id: indexUnavailable ? 'skills.market.indexEmpty' : 'skills.market.noResults',
        })}
        description={!indexUnavailable && query ? `"${query}"` : undefined}
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
              if (e.key === 'Enter' && !isImeComposing(e)) handleContinue();
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
                  {sortAgentsByName(agents, intl.locale).map((agent) => (
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

          <AutonomyNote id="skillsInstall" />

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
