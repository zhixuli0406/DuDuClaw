import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, RefreshCw } from 'lucide-react';
import { api } from '@/lib/api';
import type { ConfigRawFile } from '@/lib/config-rpc-types';
import { formatError, toast } from '@/lib/toast';
import { normalizeRestartRequired } from '@/lib/config-toml';
import { useAgentsStore } from '@/stores/agents-store';
import { useRestartRequiredStore } from '@/stores/restart-required-store';
import { Button, SettingsCard, SettingsSection, Textarea } from '@/components/mds';
import { ConfirmDialog, type SelectOption } from '@/components/settings/controls';
import { RowSelect } from '@/pages/agent-form/form-rows';

/** What the gateway writes in place of a stored secret in `config.raw.get`. */
export const RAW_SECRET_MASK = '«set»';

/** Pull a line/column out of a `config.raw.set` rejection. The gateway may
 *  answer with `{ message, line, column }` or with a TOML parser string such
 *  as "TOML parse error at line 3, column 7". */
export function parseRawError(err: unknown): { line?: number; column?: number; message: string } {
  if (err && typeof err === 'object' && !(err instanceof Error)) {
    const o = err as Record<string, unknown>;
    const message = typeof o.message === 'string' ? o.message : JSON.stringify(o);
    const line = typeof o.line === 'number' ? o.line : undefined;
    const column = typeof o.column === 'number' ? o.column : undefined;
    if (line !== undefined) return { line, column, message };
    return { ...parseRawError(message), message };
  }
  const message = err instanceof Error ? err.message : String(err ?? '');
  const m = message.match(/line\s+(\d+)(?:\s*,\s*column\s+(\d+))?/i) ?? message.match(/第\s*(\d+)\s*行(?:.*?第\s*(\d+)\s*欄)?/);
  return {
    line: m ? Number(m[1]) : undefined,
    column: m && m[2] ? Number(m[2]) : undefined,
    message,
  };
}

function isUnknownMethod(err: unknown): boolean {
  const text = err instanceof Error ? err.message : typeof err === 'string' ? err : JSON.stringify(err ?? '');
  return /unknown method|method not found|not supported/i.test(text);
}

/**
 * 設定 → 設定檔進階編輯 (v1.68 W2, admin). Edits config.toml, inference.toml or
 * one employee's agent.toml as raw TOML through `config.raw.get/set`. Secrets
 * come back masked as «set»; leaving a mask untouched keeps the stored value.
 * The gateway validates the whole file before writing, keeps a backup and
 * reports which keys need a restart.
 */
