import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Pencil, Plus, Trash2 } from 'lucide-react';
import { api } from '@/lib/api';
import type { TickSourceConfig } from '@/lib/config-rpc-types';
import { toast, formatError } from '@/lib/toast';
import { normalizeRestartRequired, tomlBool, tomlNum, tomlStr } from '@/lib/config-toml';
import { useRestartRequiredStore } from '@/stores/restart-required-store';
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  ErrorState,
  Input,
  SettingsCard,
  SettingsRow,
  SettingsSection,
  Switch,
  Textarea,
} from '@/components/mds';
import { ConfirmDialog, type SelectOption } from '@/components/settings/controls';
import { RowNumber, RowSelect, RowSwitch } from '@/pages/agent-form/form-rows';
import { useConfigForm, vBool, vNum, vStr } from '../useConfigForm';
import {
  KIND_FIELDS,
  TICK_KINDS,
  draftFromConfig,
  draftToUpsert,
  emptyDraft,
  type TickSourceDraft,
} from './tickSourceForm';

const TICK_PRESETS = ['', 'conservative', 'aggressive'] as const;

/**
 * 設定 → 自動化: 常駐感知 (`config.toml [tick]` + `[[tick.sources]]`), v1.68 W2.
 *
 * The tick layer is built once at gateway start, so every change here needs a
 * restart — the RPCs say so with `restart_required` and the persistent banner
 * plus an inline notice repeat it. The live status of running sources stays in
 * the read-only 即時監控來源 card on the 自動化規則 tab.
 */
