import { useState, useCallback, useEffect } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type MemoryEntry, type SkillInfo, type EvolutionVersion, type KeyFactEntry } from '@/lib/api';
import { parsePredictionMemory, toPercent, type PredictionMemory } from '@/lib/memory-format';
import { Link } from 'react-router';
import { toast, formatError } from '@/lib/toast';
import {
  Page,
  PageHeader,
  Card,
  Section,
  Tabs,
  Button,
  Badge,
  EmptyState,
  Toolbar,
  controlClass,
  type TabItem,
} from '@/components/ui';
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
  ArrowRight,
  Lightbulb,
  Activity,
} from 'lucide-react';

type TabId = 'memories' | 'skills' | 'evolution' | 'insights';

export function MemoryPage() {
  const intl = useIntl();
  const [activeTab, setActiveTab] = useState<TabId>('memories');

  const tabs: TabItem[] = [
    { id: 'memories', label: intl.formatMessage({ id: 'memory.tab.memories' }), icon: Brain },
    { id: 'insights', label: intl.formatMessage({ id: 'memory.tab.insights' }), icon: Lightbulb },
    { id: 'skills', label: intl.formatMessage({ id: 'memory.tab.skills' }), icon: Sparkles },
    { id: 'evolution', label: intl.formatMessage({ id: 'memory.tab.evolution' }), icon: GitBranch },
  ];

  return (
    <Page wide>
      <PageHeader
        icon={Brain}
        title={intl.formatMessage({ id: 'nav.memory' })}
        subtitle={intl.formatMessage({ id: 'app.subtitle' })}
      />

      <Tabs items={tabs} value={activeTab} onChange={(id) => setActiveTab(id as TabId)} />

      {activeTab === 'memories' && <MemoriesTab />}
      {activeTab === 'insights' && <InsightsTab />}
      {activeTab === 'skills' && <SkillsTab />}
      {activeTab === 'evolution' && <EvolutionTab />}
    </Page>
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
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
    // Run once on mount; `selectedAgent` is only read inside the promise body
    // to seed a default and shouldn't retrigger this fetch. `intl` is stable
    // from react-intl context, so omitting it here is safe.
  }, []);

  // Auto-browse when agent changes
  useEffect(() => {
    if (!selectedAgent) return;
    setLoading(true);
    api.memory.browse(selectedAgent, 50).then((res) => {
      setEntries(res?.entries ?? []);
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setEntries([]);
    }).finally(() => setLoading(false));
  }, [selectedAgent, intl]);

  const handleSearch = useCallback(async () => {
    if (!query.trim() || !selectedAgent) return;
    setLoading(true);
    try {
      const result = await api.memory.search(selectedAgent, query);
      setEntries(result?.entries ?? []);
    } catch (e) {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setEntries([]);
    } finally {
      setLoading(false);
    }
  }, [query, selectedAgent, intl]);

  return (
    <div className="space-y-4">
      {/* Agent selector + Search bar */}
      <Toolbar
        search={query}
        onSearchChange={setQuery}
        onSearchEnter={handleSearch}
        searchPlaceholder={intl.formatMessage({ id: 'memory.search.placeholder' })}
      >
        <select
          value={selectedAgent}
          onChange={(e) => { setSelectedAgent(e.target.value); setQuery(''); }}
          className={cn(controlClass, 'w-auto')}
        >
          {agents.map((a) => (
            <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
          ))}
        </select>
        <Button
          variant="primary"
          icon={Search}
          onClick={handleSearch}
          disabled={loading || !selectedAgent}
          aria-label={intl.formatMessage({ id: 'memory.search.placeholder' })}
        />
      </Toolbar>

      {/* Memory entries */}
      {loading ? (
        <div className="py-12 text-center text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      ) : entries.length === 0 ? (
        <Card>
          <EmptyState
            icon={Brain}
            title={intl.formatMessage({ id: 'memory.empty.memories' })}
            action={
              <Link
                to="/agents"
                className="inline-flex items-center gap-1.5 text-sm font-medium text-amber-600 hover:text-amber-700 dark:text-amber-400 dark:hover:text-amber-300"
              >
                {intl.formatMessage({ id: 'memory.empty.memories.action' })}
                <ArrowRight className="h-3.5 w-3.5" />
              </Link>
            }
          />
        </Card>
      ) : (
        <div className="space-y-3">
          {entries.map((entry) => {
            const prediction = parsePredictionMemory(entry.content);
            if (prediction) {
              return <PredictionMemoryCard key={entry.id} entry={entry} data={prediction} />;
            }
            return (
              <Card key={entry.id}>
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
                      <Badge key={tag} tone="neutral">
                        <Tag className="h-2.5 w-2.5" />
                        {tag}
                      </Badge>
                    ))}
                  </div>
                )}
              </Card>
            );
          })}
        </div>
      )}
    </div>
  );
}

