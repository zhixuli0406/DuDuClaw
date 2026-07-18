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
} from '@/lib/api';
import { ChipEditor } from '@/components/shared/ChipEditor';
import { toast, formatError } from '@/lib/toast';
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
import { RowText, RowSwitch, FieldBlock } from '@/pages/agent-form/form-rows';
import { Cpu, Save, RefreshCw, Loader2, AlertTriangle } from 'lucide-react';

/** Read a flat backend sub-section value as a string for the input field. */
function asStr(v: unknown): string {
  if (v === undefined || v === null) return '';
  if (typeof v === 'string') return v;
  if (typeof v === 'number' || typeof v === 'boolean') return String(v);
  return '';
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
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Root scalars
  const [enabled, setEnabled] = useState(false);
  const [backend, setBackend] = useState('');
  const [modelsDir, setModelsDir] = useState('');
  const [defaultModel, setDefaultModel] = useState('');
  const [autoLoad, setAutoLoad] = useState(false);
  const [maxMemoryMb, setMaxMemoryMb] = useState<number | undefined>(undefined);

  // Generation
  const [gen, setGen] = useState<InferenceGeneration>({});
  // Router
  const [router, setRouter] = useState<InferenceRouter>({});
  // openai_compat (api_key write-only)
  const [oc, setOc] = useState<InferenceOpenAiCompat>({});
  const [ocApiKey, setOcApiKey] = useState(''); // only sent when non-empty
  const [ocApiKeySet, setOcApiKeySet] = useState(false);

  // Generic flat backend sections — stored as string maps for editing.
  const [exo, setExo] = useState<Record<string, string>>({});
  const [llamafile, setLlamafile] = useState<Record<string, string>>({});
  const [mlx, setMlx] = useState<Record<string, string>>({});
  const [mistralrs, setMistralrs] = useState<Record<string, string>>({});
  const [llmlingua, setLlmlingua] = useState<Record<string, string>>({});
  const [streamingLlm, setStreamingLlm] = useState<Record<string, string>>({});
  const [embedding, setEmbedding] = useState<Record<string, string>>({});

  const toStrMap = (section: unknown): Record<string, string> => {
    const out: Record<string, string> = {};
    if (section && typeof section === 'object') {
      for (const [k, v] of Object.entries(section as Record<string, unknown>)) {
        // Arrays are not handled by this generic editor; skip them.
        if (Array.isArray(v)) continue;
        out[k] = asStr(v);
      }
    }
    return out;
  };

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
      setMaxMemoryMb(res.max_memory_mb ?? undefined);
      setGen(res.generation ?? {});
      setRouter(res.router ?? {});
      const ocIn = res.openai_compat ?? {};
      setOc({ base_url: ocIn.base_url ?? '', model: ocIn.model ?? '' });
      setOcApiKeySet(Boolean(ocIn.api_key_set));
      setOcApiKey('');
      setExo(toStrMap(res.exo));
      setLlamafile(toStrMap(res.llamafile));
      setMlx(toStrMap(res.mlx));
      setMistralrs(toStrMap(res.mistralrs));
      setLlmlingua(toStrMap(res.llmlingua));
      setStreamingLlm(toStrMap(res.streaming_llm));
      setEmbedding(toStrMap(res.embedding));
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

  /** Coerce a string map back to typed values (number/bool inference). */
  const coerceMap = (m: Record<string, string>): Record<string, unknown> => {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(m)) {
      if (v === '') continue;
      if (v === 'true') out[k] = true;
      else if (v === 'false') out[k] = false;
      else if (/^-?\d+$/.test(v)) out[k] = Number(v);
      else if (/^-?\d*\.\d+$/.test(v)) out[k] = Number(v);
      else out[k] = v;
    }
    return out;
  };

  const handleSave = async () => {
    if (strongGteFast) {
      toast.error(t('inference.router.thresholdError'));
      return;
    }
    setSaving(true);
    try {
      const payload: InferenceUpdate = {
        enabled,
        backend: backend || undefined,
        models_dir: modelsDir || undefined,
        default_model: defaultModel || undefined,
        auto_load: autoLoad,
        max_memory_mb: maxMemoryMb,
        generation: gen,
        router,
        openai_compat: {
          base_url: oc.base_url || undefined,
          model: oc.model || undefined,
          // Write-only: only send api_key when the operator typed one.
          ...(ocApiKey !== '' ? { api_key: ocApiKey } : {}),
        },
        exo: coerceMap(exo),
        llamafile: coerceMap(llamafile),
        mlx: coerceMap(mlx),
        mistralrs: coerceMap(mistralrs),
        llmlingua: coerceMap(llmlingua),
        streaming_llm: coerceMap(streamingLlm),
        embedding: coerceMap(embedding),
      };
      await api.inference.update(payload);
      setSaved(true);
      if (savedTimerRef.current) clearTimeout(savedTimerRef.current);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2500);
      toast.success(t('inference.saved'));
      // Re-load so masked secret state / authoritative values refresh.
      await load();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  // Generic editor for a flat backend section (string map) — each key becomes a
  // labelled SettingsRow; the section title + raw-config notice carry the rest.
  const BackendSection = ({ title, map, set }: { title: string; map: Record<string, string>; set: (m: Record<string, string>) => void }) => {
    const keys = Object.keys(map);
    return (
      <SettingsSection title={title} description={t('inference.backend.rawNotice')}>
        {keys.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t('inference.backend.emptySection')}</p>
        ) : (
          <SettingsCard>
            {keys.map((k) => (
              <RowText key={k} label={k} value={map[k]} onChange={(v) => set({ ...map, [k]: v })} />
            ))}
          </SettingsCard>
        )}
      </SettingsSection>
    );
  };

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
              <RowText label={t('inference.backend')} description={t('inference.backend.hint')} value={backend} onChange={setBackend} placeholder="llama_cpp" />
              <RowText label={t('inference.modelsDir')} description={t('inference.modelsDir.hint')} value={modelsDir} onChange={setModelsDir} placeholder="~/.duduclaw/models" />
              <RowText label={t('inference.defaultModel')} description={t('inference.defaultModel.hint')} value={defaultModel} onChange={setDefaultModel} />
              <RowSwitch label={t('inference.autoLoad')} checked={autoLoad} onChange={setAutoLoad} />
              <RowNumOpt label={t('inference.maxMemoryMb')} description={t('inference.maxMemoryMb.hint')} value={maxMemoryMb} onChange={setMaxMemoryMb} min={0} />
            </SettingsCard>
          </SettingsSection>

          {/* Generation */}
          <SettingsSection title={t('inference.section.generation')}>
            <SettingsCard>
              <RowNumOpt label={t('inference.gen.maxTokens')} value={gen.max_tokens} onChange={(n) => setGen((p) => ({ ...p, max_tokens: n }))} min={1} />
              <RowNumOpt label={t('inference.gen.temperature')} description="0.0-2.0" value={gen.temperature} onChange={(n) => setGen((p) => ({ ...p, temperature: n }))} min={0} max={2} step={0.05} />
              <RowNumOpt label={t('inference.gen.topP')} description="0.0-1.0" value={gen.top_p} onChange={(n) => setGen((p) => ({ ...p, top_p: n }))} min={0} max={1} step={0.05} />
              <RowNumOpt label={t('inference.gen.gpuLayers')} value={gen.gpu_layers} onChange={(n) => setGen((p) => ({ ...p, gpu_layers: n }))} min={-1} />
              <RowNumOpt label={t('inference.gen.contextSize')} value={gen.context_size} onChange={(n) => setGen((p) => ({ ...p, context_size: n }))} min={512} />
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
          </SettingsSection>

          {/* openai_compat (typed local backend) */}
          <SettingsSection title={t('inference.section.localBackends')}>
            <SettingsCard>
              <RowText label={t('inference.oc.baseUrl')} value={oc.base_url ?? ''} onChange={(v) => setOc((p) => ({ ...p, base_url: v }))} placeholder="http://localhost:8080/v1" />
              <RowText label={t('inference.oc.model')} value={oc.model ?? ''} onChange={(v) => setOc((p) => ({ ...p, model: v }))} />
              <RowText
                label={t('inference.oc.apiKey')}
                description={ocApiKeySet ? t('inference.oc.apiKey.set') : t('inference.oc.apiKey.hint')}
                type="password"
                autoComplete="off"
                value={ocApiKey}
                onChange={setOcApiKey}
                placeholder={ocApiKeySet ? '••••••••' : ''}
              />
            </SettingsCard>
          </SettingsSection>

          {/* Generic flat backend sections */}
          <BackendSection title="exo" map={exo} set={setExo} />
          <BackendSection title="llamafile" map={llamafile} set={setLlamafile} />
          <BackendSection title="mlx" map={mlx} set={setMlx} />
          <BackendSection title="mistralrs" map={mistralrs} set={setMistralrs} />
          <BackendSection title="llmlingua" map={llmlingua} set={setLlmlingua} />
          <BackendSection title="streaming_llm" map={streamingLlm} set={setStreamingLlm} />
          <BackendSection title="embedding" map={embedding} set={setEmbedding} />
        </div>
      )}
    </div>
  );
}
