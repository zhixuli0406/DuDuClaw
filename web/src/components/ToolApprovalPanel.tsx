import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useBrowserStore } from '@/stores/browser-store';
import { ShieldCheck, Plus, Clock, X } from 'lucide-react';

export function ToolApprovalPanel() {
  const intl = useIntl();
  const { toolApprovals, fetchToolApprovals, approveTool, revokeTool } = useBrowserStore();
  const [showForm, setShowForm] = useState(false);
  const [formTool, setFormTool] = useState('browser');
  const [formAgent, setFormAgent] = useState('');
  const [formDuration, setFormDuration] = useState('');
  const [formSession, setFormSession] = useState(false);

  useEffect(() => {
    fetchToolApprovals();
  }, [fetchToolApprovals]);

  const handleApprove = () => {
    if (!formAgent.trim()) return;
    approveTool(
      formTool,
      formAgent.trim(),
      formDuration ? Number(formDuration) : undefined,
      formSession
    );
    setShowForm(false);
    setFormAgent('');
    setFormDuration('');
  };

  const inputClass = 'w-full rounded-lg border border-stone-300 bg-stone-50 px-3 py-1.5 text-sm text-stone-700 placeholder:text-stone-400 focus:border-amber-400 focus:outline-none focus:ring-1 focus:ring-amber-400 dark:border-stone-600 dark:bg-stone-700 dark:text-stone-200';

  return (
    <div className="rounded-xl border border-stone-200 bg-white p-5 dark:border-stone-700 dark:bg-stone-800/50">
      <div className="mb-4 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <ShieldCheck className="h-5 w-5 text-amber-500" />
          <h3 className="font-semibold text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'browser.approvals.title' })}
          </h3>
        </div>
        <button
          onClick={() => setShowForm(!showForm)}
          className="flex items-center gap-1 rounded-lg bg-amber-50 px-3 py-1.5 text-xs font-medium text-amber-700 hover:bg-amber-100 dark:bg-amber-900/20 dark:text-amber-400 dark:hover:bg-amber-900/40"
        >
          <Plus className="h-3 w-3" />
          {intl.formatMessage({ id: 'browser.approvals.add' })}
        </button>
      </div>

      {/* Add form */}
      {showForm && (
        <div className="mb-4 space-y-3 rounded-lg border border-amber-200 bg-amber-50/50 p-4 dark:border-amber-900/50 dark:bg-amber-900/10">
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="mb-1 block text-xs font-medium text-stone-600 dark:text-stone-400">{intl.formatMessage({ id: 'browser.approvals.tool' })}</label>
              <select
                value={formTool}
                onChange={(e) => setFormTool(e.target.value)}
                className={inputClass}
              >
                <option value="browser">Browser</option>
                <option value="computer_use">Computer Use</option>
                <option value="web_fetch">Web Fetch</option>
                <option value="web_extract">Web Extract</option>
              </select>
            </div>
            <div>
              <label className="mb-1 block text-xs font-medium text-stone-600 dark:text-stone-400">{intl.formatMessage({ id: 'browser.approvals.agentId' })}</label>
              <input
                type="text"
                value={formAgent}
                onChange={(e) => setFormAgent(e.target.value)}
                placeholder="agent-id"
                className={inputClass}
              />
            </div>
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="mb-1 block text-xs font-medium text-stone-600 dark:text-stone-400">{intl.formatMessage({ id: 'browser.approvals.duration' })}</label>
              <input
                type="number"
                value={formDuration}
                onChange={(e) => setFormDuration(e.target.value)}
                placeholder={intl.formatMessage({ id: 'browser.approvals.durationPlaceholder' })}
                className={inputClass}
              />
            </div>
            <div className="flex items-end gap-2">
              <label className="flex items-center gap-2 text-sm text-stone-600 dark:text-stone-400">
                <input
                  type="checkbox"
                  checked={formSession}
                  onChange={(e) => setFormSession(e.target.checked)}
                  className="rounded"
                />
                {intl.formatMessage({ id: 'browser.approvals.sessionScoped' })}
              </label>
            </div>
          </div>
          <div className="flex justify-end gap-2">
            <button
              onClick={() => setShowForm(false)}
              className="rounded-lg px-3 py-1.5 text-xs text-stone-500 hover:text-stone-700 dark:text-stone-400"
            >
              {intl.formatMessage({ id: 'browser.approvals.cancel' })}
            </button>
            <button
              onClick={handleApprove}
              disabled={!formAgent.trim()}
              className="rounded-lg bg-amber-500 px-4 py-1.5 text-xs font-medium text-white hover:bg-amber-600 disabled:opacity-40"
            >
              {intl.formatMessage({ id: 'browser.approvals.approve' })}
            </button>
          </div>
        </div>
      )}

      {/* Approvals list */}
      {toolApprovals.length === 0 ? (
        <p className="py-6 text-center text-sm text-stone-400">
          {intl.formatMessage({ id: 'browser.approvals.empty' })}
        </p>
      ) : (
        <div className="space-y-2">
          {toolApprovals.map((a, i) => (
            <div
              key={`${a.tool_name}-${a.agent_id}-${i}`}
              className="flex items-center gap-3 rounded-lg border border-stone-100 bg-stone-50/50 px-3 py-2 dark:border-stone-700 dark:bg-stone-800"
            >
              <ShieldCheck className="h-4 w-4 text-emerald-500" />
              <span className="rounded-md bg-emerald-100 px-2 py-0.5 text-xs font-medium text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400">
                {a.tool_name}
              </span>
              <span className="text-sm text-stone-700 dark:text-stone-200">{a.agent_id}</span>
              {a.session_scoped && (
                <span className="rounded-md bg-blue-100 px-1.5 py-0.5 text-xs text-blue-600 dark:bg-blue-900/30 dark:text-blue-400">
                  session
                </span>
              )}
              {a.duration_minutes && (
                <span className="flex items-center gap-1 text-xs text-stone-400">
                  <Clock className="h-3 w-3" />
                  {a.duration_minutes}m
                </span>
              )}
              <button
                onClick={() => revokeTool(a.tool_name, a.agent_id)}
                className="ml-auto rounded-md p-1 text-stone-400 hover:bg-rose-50 hover:text-rose-500 dark:hover:bg-rose-900/20"
                title={intl.formatMessage({ id: 'browser.approvals.revoke' })}
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
