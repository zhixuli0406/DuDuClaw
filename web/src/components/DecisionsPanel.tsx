import { useCallback, useEffect, useState } from 'react';
import { api, type DecisionInfo } from '@/lib/api';
import { ListChecks, RefreshCw, XCircle } from 'lucide-react';
import { toast } from '@/lib/toast';

/**
 * RFC-24 Decision Continuity — Dashboard panel.
 *
 * Lists an agent's still-open decisions (proposals it offered the user that are
 * awaiting a choice) and lets an operator dismiss a wrongly-captured one as a
 * false positive (feeds the `decision_false_positive` precision metric).
 *
 * Self-contained with its own agent filter, mirroring BrowserAuditPanel — no
 * global store, since the data is read-on-demand per agent.
 */
export function DecisionsPanel() {
  const [agentId, setAgentId] = useState('');
  const [decisions, setDecisions] = useState<ReadonlyArray<DecisionInfo>>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async (agent: string) => {
    if (!agent.trim()) {
      setDecisions([]);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const res = await api.decisions.list(agent.trim());
      setDecisions(res?.decisions ?? []);
    } catch (e) {
      setError(String(e));
      setDecisions([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (agentId.trim()) void load(agentId);
  }, [agentId, load]);

  const dismiss = async (decisionId: string) => {
    try {
      await api.decisions.dismiss(agentId.trim(), decisionId);
      setDecisions((prev) => prev.filter((d) => d.id !== decisionId));
      toast.success('已標記為誤判並移除');
    } catch (e) {
      toast.error(`移除失敗：${String(e)}`);
    }
  };

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-700 dark:bg-stone-800/50">
      <div className="mb-4 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <ListChecks className="h-5 w-5 text-amber-500" />
          <h3 className="font-semibold text-stone-900 dark:text-stone-50">待決事項 (Open Decisions)</h3>
        </div>
        <button
          type="button"
          onClick={() => void load(agentId)}
          disabled={!agentId.trim() || loading}
          className="flex items-center gap-1 rounded-lg border border-stone-200 px-2 py-1 text-xs text-stone-600 hover:bg-stone-50 disabled:opacity-50 dark:border-stone-700 dark:text-stone-300 dark:hover:bg-stone-700/50"
        >
          <RefreshCw className={`h-3.5 w-3.5 ${loading ? 'animate-spin' : ''}`} />
          重新整理
        </button>
      </div>

      <input
        type="text"
        value={agentId}
        onChange={(e) => setAgentId(e.target.value)}
        placeholder="輸入 agent id 以查看其未決決策"
        className="mb-4 w-full rounded-lg border border-stone-200 bg-stone-50 px-3 py-2 text-sm text-stone-900 placeholder:text-stone-400 focus:border-amber-400 focus:outline-none dark:border-stone-700 dark:bg-stone-900 dark:text-stone-100"
      />

      {error && (
        <p className="mb-3 rounded-lg bg-rose-50 px-3 py-2 text-sm text-rose-600 dark:bg-rose-900/20 dark:text-rose-400">
          {error}
        </p>
      )}

      {!loading && agentId.trim() && decisions.length === 0 && !error && (
        <p className="py-6 text-center text-sm text-stone-400">此 agent 目前沒有未決決策。</p>
      )}

      <ul className="space-y-3">
        {decisions.map((d) => (
          <li
            key={d.id}
            className="rounded-lg border border-stone-200 p-3 dark:border-stone-700"
          >
            <div className="mb-2 flex items-start justify-between gap-2">
              <div>
                <p className="font-medium text-stone-900 dark:text-stone-50">{d.question}</p>
                <p className="font-mono text-[11px] text-stone-400">decision:{d.id}</p>
              </div>
              <button
                type="button"
                onClick={() => void dismiss(d.id)}
                title="標記為誤判 (false positive) 並移除"
                className="flex shrink-0 items-center gap-1 rounded-md px-2 py-1 text-xs text-stone-500 hover:bg-rose-50 hover:text-rose-600 dark:hover:bg-rose-900/20"
              >
                <XCircle className="h-3.5 w-3.5" />
                誤判
              </button>
            </div>
            <ul className="space-y-1">
              {d.options.map((o) => (
                <li key={o.key} className="text-sm text-stone-600 dark:text-stone-300">
                  <span className="mr-1 font-semibold text-amber-600 dark:text-amber-400">{o.key}</span>
                  {o.content}
                </li>
              ))}
            </ul>
          </li>
        ))}
      </ul>
    </div>
  );
}