/**
 * Renders a prediction-deviation episodic memory as a human-readable "learning
 * signal" card instead of the raw English telemetry string (which users can't
 * parse). See {@link parsePredictionMemory}.
 */
function PredictionMemoryCard({ entry, data }: { entry: MemoryEntry; data: PredictionMemory }) {
  const intl = useIntl();
  const expectedPct = toPercent(data.expected);
  const actualPct = toPercent(data.inferred);
  const lower = data.inferred < data.expected;

  return (
    <Card>
      <div className="mb-2 flex items-center justify-between">
        <span className="flex items-center gap-1.5 text-xs font-medium text-amber-600 dark:text-amber-400">
          <Activity className="h-3.5 w-3.5" />
          {entry.agent_id}
          <span className="text-stone-400 dark:text-stone-500">
            · {intl.formatMessage({ id: 'memory.prediction.label' })}
          </span>
        </span>
        <span className="flex items-center gap-1 text-xs text-stone-400 dark:text-stone-500">
          <Clock className="h-3 w-3" />
          {new Date(entry.timestamp).toLocaleString()}
        </span>
      </div>
      <p className="text-sm text-stone-700 dark:text-stone-300">
        {intl.formatMessage({ id: 'memory.prediction.satisfaction' })}{' '}
        <span className="text-stone-500 dark:text-stone-400 tabular-nums">{expectedPct}%</span>
        <ArrowRight className="mx-1 inline h-3 w-3 text-stone-400" />
        <span
          className={cn(
            'font-medium tabular-nums',
            lower ? 'text-rose-600 dark:text-rose-400' : 'text-emerald-600 dark:text-emerald-400',
          )}
        >
          {actualPct}%
        </span>
      </p>
      <div className="mt-2.5 flex flex-wrap gap-1.5">
        <Badge tone="neutral">
          {intl.formatMessage({ id: 'memory.prediction.surprise' }, { value: toPercent(data.surprise) })}
        </Badge>
        {data.corrected && (
          <Badge tone="warning">{intl.formatMessage({ id: 'memory.prediction.corrected' })}</Badge>
        )}
        {data.followUp && (
          <Badge tone="accent">{intl.formatMessage({ id: 'memory.prediction.followUp' })}</Badge>
        )}
      </div>
      <p className="mt-2.5 text-xs text-stone-400 dark:text-stone-500">
        {intl.formatMessage({ id: 'memory.prediction.note' })}
      </p>
    </Card>
  );
}

