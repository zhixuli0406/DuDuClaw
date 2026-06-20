import { useCallback, useEffect, useRef, useState } from 'react';
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
import { FormField, inputClass, buttonPrimary } from '@/components/shared/Dialog';
import { ChipEditor } from '@/components/shared/ChipEditor';
import { toast, formatError } from '@/lib/toast';
import { Cpu, Save, RefreshCw, Loader2, AlertTriangle } from 'lucide-react';

// ── Local Toggle (mirrors AgentsPage) ──
function Toggle({ checked, onChange, label }: { checked: boolean; onChange: (v: boolean) => void; label: string }) {
  return (
    <label className="flex items-center justify-between py-1.5">
      <span className="text-sm text-stone-700 dark:text-stone-300">{label}</span>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
        className={cn(
          'relative inline-flex h-5 w-9 shrink-0 cursor-pointer rounded-full transition-colors',
          checked ? 'bg-amber-500' : 'bg-stone-300 dark:bg-stone-600'
        )}
      >
        <span
          className={cn(
            'pointer-events-none inline-block h-4 w-4 rounded-full bg-white shadow-sm transition-transform mt-0.5',
            checked ? 'translate-x-4 ml-0.5' : 'translate-x-0.5'
          )}
        />
      </button>
    </label>
  );
}

function SectionCard({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="glass-card rounded-2xl p-5">
      <h3 className="mb-4 text-sm font-semibold text-stone-900 dark:text-stone-50">{title}</h3>
      <div className="space-y-3">{children}</div>
    </div>
  );
}

