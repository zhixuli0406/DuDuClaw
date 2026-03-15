import { useState, useCallback, useEffect } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type MemoryEntry, type SkillInfo } from '@/lib/api';
import {
  Brain,
  Search,
  Tag,
  Clock,
  Sparkles,
  BookOpen,
  Shield,
  GitBranch,
} from 'lucide-react';

type TabId = 'memories' | 'skills' | 'evolution';

export function MemoryPage() {
  const intl = useIntl();
  const [activeTab, setActiveTab] = useState<TabId>('memories');

  const tabs: ReadonlyArray<{ id: TabId; label: string }> = [
    { id: 'memories', label: intl.formatMessage({ id: 'memory.tab.memories' }) },
    { id: 'skills', label: intl.formatMessage({ id: 'memory.tab.skills' }) },
    { id: 'evolution', label: intl.formatMessage({ id: 'memory.tab.evolution' }) },
  ];

  return (
    <div className="space-y-6">
      <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
        {intl.formatMessage({ id: 'memory.title' })}
      </h2>

      {/* Tabs */}
      <div className="flex gap-1 rounded-lg bg-stone-100 p-1 dark:bg-stone-800">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            className={cn(
              'flex-1 rounded-md px-4 py-2 text-sm font-medium transition-colors',
              activeTab === tab.id
                ? 'bg-white text-stone-900 shadow-sm dark:bg-stone-700 dark:text-stone-50'
                : 'text-stone-500 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-300'
            )}
          >
            {tab.label}
          </button>
        ))}
      </div>

      {activeTab === 'memories' && <MemoriesTab />}
      {activeTab === 'skills' && <SkillsTab />}
      {activeTab === 'evolution' && <EvolutionTab />}
    </div>
  );
}

function MemoriesTab() {
  const intl = useIntl();
  const [query, setQuery] = useState('');
  const [entries, setEntries] = useState<ReadonlyArray<MemoryEntry>>([]);
  const [loading, setLoading] = useState(false);

  const handleSearch = useCallback(async () => {
    if (!query.trim()) return;
    setLoading(true);
    try {
      const result = await api.memory.search('*', query);
      setEntries(result?.entries ?? []);
    } catch {
      // error handled silently
    } finally {
      setLoading(false);
    }
  }, [query]);

  return (
    <div className="space-y-4">
      {/* Search bar */}
      <div className="flex gap-2">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-stone-400" />
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
            placeholder={intl.formatMessage({
              id: 'memory.search.placeholder',
            })}
            className="w-full rounded-lg border border-stone-200 bg-white py-2.5 pl-10 pr-4 text-sm text-stone-900 placeholder:text-stone-400 focus:border-amber-500 focus:outline-none focus:ring-1 focus:ring-amber-500 dark:border-stone-700 dark:bg-stone-800 dark:text-stone-50 dark:placeholder:text-stone-500"
          />
        </div>
        <button
          onClick={handleSearch}
          disabled={loading}
          className="rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
        >
          <Search className="h-4 w-4" />
        </button>
      </div>

      {/* Memory entries */}
      {entries.length === 0 ? (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
          <Brain className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'common.noData' })}
          </p>
        </div>
      ) : (
        <div className="space-y-3">
          {entries.map((entry) => (
            <div
              key={entry.id}
              className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-800 dark:bg-stone-900"
            >
              <div className="mb-2 flex items-center justify-between">
                <span className="text-xs font-medium text-amber-600 dark:text-amber-400">
                  {entry.agent_id}
                </span>
                <span className="flex items-center gap-1 text-xs text-stone-400 dark:text-stone-500">
                  <Clock className="h-3 w-3" />
                  {new Date(entry.timestamp).toLocaleString('zh-TW')}
                </span>
              </div>
              <p className="text-sm text-stone-700 dark:text-stone-300">
                {entry.content}
              </p>
              {entry.tags.length > 0 && (
                <div className="mt-3 flex flex-wrap gap-1.5">
                  {entry.tags.map((tag) => (
                    <span
                      key={tag}
                      className="inline-flex items-center gap-1 rounded-full bg-stone-100 px-2 py-0.5 text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400"
                    >
                      <Tag className="h-2.5 w-2.5" />
                      {tag}
                    </span>
                  ))}
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function SkillsTab() {
  const intl = useIntl();
  const [skills, setSkills] = useState<ReadonlyArray<SkillInfo>>([]);
  const [loading, setLoading] = useState(false);

  const fetchSkills = useCallback(async () => {
    setLoading(true);
    try {
      const result = await api.skills.list() as Record<string, unknown>;
      // When no agent_id, backend returns { agents: [{ agent_id, skills }] }
      // When with agent_id, returns { skills: [...] }
      if (Array.isArray(result.skills)) {
        setSkills(result.skills as SkillInfo[]);
      } else if (Array.isArray(result.agents)) {
        const all: SkillInfo[] = [];
        for (const ag of result.agents as Array<{ agent_id: string; skills: Array<{ name: string; size: number }> }>) {
          for (const s of ag.skills) {
            all.push({ name: s.name, agent_id: ag.agent_id, content: '', security_status: undefined });
          }
        }
        setSkills(all);
      }
    } catch {
      // error handled silently
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchSkills();
  }, [fetchSkills]);

  const securityStyles: Record<string, string> = {
    pass: 'text-emerald-600 dark:text-emerald-400',
    warn: 'text-amber-600 dark:text-amber-400',
    fail: 'text-rose-600 dark:text-rose-400',
  };

  return (
    <div className="space-y-4">
      {skills.length === 0 && !loading ? (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
          <BookOpen className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'common.noData' })}
          </p>
        </div>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {skills.map((skill) => (
            <div
              key={skill.name}
              className="rounded-xl border border-stone-200 bg-white p-5 transition-shadow hover:shadow-md dark:border-stone-800 dark:bg-stone-900"
            >
              <div className="flex items-start justify-between">
                <div className="flex items-center gap-3">
                  <div className="rounded-lg bg-amber-100 p-2 dark:bg-amber-900/30">
                    <Sparkles className="h-4 w-4 text-amber-600 dark:text-amber-400" />
                  </div>
                  <div>
                    <h3 className="font-semibold text-stone-900 dark:text-stone-50">
                      {skill.name}
                    </h3>
                    {skill.agent_id && (
                      <p className="text-xs text-stone-500 dark:text-stone-400">
                        {skill.agent_id}
                      </p>
                    )}
                  </div>
                </div>
                {skill.security_status && (
                  <Shield
                    className={cn(
                      'h-4 w-4',
                      securityStyles[skill.security_status] ??
                        'text-stone-400'
                    )}
                  />
                )}
              </div>
              <p className="mt-3 line-clamp-3 text-sm text-stone-600 dark:text-stone-400">
                {skill.content}
              </p>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function EvolutionTab() {
  const intl = useIntl();

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-6 dark:border-stone-800 dark:bg-stone-900">
      <div className="flex items-center gap-3 mb-6">
        <div className="rounded-lg bg-amber-100 p-2.5 dark:bg-amber-900/30">
          <GitBranch className="h-5 w-5 text-amber-600 dark:text-amber-400" />
        </div>
        <h3 className="text-lg font-medium text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'memory.tab.evolution' })}
        </h3>
      </div>

      {/* Placeholder timeline */}
      <div className="flex flex-col items-center justify-center py-12">
        <div className="mb-6 h-32 w-px bg-gradient-to-b from-amber-500 via-amber-300 to-transparent" />
        <p className="text-stone-400 dark:text-stone-500">
          {intl.formatMessage({ id: 'common.noData' })}
        </p>
      </div>
    </div>
  );
}