function SkillsTab() {
  const intl = useIntl();
  const [skills, setSkills] = useState<ReadonlyArray<SkillInfo & { scope?: string }>>([]);
  const [loading, setLoading] = useState(false);
  const [expandedSkill, setExpandedSkill] = useState<string | null>(null);
  const [skillContent, setSkillContent] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);

  const fetchSkills = useCallback(async () => {
    setLoading(true);
    setError(null);
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
      setError(intl.formatMessage({ id: 'common.error' }));
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
      {error && (
        <div className="rounded-lg border border-rose-200 bg-rose-50 px-4 py-3 text-sm text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-400">
          {error}
        </div>
      )}
      {loading ? (
        <div className="py-12 text-center text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      ) : skills.length === 0 && !error ? (
        <Card>
          <EmptyState
            icon={BookOpen}
            title={intl.formatMessage({ id: 'memory.empty.skills' })}
            action={
              <Link
                to="/skills"
                className="inline-flex items-center gap-1.5 text-sm font-medium text-amber-600 hover:text-amber-700 dark:text-amber-400 dark:hover:text-amber-300"
              >
                {intl.formatMessage({ id: 'memory.empty.skills.action' })}
                <ArrowRight className="h-3.5 w-3.5" />
              </Link>
            }
          />
        </Card>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {skills.map((skill) => {
            const key = `${skill.agent_id ?? 'global'}:${skill.name}`;
            const isExpanded = expandedSkill === key;
            return (
              <Card key={key} interactive>
                <div className="flex items-start justify-between">
                  <div className="flex items-center gap-3">
                    <div className="rounded-lg bg-amber-500/12 p-2 text-amber-600 dark:text-amber-400">
                      <Sparkles className="h-4 w-4" />
                    </div>
                    <div>
                      <h3 className="font-semibold text-stone-900 dark:text-stone-50">
                        {skill.name}
                      </h3>
                      {skill.agent_id && (
                        <p className="flex items-center gap-1.5 text-xs text-stone-500 dark:text-stone-400">
                          {skill.agent_id}
                          {skill.scope && <Badge tone="neutral">{skill.scope}</Badge>}
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
                      <Button
                        variant="ghost"
                        size="sm"
                        icon={Eye}
                        onClick={() => handleExpand(skill.agent_id!, skill.name)}
                        aria-label={intl.formatMessage({ id: 'memory.tab.skills' })}
                      />
                    )}
                  </div>
                </div>
                {isExpanded && (
                  <pre className="mt-3 max-h-48 overflow-auto rounded-lg bg-stone-500/5 p-3 text-xs text-stone-600 dark:bg-white/5 dark:text-stone-400">
                    {skillContent[key] ?? intl.formatMessage({ id: 'common.loading' })}
                  </pre>
                )}
              </Card>
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
  const [enabled, setEnabled] = useState(false);
  const [gvuEnabledCount, setGvuEnabledCount] = useState(0);
  const [totalVersions, setTotalVersions] = useState(0);
  const [lastAppliedAt, setLastAppliedAt] = useState<string | null>(null);
  const [versions, setVersions] = useState<ReadonlyArray<EvolutionVersion>>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    setLoading(true);
    // Aggregate error surfacing — one toast covers the pair so a double
    // backend outage doesn't stack two notifications on the user.
    let notified = false;
    const onFailure = (e: unknown) => {
      console.warn("[api]", e);
      if (notified) return null;
      notified = true;
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      return null;
    };
    Promise.all([
      api.evolution.status().catch(onFailure),
      api.evolution.history(undefined, 20).catch(onFailure),
    ]).then(([status, history]) => {
      setAgents(status?.agents ?? []);
      setMode(status?.mode ?? '');
      setEnabled(status?.enabled ?? false);
      setGvuEnabledCount(status?.gvu_enabled_count ?? 0);
      setTotalVersions(status?.total_versions ?? 0);
      setLastAppliedAt(status?.last_applied_at ?? null);
      setVersions(history?.versions ?? []);
    }).finally(() => setLoading(false));
  }, [intl]);

  return (
    <div className="space-y-4">
      {/* Mode banner */}
      {mode && (
        <div className={cn(
          'flex flex-wrap items-center gap-x-4 gap-y-2 rounded-lg border px-4 py-3',
          enabled
            ? 'border-amber-200 bg-amber-50 dark:border-amber-800 dark:bg-amber-900/20'
            : 'border-[var(--panel-border)] bg-stone-500/5 dark:bg-white/5',
        )}>
          <div className="flex items-center gap-2">
            <GitBranch className={cn(
              'h-4 w-4',
              enabled ? 'text-amber-600 dark:text-amber-400' : 'text-stone-400',
            )} />
            <span className={cn(
              'text-sm',
              enabled ? 'text-amber-700 dark:text-amber-400' : 'text-stone-500 dark:text-stone-400',
            )}>
              {intl.formatMessage({ id: 'evolution.mode' })}: <strong>{mode.replace('_', ' ')}</strong>
            </span>
          </div>
          <span className="text-xs text-stone-500 dark:text-stone-400">
            {gvuEnabledCount}/{agents.length} {intl.formatMessage({ id: 'evolution.agentsEnabled' })}
          </span>
          {totalVersions > 0 && (
            <span className="text-xs text-stone-500 dark:text-stone-400">
              · {totalVersions} versions
            </span>
          )}
          {lastAppliedAt && (
            <span className="flex items-center gap-1 text-xs text-stone-500 dark:text-stone-400">
              <Clock className="h-3 w-3" />
              {new Date(lastAppliedAt).toLocaleString()}
            </span>
          )}
        </div>
      )}

      {loading ? (
        <div className="py-12 text-center text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      ) : agents.length === 0 ? (
        <Card>
          <EmptyState
            icon={GitBranch}
            title={intl.formatMessage({ id: 'common.noData' })}
          />
        </Card>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {agents.map((agent) => (
            <Card key={agent.agent_id}>
              <div className="mb-4 flex items-center gap-3">
                <div className="rounded-lg bg-amber-500/12 p-2 text-amber-600 dark:text-amber-400">
                  <GitBranch className="h-4 w-4" />
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

              <div className="mt-4 grid grid-cols-3 gap-2 border-t border-[var(--panel-border)] pt-4">
                <div className="text-center">
                  <p className="text-lg font-semibold tabular-nums text-stone-900 dark:text-stone-50">
                    {agent.max_gvu_generations}
                  </p>
                  <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.maxGenerations' })}</p>
                </div>
                <div className="text-center">
                  <p className="text-lg font-semibold tabular-nums text-stone-900 dark:text-stone-50">
                    {agent.observation_period_hours}h
                  </p>
                  <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.observationPeriod' })}</p>
                </div>
                <div className="text-center">
                  <p className="text-lg font-semibold tabular-nums text-stone-900 dark:text-stone-50">
                    {agent.max_silence_hours}h
                  </p>
                  <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.maxSilence' })}</p>
                </div>
              </div>
            </Card>
          ))}
        </div>
      )}

      {/* SOUL.md evolution history */}
      {!loading && agents.length > 0 && (
        <Section
          title={
            <span className="flex items-center gap-2">
              <GitBranch className="h-4 w-4 text-amber-600 dark:text-amber-400" />
              {intl.formatMessage({ id: 'evolution.engine' })}
            </span>
          }
        >
          {versions.length === 0 ? (
            <Card>
              <EmptyState
                icon={GitBranch}
                title={intl.formatMessage({ id: 'evolution.noHistory' })}
              />
            </Card>
          ) : (
            <div className="space-y-2">
              {versions.map((v) => (
                <EvolutionVersionCard key={v.version_id} version={v} />
              ))}
            </div>
          )}
        </Section>
      )}
    </div>
  );
}

function EvolutionVersionCard({ version }: { version: EvolutionVersion }) {
  const intl = useIntl();

  const statusLabel = (() => {
    switch (version.status) {
      case 'Confirmed': return intl.formatMessage({ id: 'evolution.status.confirmed' });
      case 'RolledBack': return intl.formatMessage({ id: 'evolution.status.rolledBack' });
      case 'Observing': return intl.formatMessage({ id: 'evolution.status.observing' });
      default: return version.status;
    }
  })();
  const statusTone: Record<string, 'success' | 'danger' | 'warning'> = {
    Confirmed: 'success',
    RolledBack: 'danger',
    Observing: 'warning',
  };

  const renderDelta = (pre: number, post: number | undefined, invert = false) => {
    if (post === undefined || post === null) return (
      <span className="text-stone-400">{pre.toFixed(2)}</span>
    );
    const delta = post - pre;
    const good = invert ? delta < 0 : delta > 0;
    const color = Math.abs(delta) < 1e-6
      ? 'text-stone-500'
      : good ? 'text-emerald-600 dark:text-emerald-400' : 'text-rose-600 dark:text-rose-400';
    return (
      <span>
        <span className="text-stone-500">{pre.toFixed(2)}</span>
        <span className="mx-1 text-stone-400">→</span>
        <span className={color}>{post.toFixed(2)}</span>
      </span>
    );
  };

  return (
    <Card bodyClassName="p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium text-amber-600 dark:text-amber-400">
            {version.agent_id}
          </span>
          <Badge tone={statusTone[version.status] ?? 'neutral'}>
            {statusLabel}
          </Badge>
          <code className="text-[10px] text-stone-400">{version.soul_hash.slice(0, 8)}</code>
        </div>
        <span className="flex items-center gap-1 text-xs text-stone-400">
          <Clock className="h-3 w-3" />
          {new Date(version.applied_at).toLocaleString()}
        </span>
      </div>
      {version.soul_summary && (
        <p className="mt-2 text-sm text-stone-700 dark:text-stone-300 whitespace-pre-wrap">
          {version.soul_summary}
        </p>
      )}
      <div className="mt-3 grid grid-cols-3 gap-2 border-t border-[var(--panel-border)] pt-3 text-xs">
        <div>
          <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.metric.feedback' })}</p>
          <p className="font-mono">
            {renderDelta(version.pre_metrics.positive_feedback_ratio, version.post_metrics?.positive_feedback_ratio)}
          </p>
        </div>
        <div>
          <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.metric.error' })}</p>
          <p className="font-mono">
            {renderDelta(version.pre_metrics.prediction_error, version.post_metrics?.prediction_error, true)}
          </p>
        </div>
        <div>
          <p className="text-[10px] text-stone-400">{intl.formatMessage({ id: 'evolution.metric.corrections' })}</p>
          <p className="font-mono">
            {renderDelta(version.pre_metrics.user_correction_rate, version.post_metrics?.user_correction_rate, true)}
          </p>
        </div>
      </div>
    </Card>
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

function InsightsTab() {
  const intl = useIntl();
  const [facts, setFacts] = useState<ReadonlyArray<KeyFactEntry>>([]);
  const [loading, setLoading] = useState(false);
  const [agents, setAgents] = useState<ReadonlyArray<{ name: string; display_name: string }>>([]);
  const [selectedAgent, setSelectedAgent] = useState('');

  // Load agent list on mount (mirrors MemoriesTab to keep UX consistent).
  useEffect(() => {
    api.agents.list().then((res) => {
      const list = res?.agents ?? [];
      setAgents(list);
      if (list.length > 0 && !selectedAgent) {
        setSelectedAgent(list[0].name);
      }
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
    // Run once on mount; see MemoriesTab for rationale.
  }, []);

  useEffect(() => {
    if (!selectedAgent) return;
    setLoading(true);
    api.memory.keyFacts(selectedAgent, 50).then((res) => {
      setFacts(res?.entries ?? []);
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setFacts([]);
    }).finally(() => setLoading(false));
  }, [selectedAgent, intl]);

  return (
    <div className="space-y-4">
      {/* Agent selector */}
      <Toolbar>
        <select
          value={selectedAgent}
          onChange={(e) => setSelectedAgent(e.target.value)}
          className={cn(controlClass, 'w-auto')}
        >
          {agents.map((a) => (
            <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
          ))}
        </select>
      </Toolbar>

      {/* Insight cards */}
      {loading ? (
        <div className="py-12 text-center text-stone-400">
          {intl.formatMessage({ id: 'common.loading' })}
        </div>
      ) : facts.length === 0 ? (
        <Card>
          <EmptyState
            icon={Lightbulb}
            title={intl.formatMessage({ id: 'memory.empty.insights' })}
          />
        </Card>
      ) : (
        <div className="space-y-3">
          {facts.map((fact) => (
            <Card key={fact.id}>
              <div className="mb-2 flex items-start justify-between gap-3">
                <div className="flex items-center gap-2">
                  <Lightbulb className="h-4 w-4 text-amber-500 dark:text-amber-400" />
                  <span className="text-xs font-medium text-amber-600 dark:text-amber-400">
                    {fact.agent_id}
                  </span>
                  {fact.access_count > 0 && (
                    <Badge tone="accent">
                      {intl.formatMessage(
                        { id: 'memory.insights.accessCount' },
                        { count: fact.access_count },
                      )}
                    </Badge>
                  )}
                </div>
                <span className="flex shrink-0 items-center gap-1 text-xs text-stone-400 dark:text-stone-500">
                  <Clock className="h-3 w-3" />
                  {new Date(fact.timestamp).toLocaleString()}
                </span>
              </div>
              <p className="text-sm text-stone-700 dark:text-stone-300 whitespace-pre-wrap">
                {fact.fact}
              </p>
              {(fact.source_session || fact.channel || fact.chat_id) && (
                <div className="mt-3 flex flex-wrap gap-x-3 gap-y-1 border-t border-[var(--panel-border)] pt-2 text-[11px] text-stone-400 dark:text-stone-500">
                  {fact.source_session && (
                    <span>session: <code className="text-stone-500 dark:text-stone-400">{fact.source_session}</code></span>
                  )}
                  {fact.channel && (
                    <span>channel: <code className="text-stone-500 dark:text-stone-400">{fact.channel}</code></span>
                  )}
                  {fact.chat_id && (
                    <span>chat: <code className="text-stone-500 dark:text-stone-400">{fact.chat_id}</code></span>
                  )}
                </div>
              )}
            </Card>
          ))}
        </div>
      )}
    </div>
  );
}