/** Read a flat backend sub-section value as a string for the input field. */
function asStr(v: unknown): string {
  if (v === undefined || v === null) return '';
  if (typeof v === 'string') return v;
  if (typeof v === 'number' || typeof v === 'boolean') return String(v);
  return '';
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
  const [maxMemoryMb, setMaxMemoryMb] = useState('');

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
      setMaxMemoryMb(res.max_memory_mb != null ? String(res.max_memory_mb) : '');
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
        max_memory_mb: maxMemoryMb !== '' ? Number(maxMemoryMb) : undefined,
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

  const numField = (value: number | undefined, set: (n: number | undefined) => void, props?: Record<string, number>) => (
    <input
      type="number"
      value={value ?? ''}
      onChange={(e) => set(e.target.value === '' ? undefined : Number(e.target.value))}
      className={inputClass}
      {...props}
    />
  );

  // Generic editor for a flat backend section (string map).
  const BackendSection = ({ title, map, set }: { title: string; map: Record<string, string>; set: (m: Record<string, string>) => void }) => {
    const keys = Object.keys(map);
    return (
      <div className="rounded-lg border border-stone-200 p-3 dark:border-stone-700">
        <h4 className="mb-2 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{title}</h4>
        {keys.length === 0 ? (
          <p className="text-xs text-stone-400 dark:text-stone-500">{t('inference.backend.emptySection')}</p>
        ) : (
          <div className="space-y-2">
            {keys.map((k) => (
              <FormField key={k} label={k}>
                <input type="text" value={map[k]} onChange={(e) => set({ ...map, [k]: e.target.value })} className={inputClass} />
              </FormField>
            ))}
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="flex items-center gap-2 text-2xl font-semibold text-stone-900 dark:text-stone-50">
          <Cpu className="h-6 w-6 text-amber-500" />
          {t('inference.title')}
        </h2>
        <div className="flex items-center gap-2">
          <button onClick={load} disabled={loading} className="inline-flex items-center gap-2 rounded-lg border border-stone-300/70 bg-white/50 px-3 py-2 text-sm font-medium text-stone-700 transition-colors hover:bg-white/80 disabled:opacity-50 dark:border-white/10 dark:bg-white/5 dark:text-stone-300">
            <RefreshCw className={cn('h-4 w-4', loading && 'animate-spin')} />
            {t('common.refresh')}
          </button>
          <button onClick={handleSave} disabled={saving || loading} className={buttonPrimary}>
            {saving ? <Loader2 className="h-4 w-4 animate-spin" /> : <Save className="h-4 w-4" />}
            {saved ? t('common.saved') : t('common.save')}
          </button>
        </div>
      </div>

      <p className="text-sm text-stone-500 dark:text-stone-400">{t('inference.desc')}</p>

      {loading && !config ? (
        <div className="flex justify-center py-16">
          <Loader2 className="h-6 w-6 animate-spin text-amber-500" />
        </div>
      ) : (
        <div className="grid gap-5 lg:grid-cols-2">
          {/* Backend (root) */}
          <SectionCard title={t('inference.section.backend')}>
            <Toggle checked={enabled} onChange={setEnabled} label={t('inference.enabled')} />
            <FormField label={t('inference.backend')} hint={t('inference.backend.hint')}>
              <input type="text" value={backend} onChange={(e) => setBackend(e.target.value)} placeholder="llama_cpp" className={inputClass} />
            </FormField>
            <FormField label={t('inference.modelsDir')}>
              <input type="text" value={modelsDir} onChange={(e) => setModelsDir(e.target.value)} placeholder="~/.duduclaw/models" className={inputClass} />
            </FormField>
            <FormField label={t('inference.defaultModel')}>
              <input type="text" value={defaultModel} onChange={(e) => setDefaultModel(e.target.value)} className={inputClass} />
            </FormField>
            <Toggle checked={autoLoad} onChange={setAutoLoad} label={t('inference.autoLoad')} />
            <FormField label={t('inference.maxMemoryMb')}>
              <input type="number" min={0} value={maxMemoryMb} onChange={(e) => setMaxMemoryMb(e.target.value)} className={inputClass} />
            </FormField>
          </SectionCard>

          {/* Generation */}
          <SectionCard title={t('inference.section.generation')}>
            <FormField label={t('inference.gen.maxTokens')}>
              {numField(gen.max_tokens, (n) => setGen((p) => ({ ...p, max_tokens: n })), { min: 1 })}
            </FormField>
            <div className="grid grid-cols-2 gap-3">
              <FormField label={t('inference.gen.temperature')} hint="0.0-2.0">
                {numField(gen.temperature, (n) => setGen((p) => ({ ...p, temperature: n })), { min: 0, max: 2, step: 0.05 })}
              </FormField>
              <FormField label={t('inference.gen.topP')} hint="0.0-1.0">
                {numField(gen.top_p, (n) => setGen((p) => ({ ...p, top_p: n })), { min: 0, max: 1, step: 0.05 })}
              </FormField>
              <FormField label={t('inference.gen.gpuLayers')}>
                {numField(gen.gpu_layers, (n) => setGen((p) => ({ ...p, gpu_layers: n })), { min: -1 })}
              </FormField>
              <FormField label={t('inference.gen.contextSize')}>
                {numField(gen.context_size, (n) => setGen((p) => ({ ...p, context_size: n })), { min: 512 })}
              </FormField>
            </div>
            <FormField label={t('inference.gen.stop')} hint={t('inference.gen.stop.hint')}>
              <ChipEditor values={gen.stop ?? []} onChange={(v) => setGen((p) => ({ ...p, stop: v }))} placeholder="</s>" addLabel={t('common.add')} />
            </FormField>
          </SectionCard>

          {/* Router */}
          <SectionCard title={t('inference.section.router')}>
            <Toggle checked={Boolean(router.enabled)} onChange={(v) => setRouter((p) => ({ ...p, enabled: v }))} label={t('inference.router.enabled')} />
            {strongGteFast && (
              <div className="flex items-center gap-2 rounded-lg bg-rose-50 p-2 text-xs text-rose-600 dark:bg-rose-900/20 dark:text-rose-400">
                <AlertTriangle className="h-4 w-4 shrink-0" />
                {t('inference.router.thresholdError')}
              </div>
            )}
            <div className="grid grid-cols-2 gap-3">
              <FormField label={t('inference.router.fastThreshold')} hint="0.0-1.0">
                {numField(router.fast_threshold, (n) => setRouter((p) => ({ ...p, fast_threshold: n })), { min: 0, max: 1, step: 0.05 })}
              </FormField>
              <FormField label={t('inference.router.strongThreshold')} hint={t('inference.router.strongThreshold.hint')}>
                {numField(router.strong_threshold, (n) => setRouter((p) => ({ ...p, strong_threshold: n })), { min: 0, max: 1, step: 0.05 })}
              </FormField>
            </div>
            <FormField label={t('inference.router.fastModel')}>
              <input type="text" value={router.fast_model ?? ''} onChange={(e) => setRouter((p) => ({ ...p, fast_model: e.target.value }))} className={inputClass} />
            </FormField>
            <FormField label={t('inference.router.strongModel')}>
              <input type="text" value={router.strong_model ?? ''} onChange={(e) => setRouter((p) => ({ ...p, strong_model: e.target.value }))} className={inputClass} />
            </FormField>
            <FormField label={t('inference.router.maxFastPromptTokens')}>
              {numField(router.max_fast_prompt_tokens, (n) => setRouter((p) => ({ ...p, max_fast_prompt_tokens: n })), { min: 0 })}
            </FormField>
            <FormField label={t('inference.router.cloudKeywords')}>
              <ChipEditor values={router.cloud_keywords ?? []} onChange={(v) => setRouter((p) => ({ ...p, cloud_keywords: v }))} placeholder="analyze" addLabel={t('common.add')} />
            </FormField>
            <FormField label={t('inference.router.fastKeywords')}>
              <ChipEditor values={router.fast_keywords ?? []} onChange={(v) => setRouter((p) => ({ ...p, fast_keywords: v }))} placeholder="hi" addLabel={t('common.add')} />
            </FormField>
          </SectionCard>

          {/* Local backends */}
          <SectionCard title={t('inference.section.localBackends')}>
            <div className="space-y-3">
              <div className="rounded-lg border border-stone-200 p-3 dark:border-stone-700">
                <h4 className="mb-2 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">openai_compat</h4>
                <FormField label={t('inference.oc.baseUrl')}>
                  <input type="text" value={oc.base_url ?? ''} onChange={(e) => setOc((p) => ({ ...p, base_url: e.target.value }))} placeholder="http://localhost:8080/v1" className={inputClass} />
                </FormField>
                <FormField label={t('inference.oc.model')}>
                  <input type="text" value={oc.model ?? ''} onChange={(e) => setOc((p) => ({ ...p, model: e.target.value }))} className={inputClass} />
                </FormField>
                <FormField label={t('inference.oc.apiKey')} hint={ocApiKeySet ? t('inference.oc.apiKey.set') : t('inference.oc.apiKey.hint')}>
                  <input type="password" value={ocApiKey} onChange={(e) => setOcApiKey(e.target.value)} placeholder={ocApiKeySet ? '••••••••' : ''} className={inputClass} autoComplete="off" />
                </FormField>
              </div>
              <BackendSection title="exo" map={exo} set={setExo} />
              <BackendSection title="llamafile" map={llamafile} set={setLlamafile} />
              <BackendSection title="mlx" map={mlx} set={setMlx} />
              <BackendSection title="mistralrs" map={mistralrs} set={setMistralrs} />
            </div>
          </SectionCard>

          {/* Compression */}
          <SectionCard title={t('inference.section.compression')}>
            <BackendSection title="llmlingua" map={llmlingua} set={setLlmlingua} />
            <BackendSection title="streaming_llm" map={streamingLlm} set={setStreamingLlm} />
          </SectionCard>

          {/* Embedding */}
          <SectionCard title={t('inference.section.embedding')}>
            <BackendSection title="embedding" map={embedding} set={setEmbedding} />
          </SectionCard>
        </div>
      )}
    </div>
  );
}
