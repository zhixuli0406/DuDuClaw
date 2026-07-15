import { useState, useEffect, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { cn } from '@/lib/utils';
import { api, type SkillIndexEntry, type SharedSkillInfo, type SkillInfo, type SkillLeaderboardEntry } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { departmentsOf } from '@/lib/agents';
import { Dialog } from '@/components/shared/Dialog';
import { CustomSkillsSection } from '@/components/skills/CustomSkillsSection';
import { toast, formatError } from '@/lib/toast';
import {
  Page,
  PageHeader,
  Card,
  Section,
  Tabs,
  Toolbar,
  Button,
  Badge,
  EmptyState,
  Field,
  CharacterAvatar,
  Mono,
  controlClass,
  type TabItem,
} from '@/components/ui';
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
  Users,
  Sparkles,
  Store,
  Puzzle,
  Trophy,
  Clock,
  Wand2,
  Link as LinkIcon,
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

export function SkillMarketPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const [activeTab, setActiveTab] = useState<SkillTab>('market');

  const tabItems: TabItem[] = [
    { id: 'market', label: intl.formatMessage({ id: 'skills.tab.market' }), icon: Store },
    { id: 'shared', label: intl.formatMessage({ id: 'skills.tab.shared' }), icon: Users },
    { id: 'mySkills', label: intl.formatMessage({ id: 'skills.tab.mySkills' }), icon: Sparkles },
    { id: 'leaderboard', label: intl.formatMessage({ id: 'skills.tab.leaderboard' }), icon: Trophy },
  ];

  return (
    <Page wide>
      <PageHeader
        icon={Puzzle}
        title={intl.formatMessage({ id: 'nav.skills' })}
        subtitle={intl.formatMessage({ id: 'skills.market.title' })}
        actions={
          // Primary CTA: build a skill — coexists with the "install from market"
          // flow below on one converged /skills page (WP1). `/skills/new` route
          // is preserved for deep-link compatibility.
          <Button variant="primary" icon={Wand2} onClick={() => navigate('/skills/new')}>
            {intl.formatMessage({ id: 'skills.new.title' })}
          </Button>
        }
      />

      <Tabs items={tabItems} value={activeTab} onChange={(id) => setActiveTab(id as SkillTab)} />

      {activeTab === 'market' && <MarketTab />}
      {activeTab === 'shared' && <SharedSkillsTab />}
      {activeTab === 'mySkills' && <MySkillsTab />}
      {activeTab === 'leaderboard' && <LeaderboardTab />}
    </Page>
  );
}

// ── Leaderboard Tab (WP10-T10.1 — honour roll of time-saving skills) ─────

const RANK_ACCENT: Record<number, string> = {
  0: 'text-amber-500',
  1: 'text-stone-400',
  2: 'text-orange-400',
};

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

  return (
    <div className="space-y-6">
      <p className="text-sm text-stone-500 dark:text-stone-400">
        {intl.formatMessage({ id: 'skills.leaderboard.subtitle' })}
      </p>

      {loading ? (
        <div className="py-12 text-center text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</div>
      ) : entries.length === 0 ? (
        <Card>
          <EmptyState
            dudu="curious"
            icon={Trophy}
            title={intl.formatMessage({ id: 'skills.leaderboard.empty' })}
          />
        </Card>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {entries.map((entry, i) => (
            <Card key={`${entry.owner}/${entry.skill}`}>
              <div className="flex items-start gap-3">
                <span
                  className={cn(
                    'flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-stone-500/10 text-sm font-bold tabular-nums dark:bg-white/5',
                    RANK_ACCENT[i] ?? 'text-stone-500 dark:text-stone-400',
                  )}
                  aria-hidden="true"
                >
                  {i < 3 ? <Trophy className="h-4 w-4" /> : i + 1}
                </span>
                <div className="min-w-0 flex-1">
                  <h3 className="truncate font-semibold text-stone-900 dark:text-stone-50" title={entry.display_name || entry.skill}>
                    {entry.display_name || entry.skill}
                  </h3>
                  <p className="mt-1 flex items-center gap-1.5 text-sm font-medium text-amber-600 dark:text-amber-400">
                    <Clock className="h-3.5 w-3.5" />
                    {intl.formatMessage(
                      { id: 'skills.leaderboard.minutesSaved' },
                      { minutes: entry.estimated_minutes_saved },
                    )}
                  </p>
                </div>
              </div>

              <div className="mt-4 flex flex-wrap items-center gap-1.5 border-t border-[var(--panel-border)] pt-3 text-xs text-stone-500 dark:text-stone-400">
                {entry.owner && (
                  <span className="flex items-center gap-1">
                    <CharacterAvatar agentId={entry.owner} name={entry.owner} size={24} />
                    {entry.owner}
                  </span>
                )}
                {entry.scope && <Badge tone="neutral">{entry.scope}</Badge>}
              </div>
            </Card>
          ))}
        </div>
      )}

      {note && (
        <p className="text-xs text-stone-400 dark:text-stone-500">{note}</p>
      )}
    </div>
  );
}

