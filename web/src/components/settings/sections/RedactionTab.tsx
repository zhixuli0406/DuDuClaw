import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  api,
  type RedactionConfig,
  type RedactionSourceMode,
  type RedactionSources,
  type RedactionRestoreArgs,
  type RedactionEgressRule,
  type RedactionUpdate,
} from '@/lib/api';
import { FormField, inputClass, selectClass } from '@/components/shared/Dialog';
import { ChipEditor } from '@/components/shared/ChipEditor';
import { toast, formatError } from '@/lib/toast';
import { Card, Button } from '@/components/ui';
import { EyeOff, Plus, Trash2 } from 'lucide-react';

// ── Privacy / Redaction Tab (RED) ──────────────────────────────

const REDACTION_SOURCE_KEYS: ReadonlyArray<keyof RedactionSources> = [
  'user_input',
  'tool_results',
  'system_prompt',
  'sub_agent',
  'cron_context',
];
const REDACTION_MODES: ReadonlyArray<RedactionSourceMode> = ['on', 'off', 'selective', 'inherit'];
const REDACTION_RESTORE: ReadonlyArray<RedactionRestoreArgs> = ['restore', 'passthrough', 'deny'];

export function RedactionTab() {
  const intl = useIntl();
  const [config, setConfig] = useState<RedactionConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [newTool, setNewTool] = useState('');
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  const load = useCallback(async () => {
    try {
      const res = await api.redaction.get();
      setConfig(res);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  useEffect(() => { load(); }, [load]);

  const handleSave = async () => {
    if (!config) return;
    setSaving(true);
    try {
      const payload: RedactionUpdate = {
        enabled: config.enabled,
        vault_ttl_hours: config.vault_ttl_hours,
        purge_after_expire_days: config.purge_after_expire_days,
        profiles: config.profiles,
        sources: config.sources,
        tool_egress: config.tool_egress,
      };
      await api.redaction.update(payload);
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const addEgress = () => {
    const tool = newTool.trim();
    if (!tool || !config || config.tool_egress[tool]) {
      setNewTool('');
      return;
    }
    setConfig({ ...config, tool_egress: { ...config.tool_egress, [tool]: { restore_args: 'deny', audit_reveal: false } } });
    setNewTool('');
  };

  const removeEgress = (tool: string) => {
    if (!config) return;
    const next = { ...config.tool_egress };
    delete next[tool];
    setConfig({ ...config, tool_egress: next });
  };

  if (!config) {
    return (
      <Card>
        <p className="py-8 text-center text-sm text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</p>
      </Card>
    );
  }

  return (
    <Card
      bodyClassName="space-y-6"
      title={
        <span className="flex items-center gap-2">
          <EyeOff className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'settings.redaction' })}
        </span>
      }
    >
      <p className="text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'redaction.desc' })}</p>

      {/* Master toggle + scalars */}
      <div className="space-y-4">
        <label className="flex items-center justify-between py-1.5">
          <span className="text-sm text-stone-700 dark:text-stone-300">{intl.formatMessage({ id: 'redaction.enabled' })}</span>
          <input type="checkbox" checked={config.enabled} onChange={(e) => setConfig({ ...config, enabled: e.target.checked })} className="h-4 w-4 accent-amber-500" />
        </label>
        <div className="grid gap-4 sm:grid-cols-2">
          <FormField label={intl.formatMessage({ id: 'redaction.vaultTtl' })} hint="1-8760">
            <input type="number" min={1} max={8760} value={config.vault_ttl_hours} onChange={(e) => setConfig({ ...config, vault_ttl_hours: Number(e.target.value) })} className={inputClass} />
          </FormField>
          <FormField label={intl.formatMessage({ id: 'redaction.purgeAfter' })} hint="0-3650">
            <input type="number" min={0} max={3650} value={config.purge_after_expire_days} onChange={(e) => setConfig({ ...config, purge_after_expire_days: Number(e.target.value) })} className={inputClass} />
          </FormField>
        </div>
        <FormField label={intl.formatMessage({ id: 'redaction.profiles' })} hint={intl.formatMessage({ id: 'redaction.profiles.hint' })}>
          <ChipEditor values={config.profiles} onChange={(v) => setConfig({ ...config, profiles: v })} placeholder="pii" addLabel={intl.formatMessage({ id: 'common.add' })} />
        </FormField>
      </div>

      {/* Sources matrix */}
      <div className="border-t border-[var(--panel-border)] pt-4">
        <h4 className="mb-3 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'redaction.sources' })}</h4>
        <div className="space-y-3">
          {REDACTION_SOURCE_KEYS.map((key) => (
            <div key={key} className="flex items-center justify-between gap-3">
              <span className="text-sm text-stone-700 dark:text-stone-300">{intl.formatMessage({ id: `redaction.source.${key}` })}</span>
              <select
                value={config.sources[key]}
                onChange={(e) => setConfig({ ...config, sources: { ...config.sources, [key]: e.target.value as RedactionSourceMode } })}
                className={cn(selectClass, 'w-40')}
              >
                {REDACTION_MODES.map((m) => (
                  <option key={m} value={m}>{intl.formatMessage({ id: `redaction.mode.${m}` })}</option>
                ))}
              </select>
            </div>
          ))}
        </div>
      </div>

      {/* Tool egress rules */}
      <div className="border-t border-[var(--panel-border)] pt-4">
        <h4 className="mb-3 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'redaction.toolEgress' })}</h4>
        <p className="mb-3 text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'redaction.toolEgress.hint' })}</p>
        <div className="space-y-2">
          {(Object.entries(config.tool_egress) as Array<[string, RedactionEgressRule]>).map(([tool, rule]) => (
            <div key={tool} className="flex flex-wrap items-center gap-2 rounded-lg bg-stone-500/5 p-2.5 dark:bg-white/5">
              <code className="rounded bg-stone-500/10 px-2 py-0.5 font-mono text-xs text-stone-700 dark:text-stone-300">{tool}</code>
              <select
                value={rule.restore_args}
                onChange={(e) => setConfig({ ...config, tool_egress: { ...config.tool_egress, [tool]: { ...rule, restore_args: e.target.value as RedactionRestoreArgs } } })}
                className={cn(selectClass, 'w-36')}
              >
                {REDACTION_RESTORE.map((r) => (
                  <option key={r} value={r}>{intl.formatMessage({ id: `redaction.restore.${r}` })}</option>
                ))}
              </select>
              <label className="flex items-center gap-1.5 text-xs text-stone-600 dark:text-stone-400">
                <input type="checkbox" checked={rule.audit_reveal} onChange={(e) => setConfig({ ...config, tool_egress: { ...config.tool_egress, [tool]: { ...rule, audit_reveal: e.target.checked } } })} className="accent-amber-500" />
                {intl.formatMessage({ id: 'redaction.auditReveal' })}
              </label>
              <button onClick={() => removeEgress(tool)} className="ml-auto rounded p-1 text-rose-500 hover:bg-rose-500/10">
                <Trash2 className="h-3.5 w-3.5" />
              </button>
            </div>
          ))}
          {Object.keys(config.tool_egress).length === 0 && (
            <p className="text-xs text-stone-400">{intl.formatMessage({ id: 'common.noData' })}</p>
          )}
        </div>
        <div className="mt-3 flex gap-2">
          <input type="text" value={newTool} onChange={(e) => setNewTool(e.target.value)} placeholder={intl.formatMessage({ id: 'redaction.toolEgress.toolName' })} className={cn(inputClass, 'flex-1')} />
          <Button variant="secondary" icon={Plus} onClick={addEgress}>
            {intl.formatMessage({ id: 'redaction.toolEgress.add' })}
          </Button>
        </div>
      </div>

      <div className="flex items-center justify-end gap-2 pt-2">
        {saved && <span className="text-xs text-emerald-600 dark:text-emerald-400">{intl.formatMessage({ id: 'settings.general.saved' })}</span>}
        <Button variant="primary" onClick={handleSave} disabled={saving}>
          {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
        </Button>
      </div>
    </Card>
  );
}
