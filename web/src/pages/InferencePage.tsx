import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  api,
  type InferenceConfig,
  type InferenceUpdate,
  type InferenceGeneration,
  type InferenceRouter,
  type InferenceOpenAiCompat,
  type InferenceLlamafile,
} from '@/lib/api';
import { ChipEditor } from '@/components/shared/ChipEditor';
import { AdvancedSection, type SelectOption } from '@/components/settings/controls';
import { toast, formatError, formatErrorDetail } from '@/lib/toast';
import {
  Button,
  Input,
  SettingsSection,
  SettingsCard,
  SettingsRow,
  SettingsSaveState,
  type SettingsSaveStatus,
  type SettingsRowTier,
} from '@/components/mds';
import { RowText, RowSecret, RowSwitch, RowSelect, FieldBlock } from '@/pages/agent-form/form-rows';
import { Cpu, Save, RefreshCw, Loader2, AlertTriangle } from 'lucide-react';

/** v1.68 (W2): read `[llamafile]` into the typed form, dropping anything of
 *  the wrong type instead of guessing. */
export function llamafileFromConfig(section: unknown): InferenceLlamafile {
  const o = section && typeof section === 'object' ? (section as Record<string, unknown>) : {};
  const str = (k: string) => (typeof o[k] === 'string' ? (o[k] as string) : undefined);
  const num = (k: string) => (typeof o[k] === 'number' && Number.isFinite(o[k]) ? (o[k] as number) : undefined);
  return {
    enabled: typeof o.enabled === 'boolean' ? o.enabled : undefined,
    dir: str('dir'),
    default_file: str('default_file'),
    port: num('port'),
    host: str('host'),
    gpu_layers: num('gpu_layers'),
    context_size: num('context_size'),
    extra_args: Array.isArray(o.extra_args) ? o.extra_args.filter((x): x is string => typeof x === 'string') : undefined,
  };
}

/** Keys with a value are sent; a key that had a stored value and is now
 *  empty is sent as `null` so the gateway removes it (v1.68). Untouched empty
 *  keys stay absent. */
export function llamafileToUpdate(
  l: InferenceLlamafile,
  stored: InferenceLlamafile = {},
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  const keys: (keyof InferenceLlamafile)[] = ['enabled', 'dir', 'default_file', 'port', 'host', 'gpu_layers', 'context_size', 'extra_args'];
  const empty = (v: unknown) => v === undefined || v === '' || (Array.isArray(v) && v.length === 0);
  for (const k of keys) {
    const v = l[k];
    if (!empty(v)) out[k] = v;
    else if (!empty(stored[k])) out[k] = null;
  }
  return out;
}

/** `generation` without the two keys nothing reads any more (v1.68). */
function generationForUpdate(g: InferenceGeneration): InferenceGeneration {
  const { gpu_layers: _gl, context_size: _cs, ...rest } = g;
  void _gl;
  void _cs;
  return rest;
}

/**
 * Optional numeric SettingsRow — mirrors the legacy `numField`: an empty input
 * maps back to `undefined` (the field stays unset) rather than 0/NaN, so we do
 * not silently write a value the operator never typed. `RowNumber` from
 * form-rows can't express "unset", hence this local helper.
 */
function RowNumOpt({
  label,
  description,
  value,
  onChange,
  min,
  max,
  step,
  tier = 'select',
}: {
  label: ReactNode;
  description?: ReactNode;
  value: number | undefined;
  onChange: (v: number | undefined) => void;
  min?: number;
  max?: number;
  step?: number;
  tier?: SettingsRowTier;
}) {
  return (
    <SettingsRow label={label} description={description} tier={tier}>
      <Input
        type="number"
        value={value ?? ''}
        min={min}
        max={max}
        step={step}
        onChange={(e) => onChange(e.target.value === '' ? undefined : Number(e.target.value))}
      />
    </SettingsRow>
  );
}