// ── Market Tab (original content) ───────────────────────────

function MarketTab() {
  const intl = useIntl();
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<SkillIndexEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [searched, setSearched] = useState(false);
  const [installSkill, setInstallSkill] = useState<SkillIndexEntry | null>(null);
  const [showUrlImport, setShowUrlImport] = useState(false);

  const handleSearchQuery = async (q: string) => {
    if (!q.trim()) return;
    setLoading(true);
    setSearched(true);
    try {
      const res = await api.skillMarket.search(q);
      setResults(res?.skills ?? []);
    } catch (e) {
      console.warn('[api]', e);
      // Surface to the user so they don't mistake a network/backend error for
      // "no results." The empty-results reset still drives the empty-state UI.
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setResults([]);
    } finally {
      setLoading(false);
    }
  };

  const handleSearch = () => handleSearchQuery(query);

  return (
    <div className="space-y-6">
      <p className="text-sm text-stone-500 dark:text-stone-400">
        {intl.formatMessage({ id: 'skills.market.subtitle' })}
      </p>

      {/* Search bar */}
      <Toolbar
        search={query}
        onSearchChange={setQuery}
        onSearchEnter={handleSearch}
        searchPlaceholder={intl.formatMessage({ id: 'skills.market.searchPlaceholder' })}
      >
        <Button variant="primary" icon={Search} onClick={handleSearch} disabled={loading}>
          {intl.formatMessage({ id: 'skills.market.search' })}
        </Button>
        <Button variant="secondary" icon={LinkIcon} onClick={() => setShowUrlImport(true)}>
          {intl.formatMessage({ id: 'skills.import.fromUrl' })}
        </Button>
      </Toolbar>

      {loading && (
        <div className="py-12 text-center text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      )}

      {!loading && searched && results.length === 0 && (
        <Card>
          <EmptyState
            dudu="concerned"
            icon={Search}
            title={intl.formatMessage({ id: 'skills.market.noResults' })}
          />
        </Card>
      )}

      {!loading && results.length > 0 && (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {results.map((skill) => (
            <SkillCard key={skill.name} skill={skill} onInstall={() => setInstallSkill(skill)} />
          ))}
        </div>
      )}

      {!searched && (
        <Section title={intl.formatMessage({ id: 'skills.market.categories' })}>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            {['utility', 'communication', 'code', 'data', 'security', 'ai', 'media', 'automation'].map((cat) => (
              <Card
                key={cat}
                interactive
                padded={false}
                onClick={() => { setQuery(cat); handleSearchQuery(cat); }}
              >
                <span className="flex items-center gap-2 px-4 py-3 text-sm text-stone-700 dark:text-stone-300">
                  <Tag className="h-4 w-4 text-amber-500" />
                  {cat}
                </span>
              </Card>
            ))}
          </div>
        </Section>
      )}

      {installSkill && <InstallDialog skill={installSkill} onClose={() => setInstallSkill(null)} />}
      {showUrlImport && <UrlImportDialog onClose={() => setShowUrlImport(false)} />}
    </div>
  );
}

// ── Import from GitHub / URL ────────────────────────────────
//
// Collects a source URL, then reuses InstallDialog — the vet RPC fetches the
// content server-side (SSRF-gated), scans it, and install re-scans fail-closed.

function UrlImportDialog({ onClose }: { onClose: () => void }) {
  const intl = useIntl();
  const [url, setUrl] = useState('');
  const [entry, setEntry] = useState<SkillIndexEntry | null>(null);

  const trimmed = url.trim();
  const valid = /^https?:\/\/\S+$/i.test(trimmed);

  const handleContinue = () => {
    if (!valid) return;
    let name = trimmed;
    try {
      const u = new URL(trimmed);
      name = `${u.hostname}${u.pathname}`.replace(/\/+$/, '');
    } catch { /* keep raw url as the display name */ }
    setEntry({
      name,
      description: trimmed,
      tags: [],
      author: '',
      url: trimmed,
      compatible: [],
    });
  };

  if (entry) {
    return <InstallDialog skill={entry} onClose={onClose} />;
  }

  return (
    <Dialog
      open
      onClose={onClose}
      title={intl.formatMessage({ id: 'skills.import.title' })}
      className="max-w-xl"
    >
      <div className="space-y-4">
        <p className="text-sm text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'skills.import.desc' })}
        </p>
        <Field label="URL">
          <input
            type="url"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') handleContinue(); }}
            placeholder="https://github.com/user/repo"
            className={controlClass}
            autoFocus
          />
        </Field>
        <p className="text-xs text-stone-400 dark:text-stone-500">
          {intl.formatMessage({ id: 'skills.import.hint' })}
        </p>
        <div className="flex justify-end gap-3 border-t border-[var(--panel-border)] pt-4">
          <Button variant="secondary" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="primary" icon={Shield} onClick={handleContinue} disabled={!valid}>
            {intl.formatMessage({ id: 'skills.import.continue' })}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

// ── Shared Skills Tab (Phase 4) ─────────────────────────────

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
  useEffect(() => () => { if (adoptTimerRef.current) clearTimeout(adoptTimerRef.current); }, []);

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
      adoptTimerRef.current = setTimeout(() => { setAdoptTarget(null); setAdoptSuccess(null); }, 1500);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  }, [adoptTarget, adoptAgent, intl]);

  return (
    <div className="space-y-6">
      <p className="text-sm text-stone-500 dark:text-stone-400">
        {intl.formatMessage({ id: 'skills.shared.subtitle' })}
      </p>

      {loading ? (
        <div className="py-12 text-center text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</div>
      ) : sharedSkills.length === 0 ? (
        <Card>
          <EmptyState
            dudu="curious"
            icon={Share2}
            title={intl.formatMessage({ id: 'skills.shared.empty' })}
          />
        </Card>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {sharedSkills.map((skill) => (
            <Card key={skill.name} interactive>
              <h3 className="font-semibold text-stone-900 dark:text-stone-50">{skill.name}</h3>
              <p className="mt-1 text-sm text-stone-600 dark:text-stone-400">
                {skill.description || '—'}
              </p>

              {skill.tags.length > 0 && (
                <div className="mt-3 flex flex-wrap gap-1.5">
                  {skill.tags.map((tag) => (
                    <Badge key={tag} tone="neutral">{tag}</Badge>
                  ))}
                </div>
              )}

              <div className="mt-4 space-y-2 text-xs text-stone-500 dark:text-stone-400">
                <div className="flex items-center gap-2">
                  <CharacterAvatar agentId={skill.shared_by} name={skill.shared_by} size={24} />
                  <span>{intl.formatMessage({ id: 'skills.shared.sharedBy' })}: {skill.shared_by}</span>
                </div>
                <div className="flex items-center gap-2">
                  <Download className="h-3 w-3" />
                  <span>{intl.formatMessage({ id: 'skills.shared.usageCount' })}: <Mono>{skill.usage_count}</Mono></span>
                </div>
                {skill.adopted_by.length > 0 && (
                  <div className="flex items-center gap-2">
                    <Users className="h-3 w-3" />
                    <span>{intl.formatMessage({ id: 'skills.shared.adoptedBy' })}: {skill.adopted_by.join(', ')}</span>
                  </div>
                )}
              </div>

              <div className="mt-4 border-t border-[var(--panel-border)] pt-3">
                <Button
                  size="sm"
                  icon={Download}
                  onClick={() => {
                    setAdoptTarget(skill);
                    setAdoptAgent(agents[0]?.name ?? '');
                    setAdoptSuccess(null);
                  }}
                >
                  {intl.formatMessage({ id: 'skills.shared.adopt' })}
                </Button>
              </div>
            </Card>
          ))}
        </div>
      )}

      {/* Adopt dialog */}
      {adoptTarget && (
        <Dialog
          open
          onClose={() => { setAdoptTarget(null); setAdoptSuccess(null); }}
          title={intl.formatMessage({ id: 'skills.shared.adopt' })}
        >
          <div className="space-y-4">
            <p className="text-sm text-stone-600 dark:text-stone-400">
              <strong>{adoptTarget.name}</strong> → {intl.formatMessage({ id: 'skills.shared.adoptTo' })}
            </p>
            <select
              value={adoptAgent}
              onChange={(e) => setAdoptAgent(e.target.value)}
              className={controlClass}
            >
              {agents.map((a) => (
                <option key={a.name} value={a.name}>{a.icon || '🤖'} {a.display_name}</option>
              ))}
            </select>
            {adoptSuccess && (
              <div className="flex items-center gap-2 rounded-control border border-emerald-200 bg-emerald-50 p-3 text-sm text-emerald-700 dark:border-emerald-800 dark:bg-emerald-900/20 dark:text-emerald-400">
                <CheckCircle className="h-4 w-4 flex-shrink-0" />
                {adoptSuccess}
              </div>
            )}
            <div className="flex justify-end gap-3">
              <Button
                variant="secondary"
                onClick={() => { setAdoptTarget(null); setAdoptSuccess(null); }}
              >
                {intl.formatMessage({ id: 'common.cancel' })}
              </Button>
              <Button
                variant="primary"
                onClick={handleAdopt}
                disabled={!adoptAgent || !!adoptSuccess}
              >
                {intl.formatMessage({ id: 'skills.shared.adopt' })}
              </Button>
            </div>
          </div>
        </Dialog>
      )}
    </div>
  );
}

// ── My Skills Tab ───────────────────────────────────────────

function MySkillsTab() {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  const [selectedAgent, setSelectedAgent] = useState<string>('');
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const [shareSuccess, setShareSuccess] = useState<string | null>(null);

  useEffect(() => { fetchAgents(); }, [fetchAgents]);
  useEffect(() => { if (agents.length > 0 && !selectedAgent) setSelectedAgent(agents[0].name); }, [agents, selectedAgent]);

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

  const handleShare = useCallback(async (skillName: string) => {
    if (!selectedAgent) return;
    try {
      await api.sharedSkills.share(selectedAgent, skillName);
      setShareSuccess(skillName);
      setTimeout(() => setShareSuccess(null), 2000);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  }, [selectedAgent, intl]);

  return (
    <div className="space-y-6">
      {/* Self-built skills (V13 / T13.2) */}
      <CustomSkillsSection />

      {/* Agent selector */}
      <Toolbar>
        <Field label="Agent" className="space-y-0">
          <select
            value={selectedAgent}
            onChange={(e) => setSelectedAgent(e.target.value)}
            className={cn(controlClass, 'w-auto')}
          >
            {agents.map((a) => (
              <option key={a.name} value={a.name}>{a.icon || '🤖'} {a.display_name}</option>
            ))}
          </select>
        </Field>
      </Toolbar>

      {loading ? (
        <div className="py-12 text-center text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</div>
      ) : skills.length === 0 ? (
        <Card>
          <EmptyState
            dudu="curious"
            icon={Sparkles}
            title={intl.formatMessage({ id: 'common.noData' })}
          />
        </Card>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {skills.map((skill) => (
            <Card key={skill.name}>
              <div className="flex items-start justify-between gap-2">
                <h3 className="font-semibold text-stone-900 dark:text-stone-50">{skill.name}</h3>
                {skill.security_status && (
                  <Badge
                    tone={
                      skill.security_status === 'pass' ? 'success' :
                      skill.security_status === 'warn' ? 'warning' :
                      'danger'
                    }
                  >
                    {skill.security_status}
                  </Badge>
                )}
              </div>

              <p className="mt-2 line-clamp-3 text-sm text-stone-600 dark:text-stone-400">
                {skill.content.slice(0, 150)}{skill.content.length > 150 ? '...' : ''}
              </p>

              <div className="mt-4 border-t border-[var(--panel-border)] pt-3">
                <Button
                  size="sm"
                  variant={shareSuccess === skill.name ? 'secondary' : 'primary'}
                  icon={shareSuccess === skill.name ? CheckCircle : Share2}
                  onClick={() => handleShare(skill.name)}
                  disabled={shareSuccess === skill.name}
                >
                  {shareSuccess === skill.name
                    ? 'Shared!'
                    : intl.formatMessage({ id: 'skills.shared.share' })}
                </Button>
              </div>
            </Card>
          ))}
        </div>
      )}
    </div>
  );
}

function SkillCard({
  skill,
  onInstall,
}: {
  skill: SkillIndexEntry;
  onInstall: () => void;
}) {
  const intl = useIntl();
  return (
    <Card interactive>
      <div className="mb-3 flex items-start justify-between">
        <h3 className="font-semibold text-stone-900 dark:text-stone-50">
          {skill.name}
        </h3>
        {skill.url && /^https?:\/\//i.test(skill.url) && (
          <a
            href={skill.url}
            target="_blank"
            rel="noopener noreferrer"
            className="text-stone-400 hover:text-amber-500"
          >
            <ExternalLink className="h-4 w-4" />
          </a>
        )}
      </div>

      <p className="mb-3 text-sm text-stone-600 dark:text-stone-400">
        {skill.description || intl.formatMessage({ id: 'common.noData' })}
      </p>

      {skill.tags.length > 0 && (
        <div className="mb-3 flex flex-wrap gap-1.5">
          {skill.tags.map((tag) => (
            <Badge key={tag} tone="neutral">{tag}</Badge>
          ))}
        </div>
      )}

      <div className="flex items-center justify-between border-t border-[var(--panel-border)] pt-3">
        {skill.author && (
          <span className="flex items-center gap-1 text-xs text-stone-400">
            <CharacterAvatar agentId={skill.author} name={skill.author} size={24} />
            {skill.author}
          </span>
        )}
        <div className="flex items-center gap-2">
          {skill.url && /^https?:\/\//i.test(skill.url) && (
            <Button
              size="sm"
              variant="secondary"
              icon={ExternalLink}
              onClick={() => window.open(skill.url, '_blank', 'noopener,noreferrer')}
            >
              GitHub
            </Button>
          )}
          <Button
            size="sm"
            variant="primary"
            icon={Download}
            onClick={onInstall}
          >
            {intl.formatMessage({ id: 'skills.market.install' })}
          </Button>
        </div>
      </div>
    </Card>
  );
}