export function ConfigRawTab() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const { agents, fetchAgents } = useAgentsStore();
  const addRestart = useRestartRequiredStore((s) => s.add);

  const [file, setFile] = useState<ConfigRawFile>('config');
  const [pendingFile, setPendingFile] = useState<ConfigRawFile | null>(null);
  const [loadedText, setLoadedText] = useState<string | null>(null);
  // `config.raw.get` hash, sent back as `base_hash` so a file changed by
  // someone else since it was opened is refused instead of overwritten.
  const [baseHash, setBaseHash] = useState<string | undefined>(undefined);
  const [hiddenSections, setHiddenSections] = useState<string[]>([]);
  const [text, setText] = useState('');
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [unsupported, setUnsupported] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<{ line?: number; column?: number; message: string } | null>(null);
  const [result, setResult] = useState<{ restart: string[]; backup?: string } | null>(null);

  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);

  const load = useCallback(async (target: ConfigRawFile) => {
    setLoading(true);
    setLoadError(null);
    setSaveError(null);
    setResult(null);
    try {
      const res = await api.configRaw.get(target);
      const content = typeof res?.content === 'string' ? res.content : '';
      setLoadedText(content);
      setText(content);
      setBaseHash(typeof res?.hash === 'string' ? res.hash : undefined);
      setHiddenSections(Array.isArray(res?.hidden_sections) ? res.hidden_sections : []);
      setUnsupported(false);
    } catch (e) {
      setLoadedText(null);
      setText('');
      if (isUnknownMethod(e)) setUnsupported(true);
      else setLoadError(formatError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load(file);
  }, [file, load]);

  const dirty = loadedText !== null && text !== loadedText;

  const requestFile = (next: string) => {
    const target = next as ConfigRawFile;
    if (target === file) return;
    if (dirty) setPendingFile(target);
    else setFile(target);
  };

  const onSave = async () => {
    if (!dirty) {
      toast.info(t('settings.noChanges'));
      return;
    }
    setSaving(true);
    setSaveError(null);
    setResult(null);
    try {
      const res = await api.configRaw.set(file, text, baseHash);
      const restart = normalizeRestartRequired(res?.restart_required, file);
      if (restart.length > 0) addRestart(restart);
      setResult({ restart, backup: typeof res?.backup === 'string' ? res.backup : undefined });
      toast.success(t('settings.configRaw.saved'));
      // Re-read so the editor shows what is on disk (masks re-applied).
      const fresh = await api.configRaw.get(file).catch(() => null);
      if (fresh && typeof fresh.content === 'string') {
        setLoadedText(fresh.content);
        setText(fresh.content);
        setBaseHash(typeof fresh.hash === 'string' ? fresh.hash : undefined);
      } else {
        setLoadedText(text);
      }
    } catch (e) {
      setSaveError(parseRawError(e));
    } finally {
      setSaving(false);
    }
  };

  const fileOptions: SelectOption[] = [
    { value: 'config', label: t('settings.configRaw.file.config'), raw: 'config' },
    { value: 'inference', label: t('settings.configRaw.file.inference'), raw: 'inference' },
    ...agents.map((a) => ({
      value: `agent:${a.name}`,
      label: intl.formatMessage({ id: 'settings.configRaw.file.agent' }, { name: a.display_name || a.name }),
      raw: `agent:${a.name}`,
    })),
  ];

  return (
    <div className="space-y-6">
      <SettingsSection>
        <SettingsCard>
          <RowSelect
            label={t('settings.configRaw.file')}
            description={t('settings.configRaw.file.help')}
            value={file}
            onChange={requestFile}
            options={fileOptions}
          />
        </SettingsCard>
        <ul className="list-disc space-y-1 rounded-md bg-secondary px-6 py-2.5 text-xs text-muted-foreground">
          <li>{intl.formatMessage({ id: 'settings.configRaw.maskNote' }, { mask: RAW_SECRET_MASK })}</li>
          <li>{t('settings.configRaw.validateNote')}</li>
          <li>{t('settings.configRaw.backupNote')}</li>
          {hiddenSections.length > 0 && (
            <li>{intl.formatMessage({ id: 'settings.configRaw.hiddenNote' }, { sections: hiddenSections.join('、') })}</li>
          )}
        </ul>
      </SettingsSection>

      {unsupported ? (
        <p role="status" className="rounded-md border border-border px-3 py-2.5 text-sm text-muted-foreground">
          {t('settings.configRaw.unsupported')}
        </p>
      ) : loadError ? (
        <div role="alert" className="flex items-start gap-2 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2.5 text-sm text-destructive">
          <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
          <div className="space-y-1">
            <p>{intl.formatMessage({ id: 'settings.configRaw.loadFailed' }, { message: loadError })}</p>
            <Button variant="outline" size="sm" onClick={() => void load(file)}>
              <RefreshCw className="size-4" />
              {t('settings.configRaw.retry')}
            </Button>
          </div>
        </div>
      ) : (
        <>
          <Textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            rows={28}
            spellCheck={false}
            autoComplete="off"
            disabled={loading}
            aria-label={t('settings.configRaw.editor')}
            className="font-mono text-xs leading-5"
            data-testid="config-raw-editor"
          />
          {saveError && (
            <div role="alert" className="rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2.5 text-sm text-destructive">
              <p className="font-medium">
                {saveError.line !== undefined
                  ? intl.formatMessage(
                      { id: 'settings.configRaw.errorAt' },
                      { line: saveError.line, column: saveError.column ?? 1 },
                    )
                  : t('settings.configRaw.rejected')}
              </p>
              <p className="break-words text-xs">{saveError.message}</p>
            </div>
          )}
          {result && (
            <div role="status" className="space-y-1 rounded-lg border border-border bg-muted/40 px-3 py-2.5 text-sm">
              <p>
                {result.restart.length > 0
                  ? intl.formatMessage({ id: 'settings.restartRequired.title' }, { keys: result.restart.join('、') })
                  : t('settings.configRaw.noRestart')}
              </p>
              {result.backup && (
                <p className="text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'settings.configRaw.backupWritten' }, { path: result.backup })}
                </p>
              )}
            </div>
          )}
          <div className="flex items-center justify-end gap-2">
            <Button variant="outline" size="sm" onClick={() => loadedText !== null && setText(loadedText)} disabled={!dirty || saving}>
              {t('settings.configRaw.revert')}
            </Button>
            <Button variant="brand" size="sm" onClick={() => void onSave()} disabled={saving || loading || !dirty}>
              {saving ? t('common.saving') : t('settings.configRaw.save')}
            </Button>
          </div>
        </>
      )}

      <ConfirmDialog
        open={pendingFile !== null}
        onClose={() => setPendingFile(null)}
        onConfirm={() => {
          if (pendingFile) setFile(pendingFile);
          setPendingFile(null);
        }}
        title={t('settings.configRaw.discardTitle')}
        message={t('settings.configRaw.discardMessage')}
        confirmLabel={t('settings.configRaw.discard')}
      />
    </div>
  );
}
