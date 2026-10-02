import { useEffect, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type AutopilotRule, type AutopilotHistoryEntry } from '@/lib/api';
import { ruleActionTarget } from '@/lib/autopilot-rules';
import { toast, formatError } from '@/lib/toast';
import {
  Card,
  Button,
  Badge,
  Empty,
  ErrorState,
  Switch,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';
import { Plus, Clock, XCircle, Workflow, Pencil } from 'lucide-react';
import { TickSourcesCard } from './TickSourcesCard';
import { AutopilotRuleDialog, useActionLabel, useTriggerLabel } from './AutopilotRuleDialog';

// ── Autopilot Tab ───────────────────────────────────────────

export function AutopilotTab() {
  const intl = useIntl();
  const [rules, setRules] = useState<AutopilotRule[]>([]);
  const [loading, setLoading] = useState(true);
  // P05: `setRules([])` on failure made a broken fetch look like "no rules
  // configured" — the one reading a user is most likely to act on wrongly
  // (they go create a duplicate rule). Three separate error slots: the list,
  // the last row action, and the history dialog.
  const [loadError, setLoadError] = useState<unknown>(null);
  const [actionError, setActionError] = useState<unknown>(null);
  const [historyError, setHistoryError] = useState<unknown>(null);
  const [showCreate, setShowCreate] = useState(false);
  const [editRule, setEditRule] = useState<AutopilotRule | null>(null);
  const triggerLabel = useTriggerLabel();
  const actionLabel = useActionLabel();
  const [historyRuleId, setHistoryRuleId] = useState<string | null>(null);
  const [historyEntries, setHistoryEntries] = useState<AutopilotHistoryEntry[]>([]);
  const [removeTarget, setRemoveTarget] = useState<{ id: string; name: string } | null>(null);

  const fetchRules = useCallback(async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const result = await api.autopilot.list();
      setRules(result?.rules ?? []);
    } catch (e) {
      setRules([]);
      setLoadError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => { fetchRules(); }, [fetchRules]);

  const handleToggle = useCallback(async (ruleId: string, enabled: boolean) => {
    setRules((prev) => prev.map((r) => (r.id === ruleId ? { ...r, enabled } : r)));
    setActionError(null);
    try {
      await api.autopilot.update(ruleId, { enabled });
    } catch (e) {
      setRules((prev) => prev.map((r) => (r.id === ruleId ? { ...r, enabled: !enabled } : r)));
      setActionError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  const handleRemove = useCallback(async () => {
    if (!removeTarget) return;
    setActionError(null);
    try {
      await api.autopilot.remove(removeTarget.id);
      setRules((prev) => prev.filter((r) => r.id !== removeTarget.id));
    } catch (e) {
      setActionError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
    }
    setRemoveTarget(null);
  }, [removeTarget, intl]);

  const handleViewHistory = useCallback(async (ruleId: string) => {
    setHistoryRuleId(ruleId);
    setHistoryError(null);
    try {
      const result = await api.autopilot.history(ruleId);
      setHistoryEntries(result?.entries ?? []);
    } catch (e) {
      setHistoryEntries([]);
      setHistoryError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  return (
    <div className="space-y-6">
      <TickSourcesCard />

      <div className="flex items-center justify-between gap-2">
        <p className="text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'autopilot.subtitle' })}
        </p>
        <Button variant="brand" size="sm" onClick={() => setShowCreate(true)}>
          <Plus />
          {intl.formatMessage({ id: 'autopilot.create' })}
        </Button>
      </div>

      {actionError != null && (
        <ErrorState
          variant="inline"
          error={actionError}
          title={intl.formatMessage({ id: 'errorState.manage.actionFailed' })}
          onRetry={() => { setActionError(null); void fetchRules(); }}
          retryLabel={intl.formatMessage({ id: 'errorState.manage.reload' })}
        />
      )}

      {loading ? (
        <p className="py-12 text-center text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'common.loading' })}
        </p>
      ) : loadError != null ? (
        <ErrorState
          error={loadError}
          title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
          description={intl.formatMessage({ id: 'errorState.manage.notEmptyHint' })}
          onRetry={() => void fetchRules()}
        />
      ) : rules.length === 0 ? (
        <Empty
          icon={Workflow}
          variant="dashed"
          title={intl.formatMessage({ id: 'autopilot.empty' })}
        />
      ) : (
        <div className="space-y-3">
          {rules.map((rule) => (
            <Card key={rule.id} className="gap-3 p-5">
              <div className="flex items-start justify-between">
                <div className="flex items-center gap-3">
                  <Switch
                    checked={rule.enabled}
                    onCheckedChange={(v) => handleToggle(rule.id, Boolean(v))}
                    aria-label={rule.name}
                  />
                  <div>
                    <h4 className="font-medium text-foreground">{rule.name}</h4>
                    <div className="mt-1 flex items-center gap-2 text-xs text-muted-foreground">
                      <Badge variant="secondary">
                        {triggerLabel(rule.trigger_event)}
                      </Badge>
                      <span aria-hidden>→</span>
                      <Badge variant="outline">
                        {actionLabel(rule.action?.type ?? '')}
                      </Badge>
                      {ruleActionTarget(rule.action) && (
                        <span className="text-muted-foreground">({ruleActionTarget(rule.action)})</span>
                      )}
                    </div>
                  </div>
                </div>
                <div className="flex items-center gap-1">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onClick={() => setEditRule(rule)}
                    title={intl.formatMessage({ id: 'autopilot.edit' })}
                    aria-label={intl.formatMessage({ id: 'autopilot.edit' })}
                  >
                    <Pencil />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onClick={() => handleViewHistory(rule.id)}
                    title={intl.formatMessage({ id: 'autopilot.history' })}
                    aria-label={intl.formatMessage({ id: 'autopilot.history' })}
                  >
                    <Clock />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={intl.formatMessage({ id: 'autopilot.remove' })}
                    onClick={() => setRemoveTarget({ id: rule.id, name: rule.name })}
                    className="text-muted-foreground hover:text-destructive"
                  >
                    <XCircle />
                  </Button>
                </div>
              </div>

              <div className="flex items-center gap-4 text-xs text-muted-foreground">
                <span>{intl.formatMessage({ id: 'autopilot.triggerCount' }, { count: rule.trigger_count })}</span>
                {rule.last_triggered_at && (
                  <span>
                    {intl.formatMessage({ id: 'autopilot.lastTriggered' })}: {new Date(rule.last_triggered_at).toLocaleString('zh-TW')}
                  </span>
                )}
              </div>
            </Card>
          ))}
        </div>
      )}

      {/* Create / Edit Rule Dialog */}
      {showCreate && (
        <AutopilotRuleDialog
          onClose={() => setShowCreate(false)}
          onSaved={() => { setShowCreate(false); fetchRules(); }}
        />
      )}
      {editRule && (
        <AutopilotRuleDialog
          rule={editRule}
          onClose={() => setEditRule(null)}
          onSaved={() => { setEditRule(null); fetchRules(); }}
        />
      )}

      {/* History Dialog */}
      {historyRuleId && (
        <AutopilotHistoryDialog
          entries={historyEntries}
          error={historyError}
          onRetry={() => void handleViewHistory(historyRuleId)}
          onClose={() => { setHistoryRuleId(null); setHistoryError(null); }}
        />
      )}

      {/* Remove Confirmation */}
      <ConfirmDialog
        open={!!removeTarget}
        onClose={() => setRemoveTarget(null)}
        onConfirm={handleRemove}
        title={intl.formatMessage({ id: 'autopilot.remove' })}
        message={removeTarget ? intl.formatMessage({ id: 'autopilot.remove.confirm' }, { name: removeTarget.name }) : ''}
        confirmLabel={intl.formatMessage({ id: 'autopilot.remove' })}
      />
    </div>
  );
}

function AutopilotHistoryDialog({
  entries,
  error,
  onRetry,
  onClose,
}: {
  entries: ReadonlyArray<AutopilotHistoryEntry>;
  /** Set when the history fetch failed — never rendered as "no history". */
  error?: unknown;
  onRetry?: () => void;
  onClose: () => void;
}) {
  const intl = useIntl();
  return (
    <Dialog open onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'autopilot.history' })}</DialogTitle>
        </DialogHeader>
        <div className="max-h-[400px] space-y-2 overflow-y-auto">
          {error != null ? (
            <ErrorState
              error={error}
              title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
              description={intl.formatMessage({ id: 'errorState.manage.notEmptyHint' })}
              onRetry={onRetry}
            />
          ) : entries.length === 0 ? (
            <p className="py-8 text-center text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'autopilot.history.empty' })}
            </p>
          ) : (
            entries.map((entry) => (
              <div
                key={entry.id}
                className="flex items-center justify-between rounded-lg border border-surface-border px-4 py-3"
              >
                <div>
                  <span className="text-sm text-foreground">
                    {new Date(entry.triggered_at).toLocaleString('zh-TW')}
                  </span>
                  {entry.details && (
                    <p className="mt-0.5 text-xs text-muted-foreground">{entry.details}</p>
                  )}
                </div>
                <Badge
                  className={cn(
                    entry.result === 'success'
                      ? 'bg-success/10 text-success'
                      : 'bg-destructive/10 text-destructive',
                  )}
                >
                  {intl.formatMessage({ id: `autopilot.history.${entry.result}` })}
                </Badge>
              </div>
            ))
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
