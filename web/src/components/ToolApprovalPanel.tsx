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

  const inputClass = 'w-full rounded-lg border border-surface-border bg-muted px-3 py-1.5 text-sm text-foreground placeholder:text-muted-foreground focus:border-brand focus:outline-none focus:ring-1 focus:ring-brand';

  return (
    <div className="rounded-xl border border-surface-border bg-surface p-5">
      <div className="mb-4 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <ShieldCheck className="h-5 w-5 text-brand" />
          <h3 className="font-semibold text-foreground">
            {intl.formatMessage({ id: 'browser.approvals.title' })}
          </h3>
        </div>
        <button
          onClick={() => setShowForm(!showForm)}
          className="flex items-center gap-1 rounded-lg bg-brand/10 px-3 py-1.5 text-xs font-medium text-brand hover:bg-brand/20"
        >
          <Plus className="h-3 w-3" />
          {intl.formatMessage({ id: 'browser.approvals.add' })}
        </button>
      </div>

      {/* Add form */}
      {showForm && (
        <div className="mb-4 space-y-3 rounded-lg border border-brand/30 bg-brand/10 p-4">
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="mb-1 block text-xs font-medium text-muted-foreground">{intl.formatMessage({ id: 'browser.approvals.tool' })}</label>
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
              <label className="mb-1 block text-xs font-medium text-muted-foreground">{intl.formatMessage({ id: 'browser.approvals.agentId' })}</label>
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
              <label className="mb-1 block text-xs font-medium text-muted-foreground">{intl.formatMessage({ id: 'browser.approvals.duration' })}</label>
              <input
                type="number"
                value={formDuration}
                onChange={(e) => setFormDuration(e.target.value)}
                placeholder={intl.formatMessage({ id: 'browser.approvals.durationPlaceholder' })}
                className={inputClass}
              />
            </div>
            <div className="flex items-end gap-2">
              <label className="flex items-center gap-2 text-sm text-muted-foreground">
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
              className="rounded-lg px-3 py-1.5 text-xs text-muted-foreground hover:text-foreground"
            >
              {intl.formatMessage({ id: 'browser.approvals.cancel' })}
            </button>
            <button
              onClick={handleApprove}
              disabled={!formAgent.trim()}
              className="rounded-lg bg-brand px-4 py-1.5 text-xs font-medium text-brand-foreground hover:bg-brand/90 disabled:opacity-40"
            >
              {intl.formatMessage({ id: 'browser.approvals.approve' })}
            </button>
          </div>
        </div>
      )}

      {/* Approvals list */}
      {toolApprovals.length === 0 ? (
        <p className="py-6 text-center text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'browser.approvals.empty' })}
        </p>
      ) : (
        <div className="space-y-2">
          {toolApprovals.map((a, i) => (
            <div
              key={`${a.tool_name}-${a.agent_id}-${i}`}
              className="flex items-center gap-3 rounded-lg border border-surface-border bg-muted px-3 py-2"
            >
              <ShieldCheck className="h-4 w-4 text-success" />
              <span className="rounded-md bg-success/10 px-2 py-0.5 text-xs font-medium text-success">
                {a.tool_name}
              </span>
              <span className="text-sm text-foreground">{a.agent_id}</span>
              {a.session_scoped && (
                <span className="rounded-md bg-blue-100 px-1.5 py-0.5 text-xs text-blue-600 dark:bg-blue-900/30 dark:text-blue-400">
                  session
                </span>
              )}
              {a.duration_minutes && (
                <span className="flex items-center gap-1 text-xs text-muted-foreground">
                  <Clock className="h-3 w-3" />
                  {a.duration_minutes}m
                </span>
              )}
              <button
                onClick={() => revokeTool(a.tool_name, a.agent_id)}
                className="ml-auto rounded-md p-1 text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
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