export function InferencePage() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  const [config, setConfig] = useState<InferenceConfig | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  // The gateway's refusal (e.g. a removed backend), shown next to the form.
  const [saveError, setSaveError] = useState<unknown>(null);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Root scalars
  const [enabled, setEnabled] = useState(false);
  const [backend, setBackend] = useState('');
  const [modelsDir, setModelsDir] = useState('');
  const [defaultModel, setDefaultModel] = useState('');
  const [autoLoad, setAutoLoad] = useState(false);
  // 記憶體上限 (`max_memory_mb`) and the generation 「GPU Layers」/「Context 大小」
  // rows were removed: nothing in the gateway or inference crate reads those
  // keys (the only backend left is an external OpenAI-compatible server,
  // which owns its own memory, GPU offload and context size). Saved values
  // stay in inference.toml untouched — `generation` round-trips as loaded.

  // Generation
  const [gen, setGen] = useState<InferenceGeneration>({});
  // Router
  const [router, setRouter] = useState<InferenceRouter>({});
  // openai_compat (api_key write-only)
  const [oc, setOc] = useState<InferenceOpenAiCompat>({});
  const [ocApiKey, setOcApiKey] = useState(''); // only sent when non-empty
  const [ocApiKeySet, setOcApiKeySet] = useState(false);

  // v1.68 (W2): typed `[llamafile]` fields. The old raw-section editor could
  // only edit keys already in the file, so a fresh install could not set any.
  // The `[embedding]` raw section was removed: nothing reads it.
  const [llamafile, setLlamafile] = useState<InferenceLlamafile>({});
  const [engineReset, setEngineReset] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const res = await api.inference.get();
      setConfig(res);
      setEnabled(Boolean(res.enabled));
      setBackend(res.backend ?? '');
      setModelsDir(res.models_dir ?? '');
      setDefaultModel(res.default_model ?? '');
      setAutoLoad(Boolean(res.auto_load));
      setGen(res.generation ?? {});
      setRouter(res.router ?? {});
      const ocIn = res.openai_compat ?? {};
      setOc({ base_url: ocIn.base_url ?? '', model: ocIn.model ?? '' });
      setOcApiKeySet(Boolean(ocIn.api_key_set));
      setOcApiKey('');
      setLlamafile(llamafileFromConfig(res.llamafile));
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => {
    load();
    return () => {
      if (savedTimerRef.current) clearTimeout(savedTimerRef.current);
    };
  }, [load]);

  // Cross-field hint: router.strong must be < router.fast.
  const strongGteFast =
    router.strong_threshold != null &&
    router.fast_threshold != null &&
    router.strong_threshold >= router.fast_threshold;

  const handleSave = async () => {
    if (strongGteFast) {
      toast.error(t('inference.router.thresholdError'));
      return;
    }
    setSaving(true);
    setSaveError(null);
    try {
      const payload: InferenceUpdate = {
        enabled,
        // Always sent: "" asks the gateway to drop the key (automatic), so a
        // stored removed backend is really cleared; an unchanged value is a
        // no-op server-side (`inf_validate_backend`).
        backend,
        // v1.68: a cleared field is sent as "" so the stored value is removed.
        models_dir: modelsDir || (config?.models_dir ? '' : undefined),
        default_model: defaultModel || (config?.default_model ? '' : undefined),
        auto_load: autoLoad,
        generation: generationForUpdate(gen),
        router,
        openai_compat: {
          base_url: oc.base_url || undefined,
          model: oc.model || undefined,
          // Write-only: only send api_key when the operator typed one.
          ...(ocApiKey !== '' ? { api_key: ocApiKey } : {}),
        },
        llamafile: llamafileToUpdate(llamafile, llamafileFromConfig(config?.llamafile)),
      };
      const res = await api.inference.update(payload);
      // v1.68: `engine_reset` = the cached engine was dropped, so channel
      // replies and dispatch use the new settings from the next request.
      setEngineReset(res?.engine_reset === true);
      setSaved(true);
      if (savedTimerRef.current) clearTimeout(savedTimerRef.current);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2500);
      toast.success(res?.engine_reset === true ? t('inference.engineReset') : t('inference.saved'));
      // Re-load so masked secret state / authoritative values refresh.
      await load();
    } catch (e) {
      setSaveError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  // The only backend the inference engine can start is `openai_compat`
  // (`duduclaw-inference` BackendType; `llama_cpp` / `mistral_rs` still parse
  // but fail at initialisation). Empty = not set, which auto-selects it too.
  // A legacy saved value stays visible, labelled, so it is not hidden.
  const backendOptions: SelectOption[] = [
    { value: '', label: t('inference.backend.auto'), raw: '' },
    { value: 'openai_compat', label: t('inference.backend.openaiCompat'), raw: 'openai_compat' },
    ...(backend && backend !== 'openai_compat'
      ? [{ value: backend, label: intl.formatMessage({ id: 'inference.backend.removed' }, { name: backend }), raw: backend }]
      : []),
  ];

  const saveStatus: SettingsSaveStatus = saving ? 'saving' : saved ? 'saved' : 'idle';

  return (
    <div className="mx-auto w-full max-w-[1200px] space-y-6">
      {/* Slim header — icon + title + subtitle · save state + actions */}
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <Cpu className="size-5 shrink-0 text-brand" />
          <div className="space-y-0.5">
            <h1 className="text-lg font-semibold">{t('nav.inference')}</h1>
            <p className="text-sm text-muted-foreground">{t('inference.desc')}</p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <SettingsSaveState status={saveStatus} savingLabel={t('common.saving')} savedLabel={t('common.saved')} />
          <Button variant="outline" size="sm" onClick={load} disabled={loading}>
            <RefreshCw className={cn('size-4', loading && 'animate-spin')} />
            {t('common.refresh')}
          </Button>
          <Button variant="brand" size="sm" onClick={handleSave} disabled={saving || loading}>
            {saving ? <Loader2 className="size-4 animate-spin" /> : <Save className="size-4" />}
            {t('common.save')}
          </Button>
        </div>
      </div>

      {engineReset && (
        <div role="status" className="flex items-start gap-2 rounded-lg border border-success/40 bg-success/10 px-3 py-2.5 text-sm text-foreground" data-testid="engine-reset">
          <RefreshCw className="mt-0.5 size-4 shrink-0 text-success" aria-hidden="true" />
          <span>{t('inference.engineReset')}</span>
        </div>
      )}

      {saveError != null && (
        <div
          role="alert"
          className="flex items-start gap-2 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2.5 text-sm text-destructive"
        >
          <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
          <div className="space-y-0.5">
            <p className="font-medium">{t('inference.saveRejected')}</p>
            <p className="break-words">{formatErrorDetail(saveError) || formatError(saveError)}</p>
          </div>
        </div>
      )}

      {/* Model install moved to the marketplace — this page keeps runtime
          settings only (the old curated list UX is retired). */}
      <div className="flex items-start gap-2 rounded-lg border border-border bg-muted/40 px-3 py-2.5 text-xs text-muted-foreground">
        <Cpu className="mt-0.5 size-4 shrink-0 text-brand" />
        <span>
          {t('inference.marketplaceMoved')}{' '}
          <a href="/manage/local-models" className="text-brand underline underline-offset-2">
            {t('manage.localModels')}
          </a>
        </span>
      </div>

      {/* Advanced-notice banner */}
      <div className="flex items-start gap-2 rounded-lg border border-border bg-muted/40 px-3 py-2.5 text-xs text-muted-foreground">
        <AlertTriangle className="mt-0.5 size-4 shrink-0 text-brand" />
        <span>{t('inference.advancedNotice')}</span>
      </div>

      {loading && !config ? (
        <div className="flex justify-center py-16">
          <Loader2 className="size-6 animate-spin text-brand" />
        </div>
      ) : (
        <div className="grid items-start gap-6 lg:grid-cols-2">
          {/* Backend (root) */}
          <SettingsSection title={t('inference.section.backend')}>
            <SettingsCard>
              <RowSwitch label={t('inference.enabled')} checked={enabled} onChange={setEnabled} />
              <RowSelect label={t('inference.backend')} description={t('inference.backend.hint')} value={backend} onChange={setBackend} options={backendOptions} />
              <RowText label={t('inference.modelsDir')} description={t('inference.modelsDir.hint')} value={modelsDir} onChange={setModelsDir} placeholder="~/.duduclaw/models" />
              <RowText label={t('inference.defaultModel')} description={t('inference.defaultModel.hint')} value={defaultModel} onChange={setDefaultModel} />
              <RowSwitch label={t('inference.autoLoad')} checked={autoLoad} onChange={setAutoLoad} />
            </SettingsCard>
          </SettingsSection>

          {/* Generation */}
          <SettingsSection title={t('inference.section.generation')}>
            <SettingsCard>
              <RowNumOpt label={t('inference.gen.maxTokens')} value={gen.max_tokens} onChange={(n) => setGen((p) => ({ ...p, max_tokens: n }))} min={1} />
              <RowNumOpt label={t('inference.gen.temperature')} description="0.0-2.0" value={gen.temperature} onChange={(n) => setGen((p) => ({ ...p, temperature: n }))} min={0} max={2} step={0.05} />
              <RowNumOpt label={t('inference.gen.topP')} description="0.0-1.0" value={gen.top_p} onChange={(n) => setGen((p) => ({ ...p, top_p: n }))} min={0} max={1} step={0.05} />
            </SettingsCard>
            <FieldBlock label={t('inference.gen.stop')} description={t('inference.gen.stop.hint')}>
              <ChipEditor values={gen.stop ?? []} onChange={(v) => setGen((p) => ({ ...p, stop: v }))} placeholder="</s>" addLabel={t('common.add')} />
            </FieldBlock>
          </SettingsSection>

          {/* Router */}
          <SettingsSection title={t('inference.section.router')}>
            {strongGteFast && (
              <div className="flex items-center gap-2 rounded-lg bg-destructive/10 p-2 text-xs text-destructive">
                <AlertTriangle className="size-4 shrink-0" />
                {t('inference.router.thresholdError')}
              </div>
            )}
            <SettingsCard>
              <RowSwitch label={t('inference.router.enabled')} checked={Boolean(router.enabled)} onChange={(v) => setRouter((p) => ({ ...p, enabled: v }))} />
              <RowNumOpt label={t('inference.router.fastThreshold')} description="0.0-1.0" value={router.fast_threshold} onChange={(n) => setRouter((p) => ({ ...p, fast_threshold: n }))} min={0} max={1} step={0.05} />
              <RowNumOpt label={t('inference.router.strongThreshold')} description={t('inference.router.strongThreshold.hint')} value={router.strong_threshold} onChange={(n) => setRouter((p) => ({ ...p, strong_threshold: n }))} min={0} max={1} step={0.05} />
              <RowText label={t('inference.router.fastModel')} value={router.fast_model ?? ''} onChange={(v) => setRouter((p) => ({ ...p, fast_model: v }))} />
              <RowText label={t('inference.router.strongModel')} value={router.strong_model ?? ''} onChange={(v) => setRouter((p) => ({ ...p, strong_model: v }))} />
              <RowNumOpt label={t('inference.router.maxFastPromptTokens')} value={router.max_fast_prompt_tokens} onChange={(n) => setRouter((p) => ({ ...p, max_fast_prompt_tokens: n }))} min={0} />
            </SettingsCard>
            <FieldBlock label={t('inference.router.cloudKeywords')}>
              <ChipEditor values={router.cloud_keywords ?? []} onChange={(v) => setRouter((p) => ({ ...p, cloud_keywords: v }))} placeholder="analyze" addLabel={t('common.add')} />
            </FieldBlock>
            <FieldBlock label={t('inference.router.fastKeywords')}>
              <ChipEditor values={router.fast_keywords ?? []} onChange={(v) => setRouter((p) => ({ ...p, fast_keywords: v }))} placeholder="hi" addLabel={t('common.add')} />
            </FieldBlock>
            {/* v1.68 (W2): router keys that had a reader but no control. */}
            <AdvancedSection storageKey="inference.router.advanced" label={t('inference.router.advanced')}>
              <SettingsCard>
                <RowSwitch
                  label={t('inference.router.localTools')}
                  description={t('inference.router.localTools.hint')}
                  checked={router.local_tools !== false}
                  onChange={(v) => setRouter((p) => ({ ...p, local_tools: v }))}
                />
                <RowText label={t('inference.router.ucciFast')} description={t('inference.router.ucciPath.hint')} value={router.ucci_fast_router ?? ''} onChange={(v) => setRouter((p) => ({ ...p, ucci_fast_router: v.trim() }))} />
                <RowText label={t('inference.router.ucciStrong')} description={t('inference.router.ucciPath.hint')} value={router.ucci_strong_router ?? ''} onChange={(v) => setRouter((p) => ({ ...p, ucci_strong_router: v.trim() }))} />
                <RowText label={t('inference.router.ucciObservations')} description={t('inference.router.ucciObservations.hint')} value={router.ucci_observations ?? ''} onChange={(v) => setRouter((p) => ({ ...p, ucci_observations: v.trim() }))} />
                <RowSwitch label={t('inference.router.ucciShadowStrong')} description={t('inference.router.ucciShadowStrong.hint')} checked={Boolean(router.ucci_shadow_strong)} onChange={(v) => setRouter((p) => ({ ...p, ucci_shadow_strong: v }))} />
                <RowNumOpt label={t('inference.router.ucciShadowMaxInflight')} value={router.ucci_shadow_max_inflight} onChange={(n) => setRouter((p) => ({ ...p, ucci_shadow_max_inflight: n }))} min={1} max={16} />
                <RowSwitch label={t('inference.router.ucciDropStopToken')} description={t('inference.router.ucciDropStopToken.hint')} checked={Boolean(router.ucci_drop_stop_token)} onChange={(v) => setRouter((p) => ({ ...p, ucci_drop_stop_token: v }))} />
                <RowSwitch label={t('inference.gen.captureLogprobs')} description={t('inference.gen.captureLogprobs.hint')} checked={Boolean(gen.capture_logprobs)} onChange={(v) => setGen((p) => ({ ...p, capture_logprobs: v }))} />
                <RowSwitch label={t('inference.gen.captureTopLogprobs')} checked={Boolean(gen.capture_top_logprobs)} onChange={(v) => setGen((p) => ({ ...p, capture_top_logprobs: v }))} />
              </SettingsCard>
            </AdvancedSection>
          </SettingsSection>

          {/* openai_compat (typed local backend) */}
          <SettingsSection title={t('inference.section.localBackends')}>
            <SettingsCard>
              <RowText label={t('inference.oc.baseUrl')} value={oc.base_url ?? ''} onChange={(v) => setOc((p) => ({ ...p, base_url: v }))} placeholder="http://localhost:8080/v1" />
              <RowText label={t('inference.oc.model')} value={oc.model ?? ''} onChange={(v) => setOc((p) => ({ ...p, model: v }))} />
              <RowSecret
                label={t('inference.oc.apiKey')}
                description={ocApiKeySet ? t('inference.oc.apiKey.set') : t('inference.oc.apiKey.hint')}
                value={ocApiKey}
                onChange={setOcApiKey}
                placeholder={ocApiKeySet ? '••••••••' : ''}
              />
            </SettingsCard>
          </SettingsSection>

          {/* v1.68 (W2): typed llamafile fields */}
          <SettingsSection title={t('inference.section.llamafile')} description={t('inference.llamafile.desc')}>
            <SettingsCard>
              <RowSwitch label={t('inference.llamafile.enabled')} checked={Boolean(llamafile.enabled)} onChange={(v) => setLlamafile((p) => ({ ...p, enabled: v }))} />
              <RowText label={t('inference.llamafile.dir')} description={t('inference.llamafile.dir.hint')} value={llamafile.dir ?? ''} onChange={(v) => setLlamafile((p) => ({ ...p, dir: v }))} placeholder="~/.duduclaw/llamafile" />
              <RowText label={t('inference.llamafile.defaultFile')} value={llamafile.default_file ?? ''} onChange={(v) => setLlamafile((p) => ({ ...p, default_file: v }))} />
              <RowText label={t('inference.llamafile.host')} value={llamafile.host ?? ''} onChange={(v) => setLlamafile((p) => ({ ...p, host: v }))} placeholder="127.0.0.1" />
              <RowNumOpt label={t('inference.llamafile.port')} value={llamafile.port} onChange={(n) => setLlamafile((p) => ({ ...p, port: n }))} min={1} max={65535} />
              <RowNumOpt label={t('inference.llamafile.gpuLayers')} description={t('inference.llamafile.gpuLayers.hint')} value={llamafile.gpu_layers} onChange={(n) => setLlamafile((p) => ({ ...p, gpu_layers: n }))} min={-1} />
              <RowNumOpt label={t('inference.llamafile.contextSize')} value={llamafile.context_size} onChange={(n) => setLlamafile((p) => ({ ...p, context_size: n }))} min={256} />
            </SettingsCard>
            <FieldBlock label={t('inference.llamafile.extraArgs')} description={t('inference.llamafile.extraArgs.hint')}>
              <ChipEditor values={llamafile.extra_args ?? []} onChange={(v) => setLlamafile((p) => ({ ...p, extra_args: v }))} placeholder="--threads 8" addLabel={t('common.add')} />
            </FieldBlock>
          </SettingsSection>
        </div>
      )}
    </div>
  );
}