const SEVERITY_COLORS: Record<string, string> = {
  critical: 'text-rose-500',
  high: 'text-orange-500',
  medium: 'text-amber-500',
  low: 'text-stone-400',
};

const SEVERITY_BG: Record<string, string> = {
  critical: 'bg-rose-50 border-rose-200 dark:bg-rose-900/20 dark:border-rose-800',
  high: 'bg-orange-50 border-orange-200 dark:bg-orange-900/20 dark:border-orange-800',
  medium: 'bg-amber-50 border-amber-200 dark:bg-amber-900/20 dark:border-amber-800',
  low: 'bg-stone-50 border-stone-200 dark:bg-stone-800/50 dark:border-stone-700',
};

function InstallDialog({
  skill,
  onClose,
}: {
  skill: SkillIndexEntry;
  onClose: () => void;
}) {
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
  const departmentOptions: string[] = departmentsOf(agents);

  return (
    <Dialog
      open
      onClose={onClose}
      title={intl.formatMessage({ id: 'skills.install.title' })}
      className="max-w-xl"
    >
      <div className="space-y-5">
        {/* Skill info */}
        <div>
          <h4 className="font-semibold text-stone-900 dark:text-stone-50">{skill.name}</h4>
          <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
            {skill.description || intl.formatMessage({ id: 'common.noData' })}
          </p>
        </div>

        {/* Scope selector — company / department / individual (WP7). */}
        <Field label={intl.formatMessage({ id: 'skills.install.scope' })}>
          <select
            value={scope}
            onChange={(e) => setScope(e.target.value)}
            className={controlClass}
          >
            <option value="global">
              {intl.formatMessage({ id: 'skills.install.scopeGlobal' })}
            </option>
            {departmentOptions.length > 0 && (
              <optgroup label={intl.formatMessage({ id: 'skills.install.scopeDeptGroup' })}>
                {departmentOptions.map((dept) => (
                  <option key={dept} value={`department:${dept}`}>
                    {intl.formatMessage({ id: 'skills.install.scopeDept' }, { dept })}
                  </option>
                ))}
              </optgroup>
            )}
            <optgroup label={intl.formatMessage({ id: 'skills.install.scopeAgentGroup' })}>
              {agents.map((agent) => (
                <option key={agent.name} value={agent.name}>
                  {intl.formatMessage(
                    { id: 'skills.install.scopeAgent' },
                    { agent: agent.display_name || agent.name },
                  )}
                </option>
              ))}
            </optgroup>
          </select>
        </Field>

        {/* Security scan button */}
        <div>
          <Button
            variant="secondary"
            icon={scanning ? Loader2 : Shield}
            onClick={handleScan}
            disabled={scanning || !skill.url}
            className={cn(scanning && '[&>svg]:animate-spin')}
          >
            {scanning
              ? intl.formatMessage({ id: 'skills.install.scanning' })
              : intl.formatMessage({ id: 'skills.install.scan' })}
          </Button>
        </div>

        {/* Scan results */}
        {scanPassed && scanResult && (
          <div className="space-y-3">
            <div className="flex items-center gap-2 text-emerald-600 dark:text-emerald-400">
              <ShieldCheck className="h-5 w-5" />
              <span className="text-sm font-medium">
                {intl.formatMessage({ id: 'skills.install.scanPassed' })}
              </span>
              <span className="ml-auto text-sm text-stone-500 dark:text-stone-400">
                {intl.formatMessage(
                  { id: 'skills.install.score' },
                  { score: scanResult.vet_result.score },
                )}
              </span>
            </div>
            {scanResult.vet_result.findings.length > 0 ? (
              <FindingsList findings={scanResult.vet_result.findings} />
            ) : (
              <p className="text-sm text-stone-400 dark:text-stone-500">
                {intl.formatMessage({ id: 'skills.install.noFindings' })}
              </p>
            )}
          </div>
        )}

        {scanFailed && scanResult && (
          <div className="space-y-3">
            <div className="flex items-center gap-2 text-rose-600 dark:text-rose-400">
              <ShieldAlert className="h-5 w-5" />
              <span className="text-sm font-medium">
                {intl.formatMessage({ id: 'skills.install.scanFailed' })}
              </span>
              <span className="ml-auto text-sm text-stone-500 dark:text-stone-400">
                {intl.formatMessage(
                  { id: 'skills.install.score' },
                  { score: scanResult.vet_result.score },
                )}
              </span>
            </div>
            {scanResult.vet_result.findings.length > 0 && (
              <FindingsList findings={scanResult.vet_result.findings} />
            )}
          </div>
        )}

        {/* Error */}
        {error && (
          <div className="rounded-control border border-rose-200 bg-rose-50 p-3 text-sm text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-400">
            {error}
          </div>
        )}

        {/* Non-admin notice: install goes through approval */}
        {!isAdmin && scanPassed && !requested && (
          <p className="text-xs text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'install.request.nonAdminNotice' })}
          </p>
        )}

        {/* Success message (admin direct install) */}
        {installed && scanResult && (
          <div className="flex items-center gap-2 rounded-control border border-emerald-200 bg-emerald-50 p-3 text-sm text-emerald-700 dark:border-emerald-800 dark:bg-emerald-900/20 dark:text-emerald-400">
            <CheckCircle className="h-4 w-4 flex-shrink-0" />
            {intl.formatMessage(
              { id: 'skills.install.success' },
              { name: scanResult.skill_name || skill.name },
            )}
          </div>
        )}

        {/* Request-filed message (non-admin) */}
        {requested && (
          <div className="flex items-center gap-2 rounded-control border border-amber-200 bg-amber-50 p-3 text-sm text-amber-700 dark:border-amber-800 dark:bg-amber-900/20 dark:text-amber-400">
            <CheckCircle className="h-4 w-4 flex-shrink-0" />
            {intl.formatMessage({
              id: requested === 'awaiting_manager'
                ? 'install.request.filedManager'
                : 'install.request.filedAdmin',
            })}
          </div>
        )}

        {/* Action buttons */}
        <div className="flex items-center justify-end gap-3 border-t border-[var(--panel-border)] pt-4">
          <Button variant="secondary" onClick={onClose}>
            {intl.formatMessage({ id: requested || installed ? 'common.close' : 'common.cancel' })}
          </Button>
          {!installed && !requested && (
            <Button
              variant="primary"
              icon={installing ? Loader2 : Download}
              onClick={handleInstall}
              disabled={!scanPassed || installing}
              title={!scanPassed ? intl.formatMessage({ id: 'skills.install.requireScan' }) : undefined}
              className={cn(installing && '[&>svg]:animate-spin')}
            >
              {installing
                ? intl.formatMessage({ id: isAdmin ? 'skills.install.installing' : 'install.request.submitting' })
                : intl.formatMessage({ id: isAdmin ? 'skills.install.installBtn' : 'install.request.submit' })}
            </Button>
          )}
        </div>
      </div>
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
      <p className="text-xs font-medium text-stone-500 dark:text-stone-400">
        {intl.formatMessage({ id: 'skills.install.findings' })}
      </p>
      <ul className="space-y-1.5">
        {findings.map((f, i) => {
          const severityKey = f.severity.toLowerCase();
          return (
            <li
              key={i}
              className={`flex items-start gap-2 rounded-control border p-2.5 text-sm ${SEVERITY_BG[severityKey] ?? SEVERITY_BG.low}`}
            >
              <AlertTriangle className={`mt-0.5 h-3.5 w-3.5 flex-shrink-0 ${SEVERITY_COLORS[severityKey] ?? SEVERITY_COLORS.low}`} />
              <div className="min-w-0">
                <span className={`text-xs font-semibold uppercase ${SEVERITY_COLORS[severityKey] ?? SEVERITY_COLORS.low}`}>
                  {intl.formatMessage({ id: `skills.install.severity.${severityKey}` })}
                </span>
                <span className="mx-1.5 text-stone-300 dark:text-stone-600">|</span>
                <span className="text-xs text-stone-500 dark:text-stone-400">{f.category}</span>
                <p className="mt-0.5 text-stone-700 dark:text-stone-300">{f.description}</p>
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
