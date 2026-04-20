import { useState, useEffect, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type SkillIndexEntry, type SharedSkillInfo, type SkillInfo } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { Dialog } from '@/components/shared/Dialog';
import { toast, formatError } from '@/lib/toast';
import {
  Search,
  Tag,
  User,
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

type SkillTab = 'market' | 'shared' | 'mySkills';

export function SkillMarketPage() {
  const intl = useIntl();
  const [activeTab, setActiveTab] = useState<SkillTab>('market');

  const tabItems: ReadonlyArray<{ id: SkillTab; label: string; icon: React.ComponentType<{ className?: string }> }> = [
    { id: 'market', label: intl.formatMessage({ id: 'skills.tab.market' }), icon: Store },
    { id: 'shared', label: intl.formatMessage({ id: 'skills.tab.shared' }), icon: Users },
    { id: 'mySkills', label: intl.formatMessage({ id: 'skills.tab.mySkills' }), icon: Sparkles },
  ];

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'skills.market.title' })}
      </h2>

      {/* Tabs */}
      <div className="flex gap-1 rounded-lg bg-stone-100 p-1 dark:bg-stone-800">
        {tabItems.map((tab) => {
          const TabIcon = tab.icon;
          return (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={cn(
                'flex items-center gap-2 whitespace-nowrap rounded-md px-4 py-2 text-sm font-medium transition-colors',
                activeTab === tab.id
                  ? 'bg-white text-stone-900 shadow-sm dark:bg-stone-700 dark:text-stone-50'
                  : 'text-stone-500 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-300',
              )}
            >
              <TabIcon className="h-4 w-4" />
              {tab.label}
            </button>
          );
        })}
      </div>

      {activeTab === 'market' && <MarketTab />}
      {activeTab === 'shared' && <SharedSkillsTab />}
      {activeTab === 'mySkills' && <MySkillsTab />}
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
      <div className="flex gap-3">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-stone-400" />
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
            placeholder={intl.formatMessage({ id: 'skills.market.searchPlaceholder' })}
            className="w-full rounded-lg border border-stone-200 bg-white py-2.5 pl-10 pr-4 text-sm text-stone-900 placeholder-stone-400 transition-colors focus:border-amber-400 focus:outline-none focus:ring-1 focus:ring-amber-400 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-50"
          />
        </div>
        <button
          onClick={handleSearch}
          disabled={loading}
          className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-5 py-2.5 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
        >
          <Search className="h-4 w-4" />
          {intl.formatMessage({ id: 'skills.market.search' })}
        </button>
      </div>

      {loading && (
        <div className="py-12 text-center text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      )}

      {!loading && searched && results.length === 0 && (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 py-16 dark:border-stone-700">
          <Search className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'skills.market.noResults' })}
          </p>
        </div>
      )}

      {!loading && results.length > 0 && (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {results.map((skill) => (
            <SkillCard key={skill.name} skill={skill} onInstall={() => setInstallSkill(skill)} />
          ))}
        </div>
      )}

      {!searched && (
        <div>
          <h3 className="mb-4 text-lg font-medium text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'skills.market.categories' })}
          </h3>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            {['utility', 'communication', 'code', 'data', 'security', 'ai', 'media', 'automation'].map((cat) => (
              <button
                key={cat}
                onClick={() => { setQuery(cat); handleSearchQuery(cat); }}
                className="flex items-center gap-2 rounded-lg border border-stone-200 bg-white px-4 py-3 text-sm text-stone-700 transition-colors hover:border-amber-300 hover:bg-amber-50 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-300 dark:hover:border-amber-600 dark:hover:bg-amber-900/20"
              >
                <Tag className="h-4 w-4 text-amber-500" />
                {cat}
              </button>
            ))}
          </div>
        </div>
      )}

      {installSkill && <InstallDialog skill={installSkill} onClose={() => setInstallSkill(null)} />}
    </div>
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
      setTimeout(() => { setAdoptTarget(null); setAdoptSuccess(null); }, 1500);
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
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
          <Share2 className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'skills.shared.empty' })}
          </p>
        </div>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {sharedSkills.map((skill) => (
            <div
              key={skill.name}
              className="rounded-xl border border-stone-200 bg-white p-5 transition-shadow hover:shadow-md dark:border-stone-800 dark:bg-stone-900"
            >
              <h3 className="font-semibold text-stone-900 dark:text-stone-50">{skill.name}</h3>
              <p className="mt-1 text-sm text-stone-600 dark:text-stone-400">
                {skill.description || '—'}
              </p>

              {skill.tags.length > 0 && (
                <div className="mt-3 flex flex-wrap gap-1.5">
                  {skill.tags.map((tag) => (
                    <span key={tag} className="rounded-full bg-stone-100 px-2 py-0.5 text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400">
                      {tag}
                    </span>
                  ))}
                </div>
              )}

              <div className="mt-4 space-y-2 text-xs text-stone-500 dark:text-stone-400">
                <div className="flex items-center gap-2">
                  <User className="h-3 w-3" />
                  <span>{intl.formatMessage({ id: 'skills.shared.sharedBy' })}: {skill.shared_by}</span>
                </div>
                <div className="flex items-center gap-2">
                  <Download className="h-3 w-3" />
                  <span>{intl.formatMessage({ id: 'skills.shared.usageCount' })}: {skill.usage_count}</span>
                </div>
                {skill.adopted_by.length > 0 && (
                  <div className="flex items-center gap-2">
                    <Users className="h-3 w-3" />
                    <span>{intl.formatMessage({ id: 'skills.shared.adoptedBy' })}: {skill.adopted_by.join(', ')}</span>
                  </div>
                )}
              </div>

              <div className="mt-4 border-t border-stone-100 pt-3 dark:border-stone-800">
                <button
                  onClick={() => {
                    setAdoptTarget(skill);
                    setAdoptAgent(agents[0]?.name ?? '');
                    setAdoptSuccess(null);
                  }}
                  className="inline-flex items-center gap-1 rounded-md bg-amber-100 px-3 py-1.5 text-xs font-medium text-amber-700 transition-colors hover:bg-amber-200 dark:bg-amber-900/30 dark:text-amber-400 dark:hover:bg-amber-900/50"
                >
                  <Download className="h-3.5 w-3.5" />
                  {intl.formatMessage({ id: 'skills.shared.adopt' })}
                </button>
              </div>
            </div>
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
              className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none focus:ring-2 focus:ring-amber-500/20 dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
            >
              {agents.map((a) => (
                <option key={a.name} value={a.name}>{a.icon || '🤖'} {a.display_name}</option>
              ))}
            </select>
            {adoptSuccess && (
              <div className="flex items-center gap-2 rounded-lg border border-emerald-200 bg-emerald-50 p-3 text-sm text-emerald-700 dark:border-emerald-800 dark:bg-emerald-900/20 dark:text-emerald-400">
                <CheckCircle className="h-4 w-4 flex-shrink-0" />
                {adoptSuccess}
              </div>
            )}
            <div className="flex justify-end gap-3">
              <button
                onClick={() => { setAdoptTarget(null); setAdoptSuccess(null); }}
                className="rounded-lg border border-stone-300 px-4 py-2 text-sm font-medium text-stone-700 transition-colors hover:bg-stone-50 dark:border-stone-600 dark:text-stone-300 dark:hover:bg-stone-800"
              >
                {intl.formatMessage({ id: 'common.cancel' })}
              </button>
              <button
                onClick={handleAdopt}
                disabled={!adoptAgent || !!adoptSuccess}
                className="rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
              >
                {intl.formatMessage({ id: 'skills.shared.adopt' })}
              </button>
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
      {/* Agent selector */}
      <div className="flex items-center gap-3">
        <label className="text-sm font-medium text-stone-700 dark:text-stone-300">Agent:</label>
        <select
          value={selectedAgent}
          onChange={(e) => setSelectedAgent(e.target.value)}
          className="rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none focus:ring-2 focus:ring-amber-500/20 dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50"
        >
          {agents.map((a) => (
            <option key={a.name} value={a.name}>{a.icon || '🤖'} {a.display_name}</option>
          ))}
        </select>
      </div>

      {loading ? (
        <div className="py-12 text-center text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</div>
      ) : skills.length === 0 ? (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
          <Sparkles className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'common.noData' })}
          </p>
        </div>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {skills.map((skill) => (
            <div
              key={skill.name}
              className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-800 dark:bg-stone-900"
            >
              <div className="flex items-start justify-between">
                <h3 className="font-semibold text-stone-900 dark:text-stone-50">{skill.name}</h3>
                {skill.security_status && (
                  <span className={cn(
                    'rounded-full px-2 py-0.5 text-xs font-medium',
                    skill.security_status === 'pass' ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400' :
                    skill.security_status === 'warn' ? 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400' :
                    'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
                  )}>
                    {skill.security_status}
                  </span>
                )}
              </div>

              <p className="mt-2 line-clamp-3 text-sm text-stone-600 dark:text-stone-400">
                {skill.content.slice(0, 150)}{skill.content.length > 150 ? '...' : ''}
              </p>

              <div className="mt-4 border-t border-stone-100 pt-3 dark:border-stone-800">
                <button
                  onClick={() => handleShare(skill.name)}
                  disabled={shareSuccess === skill.name}
                  className={cn(
                    'inline-flex items-center gap-1 rounded-md px-3 py-1.5 text-xs font-medium transition-colors',
                    shareSuccess === skill.name
                      ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400'
                      : 'bg-blue-100 text-blue-700 hover:bg-blue-200 dark:bg-blue-900/30 dark:text-blue-400 dark:hover:bg-blue-900/50',
                  )}
                >
                  {shareSuccess === skill.name ? (
                    <><CheckCircle className="h-3.5 w-3.5" /> Shared!</>
                  ) : (
                    <><Share2 className="h-3.5 w-3.5" /> {intl.formatMessage({ id: 'skills.shared.share' })}</>
                  )}
                </button>
              </div>
            </div>
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
    <div className="rounded-xl border border-stone-200 bg-white p-5 transition-shadow hover:shadow-md dark:border-stone-800 dark:bg-stone-900">
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
            <span
              key={tag}
              className="inline-flex items-center rounded-full bg-stone-100 px-2 py-0.5 text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400"
            >
              {tag}
            </span>
          ))}
        </div>
      )}

      <div className="flex items-center justify-between border-t border-stone-100 pt-3 dark:border-stone-800">
        {skill.author && (
          <span className="flex items-center gap-1 text-xs text-stone-400">
            <User className="h-3 w-3" />
            {skill.author}
          </span>
        )}
        <div className="flex items-center gap-2">
          {skill.url && /^https?:\/\//i.test(skill.url) && (
            <a
              href={skill.url}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex items-center gap-1 rounded-md bg-amber-100 px-2.5 py-1.5 text-xs font-medium text-amber-700 transition-colors hover:bg-amber-200 dark:bg-amber-900/30 dark:text-amber-400 dark:hover:bg-amber-900/50"
            >
              <ExternalLink className="h-3.5 w-3.5" />
              GitHub
            </a>
          )}
          <button
            onClick={onInstall}
            className="inline-flex items-center gap-1 rounded-md bg-emerald-100 px-2.5 py-1.5 text-xs font-medium text-emerald-700 transition-colors hover:bg-emerald-200 dark:bg-emerald-900/30 dark:text-emerald-400 dark:hover:bg-emerald-900/50"
          >
            <Download className="h-3.5 w-3.5" />
            {intl.formatMessage({ id: 'skills.market.install' })}
          </button>
        </div>
      </div>
    </div>
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
  const [scope, setScope] = useState('global');
  const [scanning, setScanning] = useState(false);
  const [scanResult, setScanResult] = useState<VetResponse | null>(null);
  const [installing, setInstalling] = useState(false);
  const [installed, setInstalled] = useState(false);
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
      await api.skills.install(skill.url, scope, scanResult.content);
      setInstalled(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setInstalling(false);
    }
  }, [scanResult, skill.url, scope]);

  const scanPassed = scanResult?.passed === true;
  const scanFailed = scanResult !== null && !scanResult.passed;

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

        {/* Scope selector */}
        <div className="space-y-1.5">
          <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: 'skills.install.scope' })}
          </label>
          <select
            value={scope}
            onChange={(e) => setScope(e.target.value)}
            className="w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 focus:border-amber-500 focus:outline-none focus:ring-2 focus:ring-amber-500/20 dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50 dark:focus:border-amber-400"
          >
            <option value="global">
              {intl.formatMessage({ id: 'skills.install.scopeGlobal' })}
            </option>
            {agents.map((agent) => (
              <option key={agent.name} value={agent.name}>
                {intl.formatMessage(
                  { id: 'skills.install.scopeAgent' },
                  { agent: agent.display_name || agent.name },
                )}
              </option>
            ))}
          </select>
        </div>

        {/* Security scan button */}
        <div>
          <button
            onClick={handleScan}
            disabled={scanning || !skill.url}
            className="inline-flex items-center gap-2 rounded-lg border border-stone-300 bg-white px-4 py-2 text-sm font-medium text-stone-700 transition-colors hover:bg-stone-50 disabled:opacity-50 dark:border-stone-600 dark:bg-stone-800 dark:text-stone-300 dark:hover:bg-stone-700"
          >
            {scanning ? (
              <>
                <Loader2 className="h-4 w-4 animate-spin" />
                {intl.formatMessage({ id: 'skills.install.scanning' })}
              </>
            ) : (
              <>
                <Shield className="h-4 w-4" />
                {intl.formatMessage({ id: 'skills.install.scan' })}
              </>
            )}
          </button>
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
          <div className="rounded-lg border border-rose-200 bg-rose-50 p-3 text-sm text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-400">
            {error}
          </div>
        )}

        {/* Success message */}
        {installed && scanResult && (
          <div className="flex items-center gap-2 rounded-lg border border-emerald-200 bg-emerald-50 p-3 text-sm text-emerald-700 dark:border-emerald-800 dark:bg-emerald-900/20 dark:text-emerald-400">
            <CheckCircle className="h-4 w-4 flex-shrink-0" />
            {intl.formatMessage(
              { id: 'skills.install.success' },
              { name: scanResult.skill_name || skill.name },
            )}
          </div>
        )}

        {/* Action buttons */}
        <div className="flex items-center justify-end gap-3 border-t border-stone-200 pt-4 dark:border-stone-700">
          <button
            onClick={onClose}
            className="inline-flex items-center justify-center gap-2 rounded-lg border border-stone-300 bg-white px-4 py-2 text-sm font-medium text-stone-700 transition-colors hover:bg-stone-50 dark:border-stone-600 dark:bg-stone-800 dark:text-stone-300 dark:hover:bg-stone-700"
          >
            {intl.formatMessage({ id: 'common.cancel' })}
          </button>
          {!installed && (
            <button
              onClick={handleInstall}
              disabled={!scanPassed || installing}
              title={!scanPassed ? intl.formatMessage({ id: 'skills.install.requireScan' }) : undefined}
              className="inline-flex items-center justify-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
            >
              {installing ? (
                <>
                  <Loader2 className="h-4 w-4 animate-spin" />
                  {intl.formatMessage({ id: 'skills.install.installing' })}
                </>
              ) : (
                <>
                  <Download className="h-4 w-4" />
                  {intl.formatMessage({ id: 'skills.install.installBtn' })}
                </>
              )}
            </button>
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
              className={`flex items-start gap-2 rounded-lg border p-2.5 text-sm ${SEVERITY_BG[severityKey] ?? SEVERITY_BG.low}`}
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