export function TickSettingsSection() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const addRestart = useRestartRequiredStore((s) => s.add);
  // null = nothing saved yet this visit; [] = saved and already running.
  const [restartKeys, setRestartKeys] = useState<string[] | null>(null);
  const [hotReloadAvailable, setHotReloadAvailable] = useState<boolean | null>(null);

  const form = useConfigForm((tables) => ({
    'tick.enabled': tomlBool(tables, 'tick', 'enabled', false),
    'tick.preset': tomlStr(tables, 'tick', 'preset', ''),
    'tick.allow_command_sources': tomlBool(tables, 'tick', 'allow_command_sources', false),
    'tick.dns_ttl_secs': tomlNum(tables, 'tick', 'dns_ttl_secs', 60),
  }));
  const { values, set } = form;

  const presetOptions: SelectOption[] = TICK_PRESETS.map((p) => ({
    value: p,
    label: t(`settings.tick.preset.${p === '' ? 'none' : p}`),
    raw: p,
  }));

  const saveGlobals = async () => {
    const restart = await form.save();
    // The gateway respawns the tick sources itself when it can and lists
    // `tick` in `restart_required` when it cannot.
    if (restart !== null) setRestartKeys(restart.filter((k) => k === 'tick' || k.startsWith('tick.')));
  };

  // ── sources ──
  const [sources, setSources] = useState<TickSourceConfig[] | null>(null);
  const [sourcesError, setSourcesError] = useState<unknown>(null);
  const [draft, setDraft] = useState<TickSourceDraft | null>(null);
  const [editingExisting, setEditingExisting] = useState(false);
  const [draftError, setDraftError] = useState<string | null>(null);
  const [savingSource, setSavingSource] = useState(false);
  const [removeTarget, setRemoveTarget] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);

  const loadSources = useCallback(async () => {
    setSourcesError(null);
    try {
      const res = await api.tickSources.list();
      setSources(Array.isArray(res?.sources) ? res.sources : []);
      setHotReloadAvailable(typeof res?.hot_reload_available === 'boolean' ? res.hot_reload_available : null);
    } catch (e) {
      setSources(null);
      setSourcesError(e);
    }
  }, []);

  useEffect(() => {
    void loadSources();
  }, [loadSources]);

  /** `restart_required: true` ⇒ saved but only runs after a restart;
   *  `false` ⇒ the gateway already respawned the sources. */
  const afterWrite = (restart: unknown) => {
    const keys = normalizeRestartRequired(restart, 'tick.sources');
    if (keys.length > 0) addRestart(keys);
    setRestartKeys(keys);
    toast.success(t(keys.length > 0 ? 'settings.tick.savedRestart' : 'settings.tick.savedApplied'));
  };

  const onSaveDraft = async () => {
    if (!draft) return;
    const built = draftToUpsert(draft);
    if ('error' in built) {
      setDraftError(intl.formatMessage({ id: `settings.tick.source.error.${built.error}` }, { line: built.detail ?? '' }));
      return;
    }
    if (!editingExisting && (sources ?? []).some((s) => s.id === built.payload.id)) {
      setDraftError(t('settings.tick.source.error.duplicate'));
      return;
    }
    setSavingSource(true);
    setDraftError(null);
    try {
      const res = await api.tickSources.upsert(built.payload);
      afterWrite(res?.restart_required);
      setDraft(null);
      await loadSources();
    } catch (e) {
      setDraftError(formatError(e));
    } finally {
      setSavingSource(false);
    }
  };

  const onRemove = async (id: string) => {
    setRemoving(true);
    try {
      const res = await api.tickSources.remove(id);
      afterWrite(res?.restart_required);
      setRemoveTarget(null);
      await loadSources();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setRemoving(false);
    }
  };

  const commandBlocked = draft?.kind === 'command' && !vBool(values, 'tick.allow_command_sources');

  return (
    <SettingsSection title={t('settings.tick')} description={t('settings.tick.desc')}>
      {form.loadError != null && (
        <ErrorState
          variant="inline"
          error={form.loadError}
          title={t('errorState.manage.loadFailed')}
          onRetry={() => void form.load()}
        />
      )}
      {/* Driven by `restart_required`: shown when the last save could not be
          applied live, or when the gateway says it cannot respawn sources. */}
      {((restartKeys !== null && restartKeys.length > 0) || hotReloadAvailable === false) && (
        <p
          role="status"
          className="rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-xs text-foreground"
          data-testid="tick-restart-notice"
        >
          {t('settings.tick.restartPending')}
        </p>
      )}
      <SettingsCard>
        <RowSwitch
          label={t('settings.tick.enabled')}
          description={t('settings.tick.enabled.help')}
          checked={vBool(values, 'tick.enabled')}
          onChange={(v) => set('tick.enabled', v)}
        />
        <RowSelect
          label={t('settings.tick.preset')}
          description={t('settings.tick.preset.help')}
          value={vStr(values, 'tick.preset')}
          onChange={(v) => set('tick.preset', v)}
          options={presetOptions}
        />
        <RowSwitch
          label={t('settings.tick.allowCommand')}
          description={t('settings.tick.allowCommand.help')}
          checked={vBool(values, 'tick.allow_command_sources')}
          onChange={(v) => set('tick.allow_command_sources', v)}
        />
        <RowNumber
          label={t('settings.tick.dnsTtl')}
          description={t('settings.tick.dnsTtl.help')}
          value={vNum(values, 'tick.dns_ttl_secs', 60)}
          min={0}
          max={86400}
          onChange={(v) => set('tick.dns_ttl_secs', v)}
        />
      </SettingsCard>
      <div className="flex justify-end">
        <Button variant="brand" size="sm" onClick={() => void saveGlobals()} disabled={form.saving}>
          {form.saving ? t('common.saving') : t('settings.tick.save')}
        </Button>
      </div>

      <div className="space-y-2">
        <div className="flex items-center justify-between gap-2">
          <div>
            <div className="text-sm font-medium">{t('settings.tick.sources')}</div>
            <p className="text-xs text-muted-foreground">{t('settings.tick.sources.help')}</p>
          </div>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => {
              setDraft(emptyDraft());
              setEditingExisting(false);
              setDraftError(null);
            }}
            disabled={sources === null}
          >
            <Plus className="size-4" />
            {t('settings.tick.source.add')}
          </Button>
        </div>
        {sourcesError != null ? (
          <p className="rounded-md bg-secondary px-3 py-2 text-xs text-muted-foreground" role="status">
            {intl.formatMessage({ id: 'settings.tick.sources.unavailable' }, { message: formatError(sourcesError) })}
          </p>
        ) : sources !== null && sources.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t('settings.tick.sources.empty')}</p>
        ) : (
          <ul className="space-y-2">
            {(sources ?? []).map((s) => (
              <li
                key={s.id}
                className="flex items-center justify-between gap-3 rounded-lg border border-surface-border px-3 py-2"
              >
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-medium">{s.id}</span>
                    <Badge variant="outline">{t(`autopilot.monitor.kind.${s.kind}`)}</Badge>
                    {s.enabled === false && <Badge variant="ghost">{t('autopilot.monitor.status.disabled')}</Badge>}
                    {s.valid === false && (
                      <Badge variant="destructive" title={s.error ?? undefined}>
                        {t('settings.tick.source.invalid')}
                      </Badge>
                    )}
                    {(s.headers_count ?? 0) > 0 && (
                      <Badge variant="secondary">
                        {intl.formatMessage({ id: 'settings.tick.source.headersCount' }, { count: s.headers_count ?? 0 })}
                      </Badge>
                    )}
                  </div>
                  <p className="truncate text-xs text-muted-foreground">
                    {s.url ?? (Array.isArray(s.command) ? s.command.join(' ') : undefined) ?? s.path ?? ''}
                  </p>
                  {s.valid === false && s.error && (
                    <p className="text-xs text-destructive">{s.error}</p>
                  )}
                </div>
                <div className="flex shrink-0 gap-1">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={intl.formatMessage({ id: 'settings.tick.source.edit' }, { id: s.id })}
                    onClick={() => {
                      setDraft(draftFromConfig(s));
                      setEditingExisting(true);
                      setDraftError(null);
                    }}
                  >
                    <Pencil className="size-4" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={intl.formatMessage({ id: 'settings.tick.source.remove' }, { id: s.id })}
                    onClick={() => setRemoveTarget(s.id)}
                  >
                    <Trash2 className="size-4" />
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>

      <Dialog open={draft !== null} onOpenChange={(open) => { if (!open) setDraft(null); }}>
        <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>
              {editingExisting ? t('settings.tick.source.editTitle') : t('settings.tick.source.addTitle')}
            </DialogTitle>
          </DialogHeader>
          {draft && (
            <TickSourceFields
              draft={draft}
              onChange={setDraft}
              idLocked={editingExisting}
              commandBlocked={commandBlocked}
            />
          )}
          {draftError && (
            <p role="alert" className="text-xs text-destructive">
              {draftError}
            </p>
          )}
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setDraft(null)}>
              {t('common.cancel')}
            </Button>
            <Button variant="brand" size="sm" onClick={() => void onSaveDraft()} disabled={savingSource}>
              {savingSource ? t('common.saving') : t('common.save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={removeTarget !== null}
        onClose={() => setRemoveTarget(null)}
        onConfirm={() => { if (removeTarget) void onRemove(removeTarget); }}
        title={t('settings.tick.source.removeTitle')}
        message={removeTarget ? intl.formatMessage({ id: 'settings.tick.source.removeConfirm' }, { id: removeTarget }) : ''}
        confirmLabel={t('common.delete')}
        busy={removing}
      />
    </SettingsSection>
  );
}

function TickSourceFields({
  draft,
  onChange,
  idLocked,
  commandBlocked,
}: {
  draft: TickSourceDraft;
  onChange: (d: TickSourceDraft) => void;
  idLocked: boolean;
  commandBlocked: boolean;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const f = KIND_FIELDS[draft.kind];
  const patch = (p: Partial<TickSourceDraft>) => onChange({ ...draft, ...p });
  const kindOptions: SelectOption[] = TICK_KINDS.map((k) => ({
    value: k,
    label: t(`autopilot.monitor.kind.${k}`),
    raw: k,
  }));

  const text = (label: string, value: string, key: keyof TickSourceDraft, opts?: { placeholder?: string; help?: string; disabled?: boolean }) => (
    <SettingsRow label={label} description={opts?.help} tier="text">
      <Input
        type="text"
        value={value}
        disabled={opts?.disabled}
        placeholder={opts?.placeholder}
        aria-label={label}
        onChange={(e) => patch({ [key]: e.target.value } as Partial<TickSourceDraft>)}
      />
    </SettingsRow>
  );

  return (
    <div className="space-y-3">
      <SettingsCard>
        {text(t('settings.tick.source.id'), draft.id, 'id', {
          disabled: idLocked,
          placeholder: 'btc-price',
          help: t('settings.tick.source.id.help'),
        })}
        <RowSelect
          label={t('settings.tick.source.kind')}
          value={draft.kind}
          onChange={(v) => patch({ kind: v as TickSourceDraft['kind'] })}
          options={kindOptions}
        />
        <SettingsRow label={t('settings.tick.source.enabled')}>
          <Switch
            checked={draft.enabled}
            onCheckedChange={(v) => patch({ enabled: Boolean(v) })}
            aria-label={t('settings.tick.source.enabled')}
          />
        </SettingsRow>
        {f.url &&
          text(t('settings.tick.source.url'), draft.url, 'url', {
            placeholder: draft.kind === 'websocket' ? 'wss://example.com/feed' : 'https://example.com/api',
            help: draft.kind === 'websocket' ? t('settings.tick.source.url.wsHelp') : undefined,
          })}
        {f.path &&
          text(t('settings.tick.source.path'), draft.path, 'path', {
            help: t('settings.tick.source.path.help'),
          })}
        {f.interval &&
          text(t('settings.tick.source.interval'), draft.intervalSecs, 'intervalSecs', { placeholder: '60' })}
        {text(t('settings.tick.source.maxEvents'), draft.maxEventsPerMinute, 'maxEventsPerMinute', {
          help: t('settings.tick.source.maxEvents.help'),
        })}
        {text(t('settings.tick.source.baselineMaxAge'), draft.baselineMaxAgeSecs, 'baselineMaxAgeSecs', {
          help: t('settings.tick.source.baselineMaxAge.help'),
        })}
      </SettingsCard>
      {f.command && (
        <div className="space-y-1">
          <div className="text-sm font-medium">{t('settings.tick.source.command')}</div>
          <p className="text-xs text-muted-foreground">{t('settings.tick.source.command.help')}</p>
          <Textarea
            value={draft.command}
            rows={3}
            spellCheck={false}
            className="font-mono text-xs"
            aria-label={t('settings.tick.source.command')}
            onChange={(e) => patch({ command: e.target.value })}
          />
        </div>
      )}
      <div className="space-y-1">
        <div className="text-sm font-medium">{t('settings.tick.source.jsonFields')}</div>
        <p className="text-xs text-muted-foreground">{t('settings.tick.source.jsonFields.help')}</p>
        <Textarea
          value={draft.jsonFields}
          rows={3}
          spellCheck={false}
          className="font-mono text-xs"
          placeholder="price = /data/price"
          aria-label={t('settings.tick.source.jsonFields')}
          onChange={(e) => patch({ jsonFields: e.target.value })}
        />
      </div>
      {commandBlocked && (
        <p className="rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-xs">
          {t('settings.tick.source.commandBlocked')}
        </p>
      )}
      {f.subscribe && (
        <div className="space-y-1">
          <div className="text-sm font-medium">{t('settings.tick.source.subscribe')}</div>
          <p className="text-xs text-muted-foreground">{t('settings.tick.source.subscribe.help')}</p>
          <Textarea
            value={draft.subscribe}
            rows={3}
            spellCheck={false}
            className="font-mono text-xs"
            aria-label={t('settings.tick.source.subscribe')}
            onChange={(e) => patch({ subscribe: e.target.value })}
          />
        </div>
      )}
      {f.headers && (
        <div className="space-y-1">
          <div className="text-sm font-medium">{t('settings.tick.source.headers')}</div>
          <p className="text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'settings.tick.source.headers.help' }, { count: draft.headersCount })}
          </p>
          <Textarea
            value={draft.headersText}
            rows={3}
            spellCheck={false}
            autoComplete="off"
            disabled={draft.clearHeaders}
            className="font-mono text-xs"
            placeholder="Authorization: secret://env/FEED_TOKEN"
            aria-label={t('settings.tick.source.headers')}
            onChange={(e) => patch({ headersText: e.target.value })}
          />
          {draft.headersCount > 0 && (
            <label className="flex items-center gap-2 text-xs">
              <Switch
                checked={draft.clearHeaders}
                onCheckedChange={(v) => patch({ clearHeaders: Boolean(v) })}
                aria-label={t('settings.tick.source.headers.clear')}
              />
              {t('settings.tick.source.headers.clear')}
            </label>
          )}
        </div>
      )}
    </div>
  );
}
