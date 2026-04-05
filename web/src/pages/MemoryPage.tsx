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
  CheckCircle,
  XCircle,
  Eye,
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
  const [agents, setAgents] = useState<ReadonlyArray<{ name: string; display_name: string }>>([]);
  const [selectedAgent, setSelectedAgent] = useState('');

  // Load agent list on mount
  useEffect(() => {
    api.agents.list().then((res) => {
      const list = res?.agents ?? [];
      setAgents(list);
      if (list.length > 0 && !selectedAgent) {
        setSelectedAgent(list[0].name);
      }
    }).catch(() => {});
  }, []);

  // Auto-browse when agent changes
  useEffect(() => {
    if (!selectedAgent) return;
    setLoading(true);
    api.memory.browse(selectedAgent, 50).then((res) => {
      setEntries(res?.entries ?? []);
    }).catch(() => {
      setEntries([]);
    }).finally(() => setLoading(false));
  }, [selectedAgent]);

  const handleSearch = useCallback(async () => {
    if (!query.trim() || !selectedAgent) return;
    setLoading(true);
    try {
      const result = await api.memory.search(selectedAgent, query);
      setEntries(result?.entries ?? []);
    } catch {
      setEntries([]);
    } finally {
      setLoading(false);
    }
  }, [query, selectedAgent]);

  const selectStyle = 'rounded-lg border border-stone-200 bg-white px-3 py-2.5 text-sm text-stone-900 focus:border-amber-500 focus:outline-none dark:border-stone-700 dark:bg-stone-800 dark:text-stone-50';

  return (
    <div className="space-y-4">
      {/* Agent selector + Search bar */}
      <div className="flex gap-2">
        <select
          value={selectedAgent}
          onChange={(e) => { setSelectedAgent(e.target.value); setQuery(''); }}
          className={selectStyle}
        >
          {agents.map((a) => (
            <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
          ))}
        </select>
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
          disabled={loading || !selectedAgent}
          className="rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50"
        >
          <Search className="h-4 w-4" />
        </button>
      </div>

      {/* Memory entries */}
      {loading ? (
        <div className="py-12 text-center text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      ) : entries.length === 0 ? (
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
                  {new Date(entry.timestamp).toLocaleString()}
                </span>
              </div>
              <p className="text-sm text-stone-700 dark:text-stone-300 whitespace-pre-wrap">
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
  const [skills, setSkills] = useState<ReadonlyArray<SkillInfo & { scope?: string }>>([]);
  const [loading, setLoading] = useState(false);
  const [expandedSkill, setExpandedSkill] = useState<string | null>(null);
  const [skillContent, setSkillContent] = useState<Record<string, string>>({});

  const fetchSkills = useCallback(async () => {
    setLoading(true);
    try {
      const result = await api.skills.list() as Record<string, unknown>;
      if (Array.isArray(result.skills)) {
        setSkills(result.skills as SkillInfo[]);
      } else if (Array.isArray(result.agents)) {
        const all: Array<SkillInfo & { scope?: string }> = [];
        for (const ag of result.agents as Array<{ agent_id: string; skills: Array<{ name: string; size: number; scope?: string }> }>) {
          for (const s of ag.skills) {
            all.push({ name: s.name, agent_id: ag.agent_id, content: '', security_status: undefined, scope: s.scope });
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

  const handleExpand = async (agentId: string, skillName: string) => {
    const key = `${agentId}:${skillName}`;
    if (expandedSkill === key) {
      setExpandedSkill(null);
      return;
    }
    setExpandedSkill(key);
    if (!skillContent[key]) {
      try {
        const res = await api.skills.content(agentId, skillName);
        setSkillContent((prev) => ({ ...prev, [key]: res?.content ?? '' }));
      } catch {
        setSkillContent((prev) => ({ ...prev, [key]: intl.formatMessage({ id: 'common.error' }) }));
      }
    }
  };

  const securityStyles: Record<string, string> = {
    pass: 'text-emerald-600 dark:text-emerald-400',
    warn: 'text-amber-600 dark:text-amber-400',
    fail: 'text-rose-600 dark:text-rose-400',
  };

  return (
    <div className="space-y-4">
      {loading ? (
        <div className="py-12 text-center text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      ) : skills.length === 0 ? (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
          <BookOpen className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'common.noData' })}
          </p>
        </div>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {skills.map((skill) => {
            const key = `${skill.agent_id ?? 'global'}:${skill.name}`;
            const isExpanded = expandedSkill === key;
            return (
              <div
                key={key}
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
                          {skill.scope && (
                            <span className="ml-1.5 rounded bg-stone-100 px-1 py-0.5 text-[10px] dark:bg-stone-800">
                              {skill.scope}
                            </span>
                          )}
                        </p>
                      )}
                    </div>
                  </div>
                  <div className="flex items-center gap-2">
                    {skill.security_status && (
                      <Shield
                        className={cn(
                          'h-4 w-4',
                          securityStyles[skill.security_status] ?? 'text-stone-400'
                        )}
                      />
                    )}
                    {skill.agent_id && (
                      <button
                        onClick={() => handleExpand(skill.agent_id!, skill.name)}
                        className="rounded p-1 text-stone-400 hover:bg-stone-100 hover:text-stone-600 dark:hover:bg-stone-800 dark:hover:text-stone-300"
                      >
                        <Eye className="h-3.5 w-3.5" />
                      </button>
                    )}
                  </div>
                </div>
                {isExpanded && (
                  <pre className="mt-3 max-h-48 overflow-auto rounded-lg bg-stone-50 p-3 text-xs text-stone-600 dark:bg-stone-800/50 dark:text-stone-400">
                    {skillContent[key] ?? intl.formatMessage({ id: 'common.loading' })}
                  </pre>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

interface EvolutionAgent {
  agent_id: string;
  gvu_enabled: boolean;
  cognitive_memory: boolean;
  skill_auto_activate: boolean;
  skill_security_scan: boolean;
  max_silence_hours: number;
  max_gvu_generations: number;
  observation_period_hours: number;
}

function EvolutionTab() {
  const intl = useIntl();
  const [agents, setAgents] = useState<EvolutionAgent[]>([]);
  const [mode, setMode] = useState('');
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    setLoading(true);
    api.evolution.status().then((res) => {
      setAgents(res?.agents ?? []);
      setMode(res?.mode ?? '');
    }).catch(() => {}).finally(() => setLoading(false));
  }, []);

  return (
    <div className="space-y-4">
      {/* Mode banner */}
      {mode && (
        <div className="flex items-center gap-2 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 dark:border-amber-800 dark:bg-amber-900/20">
          <GitBranch className="h-4 w-4 text-amber-600 dark:text-amber-400" />
          <span className="text-sm text-amber-700 dark:text-amber-400">
            {intl.formatMessage({ id: 'evolution.mode' })}: <strong>{mode.replace('_', ' ')}</strong>
          </span>
        </div>
      )}

      {loading ? (
        <div className="py-12 text-center text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      ) : agents.length === 0 ? (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
          <GitBranch className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'common.noData' })}
          </p>
        </div>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {agents.map((agent) => (
            <div
              key={agent.agent_id}
              className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-800 dark:bg-stone-900"
            >
              <div className="mb-4 flex items-center gap-3">
                <div className="rounded-lg bg-amber-100 p-2 dark:bg-amber-900/30">
                  <GitBranch className="h-4 w-4 text-amber-600 dark:text-amber-400" />
                </div>
                <h3 className="font-semibold text-stone-900 dark:text-stone-50">{agent.agent_id}</h3>
              </div>

              <div className="space-y-2 text-sm">
                <EvolutionRow
                  label="GVU"
                  enabled={agent.gvu_enabled}
                />
                <EvolutionRow
                  label={intl.formatMessage({ id: 'agents.edit.cognitiveMemory' })}
                  enabled={agent.cognitive_memory}
                />
                <EvolutionRow
                  label={intl.formatMessage({ id: 'agents.edit.skillAutoActivate' })}
                  enabled={agent.skill_auto_activate}
                />
                <EvolutionRow
                  label={intl.formatMessage({ id: 'agents.edit.skillSecurityScan' })}
                  enabled={agent.skill_security_scan}
                />
              </div>

              <div className="mt-4 grid grid-cols-3 gap-2 border-t border-stone-100 pt-4 dark:border-stone-800">
                <div className="text-center">
                  <p className="text-lg font-semibold text-stone-900 dark:text-stone-50">
                    {agent.max_gvu_generations}
                  </p>
                  <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.maxGenerations' })}</p>
                </div>
                <div className="text-center">
                  <p className="text-lg font-semibold text-stone-900 dark:text-stone-50">
                    {agent.observation_period_hours}h
                  </p>
                  <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.observationPeriod' })}</p>
                </div>
                <div className="text-center">
                  <p className="text-lg font-semibold text-stone-900 dark:text-stone-50">
                    {agent.max_silence_hours}h
                  </p>
                  <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.maxSilence' })}</p>
                </div>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function EvolutionRow({ label, enabled }: { label: string; enabled: boolean }) {
  return (
    <div className="flex items-center justify-between">
      <span className="text-stone-600 dark:text-stone-400">{label}</span>
      {enabled ? (
        <CheckCircle className="h-4 w-4 text-emerald-500" />
      ) : (
        <XCircle className="h-4 w-4 text-stone-300 dark:text-stone-600" />
      )}
    </div>
  );
}
