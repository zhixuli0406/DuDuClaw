import { useState, useEffect, useCallback, useRef, type ComponentType } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { cn } from '@/lib/utils';
import {
  api,
  type RuntimeProvider,
  type RuntimeDetect,
  type TemplatesIndustriesResponse,
  type TemplateRoster,
  type TemplateRoleDetail,
} from '@/lib/api';
import { formatError } from '@/lib/toast';
import { useAgentsStore } from '@/stores/agents-store';
import { useTourStore } from '@/stores/tour-store';
import { Card, Button, Badge, Field, controlClass } from '@/components/ui';
import { DuDu } from '@/components/mascot';
import {
  ChevronLeft,
  ChevronRight,
  Rocket,
  Cloud,
  KeyRound,
  Plug,
  Cpu,
  Terminal,
} from 'lucide-react';

// ---------------------------------------------------------------------------
// Types & backend catalog
// ---------------------------------------------------------------------------

type Backend = 'claudeSub' | 'claudeApi' | 'genericApi' | 'local' | 'otherCli';
type OtherCli = 'codex' | 'gemini' | 'antigravity';

interface BackendDef {
  readonly id: Backend;
  readonly icon: ComponentType<{ className?: string }>;
  readonly recommended?: boolean;
}

const BACKENDS: ReadonlyArray<BackendDef> = [
  { id: 'claudeSub', icon: Cloud, recommended: true },
  { id: 'claudeApi', icon: KeyRound },
  { id: 'genericApi', icon: Plug },
  { id: 'local', icon: Cpu },
  { id: 'otherCli', icon: Terminal },
] as const;

const OTHER_CLIS: ReadonlyArray<OtherCli> = ['codex', 'gemini', 'antigravity'] as const;

const DEFAULT_LOCAL_MODEL = 'qwen3-8b-q4_k_m';
const TOTAL_STEPS = 4;

/** Fallback when `templates.industries` fails (OSS install / non-admin):
 *  behaves as "no template resources" so the industry step auto-skips. */
const NO_TEMPLATES: TemplatesIndustriesResponse = {
  unlocked: false,
  present_but_locked: false,
  staged: null,
  ceo_available: false,
  industries: [],
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Derive a valid agent id (lowercase alnum + hyphen, ≤64) from a display name. */
function toAgentId(name: string): string {
  const slug = name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 64);
  return slug || 'assistant';
}

const AGENT_ID_RE = /^[a-z0-9-]{1,64}$/;

/** Strip newlines/backticks/brackets to keep persona out of prompt-injection range. */
function sanitizeSoul(input: string): string {
  return input.replace(/[`<>{}]/g, '').slice(0, 4000).trim();
}

interface WizardState {
  readonly backend: Backend | null;
  readonly otherCli: OtherCli;
  readonly apiKey: string;
  readonly apiBudget: string;
  readonly baseUrl: string;
  readonly genericModel: string;
  readonly genericKey: string;
  readonly localModel: string;
  readonly displayName: string;
  readonly agentId: string;
  readonly trigger: string;
  readonly soul: string;
}

const INITIAL: WizardState = {
  backend: null,
  otherCli: 'gemini',
  apiKey: '',
  apiBudget: '50',
  baseUrl: '',
  genericModel: '',
  genericKey: '',
  localModel: DEFAULT_LOCAL_MODEL,
  displayName: '',
  agentId: '',
  trigger: '',
  soul: '',
};

// ---------------------------------------------------------------------------
// Mid-wizard resume (sessionStorage)
// ---------------------------------------------------------------------------

/**
 * The wizard legitimately navigates away mid-flow (industry step → /license to
 * install a Pro key) and a remount would otherwise restart at step 1. Progress
 * is kept per-tab in sessionStorage; secrets (API keys) are deliberately NOT
 * persisted — returning users re-enter them. Cleared on successful deploy.
 */
const WELCOME_RESUME_KEY = 'duduclaw:welcome:resume';

function restoreWelcomeProgress(initial: WizardState): { step: number; state: WizardState } | null {
  try {
    const raw = sessionStorage.getItem(WELCOME_RESUME_KEY);
    if (!raw) return null;
    const p = JSON.parse(raw) as { step?: unknown; state?: unknown };
    if (typeof p.step !== 'number' || typeof p.state !== 'object' || p.state === null) return null;
    return {
      step: Math.min(Math.max(Math.trunc(p.step), 1), 4),
      state: { ...initial, ...(p.state as Partial<WizardState>), apiKey: '', genericKey: '' },
    };
  } catch {
    return null;
  }
}

function persistWelcomeProgress(step: number, state: WizardState): void {
  try {
    const { apiKey: _k, genericKey: _g, ...safe } = state;
    sessionStorage.setItem(WELCOME_RESUME_KEY, JSON.stringify({ step, state: safe }));
  } catch {
    /* private mode / quota — resume is best-effort */
  }
}

export function clearWelcomeProgress(): void {
  try {
    sessionStorage.removeItem(WELCOME_RESUME_KEY);
  } catch {
    /* ignore */
  }
}

// ---------------------------------------------------------------------------
// Step indicator
// ---------------------------------------------------------------------------

function StepDots({ current }: { current: number }) {
  return (
    <div className="flex items-center justify-center gap-2">
      {Array.from({ length: TOTAL_STEPS }, (_, i) => {
        const step = i + 1;
        const done = step < current;
        const active = step === current;
        return (
          <span
            key={step}
            className={cn(
              'h-2 rounded-full transition-all duration-200',
              active ? 'w-8 bg-amber-500' : done ? 'w-2 bg-amber-500/60' : 'w-2 bg-stone-500/25',
            )}
          />
        );
      })}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Detected badge for a backend card
// ---------------------------------------------------------------------------

function DetectBadge({ ok, intl }: { ok: boolean | undefined; intl: ReturnType<typeof useIntl> }) {
  if (ok === undefined) return null;
  return ok ? (
    <Badge tone="success" dot>
      {intl.formatMessage({ id: 'welcome.backend.detected' })}
    </Badge>
  ) : (
    <Badge tone="neutral">{intl.formatMessage({ id: 'welcome.backend.notInstalled' })}</Badge>
  );
}

// ---------------------------------------------------------------------------
// Main page
// ---------------------------------------------------------------------------

export function WelcomePage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);
  const requestTourPrompt = useTourStore((s) => s.requestPrompt);

  // Resume mid-wizard progress (e.g. back from /license during the industry
  // step) — lazy init so the restore runs once per mount.
  const [step, setStep] = useState(() => restoreWelcomeProgress(INITIAL)?.step ?? 1);
  const [state, setState] = useState<WizardState>(
    () => restoreWelcomeProgress(INITIAL)?.state ?? INITIAL,
  );
  const [detect, setDetect] = useState<RuntimeDetect | null>(null);
  const [deploying, setDeploying] = useState(false);
  const [deployed, setDeployed] = useState(false);
  /** Agent was created but a post-create settings step failed — success page
   *  still shows (retrying would hit "already exists"), with a warning line. */
  const [deployWarning, setDeployWarning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // ── Industry template state (Step 3 + Step 4 template picker) ──
  const [industryInfo, setIndustryInfo] = useState<TemplatesIndustriesResponse | null>(null);
  const [selectedIndustry, setSelectedIndustry] = useState<string | null>(null);
  const [industryFilter, setIndustryFilter] = useState('');
  const [staging, setStaging] = useState(false);
  const [roster, setRoster] = useState<TemplateRoster | null>(null);
  const [templateRoleId, setTemplateRoleId] = useState('');
  const [roleDetail, setRoleDetail] = useState<TemplateRoleDetail | null>(null);
  const [roleLoading, setRoleLoading] = useState(false);
  const [soulMd, setSoulMd] = useState('');
  // Guards the one-shot CEO auto-select per roster (user choice always wins after).
  const templateAutoDone = useRef(false);

  const patch = useCallback((p: Partial<WizardState>) => setState((s) => ({ ...s, ...p })), []);

  // Best-effort runtime detection — degrade silently if it errors.
  useEffect(() => {
    let alive = true;
    api.runtime
      .detect()
      .then((d) => alive && setDetect(d))
      .catch(() => {/* no badges */});
    return () => {
      alive = false;
    };
  }, []);

  // Industry template availability — a failed call degrades to "no templates"
  // so the industry step silently disappears on OSS installs.
  useEffect(() => {
    let alive = true;
    api.templates
      .industries()
      .then((info) => {
        if (!alive) return;
        // Shape-check — malformed payloads degrade to "no templates".
        if (!Array.isArray(info?.industries)) {
          setIndustryInfo(NO_TEMPLATES);
          return;
        }
        setIndustryInfo(info);
        if (info.staged) setSelectedIndustry(info.staged);
      })
      .catch(() => alive && setIndustryInfo(NO_TEMPLATES));
    return () => {
      alive = false;
    };
  }, []);

  // Keep per-tab resume state current (skipped once the wizard has finished).
  useEffect(() => {
    if (!deployed) persistWelcomeProgress(step, state);
  }, [step, state, deployed]);

  /** True when Step 3 has nothing to show (no packs, not even a locked hint). */
  const skipIndustryStep =
    industryInfo !== null &&
    !industryInfo.present_but_locked &&
    !(industryInfo.unlocked && industryInfo.industries.length > 0);

  // Auto-skip the industry step for installs without template resources.
  useEffect(() => {
    if (step === 3 && skipIndustryStep) setStep(4);
  }, [step, skipIndustryStep]);

  // Entering Step 4 without a staged roster: the generic CEO role is still
  // offered when available (templates.roster returns it even unstaged).
  useEffect(() => {
    if (step !== 4 || roster !== null || !industryInfo?.ceo_available) return;
    let alive = true;
    api.templates
      .roster()
      // Shape-check: a malformed payload degrades to blank-only, no crash.
      .then((r) => alive && setRoster(Array.isArray(r?.roles) ? r : null))
      .catch(() => {/* blank-only */});
    return () => {
      alive = false;
    };
  }, [step, roster, industryInfo]);

  /** Load one template role and prefill the identity fields. */
  const selectTemplate = useCallback(
    async (roleId: string) => {
      setTemplateRoleId(roleId);
      if (roleId === '') {
        setRoleDetail(null);
        return;
      }
      setRoleLoading(true);
      setError(null);
      try {
        const d = await api.templates.role(roleId, selectedIndustry ?? undefined);
        setRoleDetail(d);
        setSoulMd(d.soul_md);
        setState((s) => ({
          ...s,
          displayName: d.display_name,
          agentId: d.name,
          trigger: d.trigger,
        }));
      } catch (e) {
        setTemplateRoleId('');
        setRoleDetail(null);
        setError(formatError(e));
      } finally {
        setRoleLoading(false);
      }
    },
    [selectedIndustry],
  );

  // Default template = CEO (once per roster; never re-fires over a user choice).
  useEffect(() => {
    if (step !== 4 || !roster || templateAutoDone.current) return;
    templateAutoDone.current = true;
    const ceo = roster.roles.find((r) => r.kind === 'ceo' && !r.created);
    if (ceo) void selectTemplate(ceo.role_id);
  }, [step, roster, selectTemplate]);

  /** Step 3 → Step 4: stage the chosen industry (no agents are created). */
  const handleIndustryNext = useCallback(async () => {
    setError(null);
    if (!selectedIndustry) {
      // Skip → generic assistant. Drop any previously staged roster from state
      // so Step 4 falls back to the generic CEO-only roster.
      setRoster(null);
      setTemplateRoleId('');
      setRoleDetail(null);
      templateAutoDone.current = false;
      setStep(4);
      return;
    }
    setStaging(true);
    try {
      const res = await api.templates.stage(selectedIndustry);
      setRoster(Array.isArray(res.roster?.roles) ? res.roster : null);
      setTemplateRoleId('');
      setRoleDetail(null);
      templateAutoDone.current = false;
      setStep(4);
    } catch (e) {
      setError(formatError(e));
    } finally {
      setStaging(false);
    }
  }, [selectedIndustry]);

  const detectedFor = (b: Backend): boolean | undefined => {
    if (!detect) return undefined;
    switch (b) {
      case 'claudeSub':
        return detect.claude_oauth || detect.claude_cli;
      case 'claudeApi':
        return undefined; // an API key isn't "installed" — no badge
      case 'genericApi':
        return undefined;
      case 'local':
        return undefined;
      case 'otherCli':
        return detect.codex || detect.gemini || detect.antigravity;
    }
  };

  const canAdvance = (): boolean => {
    switch (step) {
      case 1:
        return true;
      case 2:
        if (!state.backend) return false;
        if (state.backend === 'claudeApi') return state.apiKey.trim().length > 0;
        if (state.backend === 'genericApi')
          return state.baseUrl.trim().length > 0 && state.genericModel.trim().length > 0;
        return true;
      case 3:
        return !staging;
      case 4:
        return state.displayName.trim().length > 0 && AGENT_ID_RE.test(state.agentId);
      default:
        return false;
    }
  };

  const onDisplayNameChange = (value: string) => {
    // Keep agentId in sync until the user edits it manually.
    const autoId = state.agentId === '' || state.agentId === toAgentId(state.displayName);
    patch({ displayName: value, ...(autoId ? { agentId: toAgentId(value) } : {}) });
  };

  const runtimeProvider = (): RuntimeProvider => {
    switch (state.backend) {
      case 'genericApi':
        return 'openai_compat';
      case 'otherCli':
        return state.otherCli;
      default:
        return 'claude';
    }
  };

  const inferenceMode = (): 'local' | 'claude' | 'hybrid' | null => {
    switch (state.backend) {
      case 'claudeSub':
        return 'hybrid';
      case 'claudeApi':
        return 'claude';
      case 'local':
        return 'local';
      default:
        return null; // genericApi / otherCli leave the global mode untouched
    }
  };

  const apiMode = (): 'cli' | 'direct' | 'auto' => {
    switch (state.backend) {
      case 'claudeApi':
        return 'direct';
      case 'local':
        return 'cli';
      default:
        return 'auto';
    }
  };

  const handleDeploy = useCallback(async () => {
    setDeploying(true);
    setError(null);
    const usedTemplate = templateRoleId !== '' && roleDetail !== null;
    const name = state.agentId;

    // ── Phase A: pre-create config + agent creation. A failure here leaves
    // nothing created, so it hard-fails and the user can simply retry. ──
    try {
      // 1. Credentials / endpoint config first, so the agent has a brain.
      if (state.backend === 'claudeApi') {
        await api.accounts.add({
          id: 'main',
          type: 'api_key',
          key: state.apiKey.trim(),
          monthly_budget_cents: Math.max(0, Math.round(Number(state.apiBudget) * 100)) || 5000,
          priority: 1,
        });
      } else if (state.backend === 'genericApi') {
        await api.inference.update({
          enabled: true,
          openai_compat: {
            base_url: state.baseUrl.trim(),
            model: state.genericModel.trim(),
            ...(state.genericKey.trim() ? { api_key: state.genericKey.trim() } : {}),
          },
        });
      }

      // 2. Global inference mode (only when this backend implies one).
      const mode = inferenceMode();
      if (mode) {
        await api.system.updateConfig({ inference_mode: mode });
      }

      // 3. Create the agent (writes [runtime] provider + SOUL.md).
      if (usedTemplate) {
        // Template path — the backend writes SOUL.md / CONTRACT.toml /
        // agent.toml from the pack; `name` is always forced into agent.toml.
        await api.templates.createAgent({
          role_id: templateRoleId,
          ...(selectedIndustry ? { industry: selectedIndustry } : {}),
          name,
          display_name: state.displayName.trim(),
          trigger: state.trigger.trim() || `@${state.displayName.trim()}`,
          soul_md: soulMd,
        });
      } else {
        await api.agents.create({
          name,
          display_name: state.displayName.trim(),
          role: 'main',
          trigger: state.trigger.trim() || `@${state.displayName.trim()}`,
          soul: state.soul.trim() ? sanitizeSoul(state.soul) : undefined,
          runtime: { provider: runtimeProvider() },
        });
      }
    } catch (e) {
      // Template errors carry an actionable zh-TW message from the gateway
      // (e.g. a TOML validation failure) — surface it verbatim.
      setError(usedTemplate ? formatError(e) : intl.formatMessage({ id: 'welcome.error' }));
      setDeploying(false);
      return;
    }

    // ── Phase B: the agent now exists. A failure below must NOT strand the
    // user on the deploy page (retry would hit "already exists") — degrade to
    // a "created, some settings incomplete" warning on the success screen. ──
    let warned = false;

    // 4. Per-agent api_mode (+ local model wiring) via update.
    try {
      await api.agents.update(name, {
        api_mode: apiMode(),
        ...(state.backend === 'local'
          ? { local_model: state.localModel.trim() || DEFAULT_LOCAL_MODEL, prefer_local: true }
          : {}),
      });
    } catch {
      warned = true;
    }

    // 5. Refresh roster so FirstRunGate lets the app through, then offer tour.
    try {
      await fetchAgents();
    } catch {
      warned = true;
    }
    requestTourPrompt();
    setDeployWarning(warned);
    setDeployed(true);
    setDeploying(false);
    clearWelcomeProgress();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state, templateRoleId, roleDetail, soulMd, selectedIndustry, fetchAgents, requestTourPrompt, intl]);

  // ── Success ───────────────────────────────────────────────
  if (deployed) {
    return (
      <div className="page-enter mx-auto flex max-w-xl flex-col items-center justify-center py-20 text-center">
        {/* DuDu cheers on the first employee joining the team (§7.3). */}
        <DuDu face="celebrating" size={120} label="DuDu" className="mb-4" />
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'welcome.success.title' })}
        </h2>
        <p className="mt-2 text-sm text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'welcome.success.subtitle' })}
        </p>
        {deployWarning && (
          <p className="mt-3 rounded-lg bg-amber-500/10 px-4 py-2 text-sm text-amber-700 dark:text-amber-300">
            {intl.formatMessage({ id: 'welcome.success.partialWarning' })}
          </p>
        )}
        {selectedIndustry && (
          <p className="mt-2 text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'welcome.success.moreRoles' })}
          </p>
        )}
        <Button variant="primary" className="mt-8" onClick={() => navigate('/')}>
          {intl.formatMessage({ id: 'welcome.goToDashboard' })}
        </Button>
      </div>
    );
  }

  return (
    <div className="page-enter mx-auto max-w-3xl space-y-8 py-4">
      <StepDots current={step} />

      {/* Step 1 — warm welcome */}
      {step === 1 && (
        <div className="flex flex-col items-center gap-5 py-8 text-center">
          {/* DuDu the receptionist waves the operator in (§7.3 接待員). */}
          <DuDu face="waving" size={112} label="DuDu" />
          <h1 className="text-3xl font-semibold tracking-tight text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'welcome.hero.title' })}
          </h1>
          <p className="max-w-md text-sm leading-relaxed text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'welcome.hero.subtitle' })}
          </p>
        </div>
      )}

      {/* Step 2 — choose AI backend */}
      {step === 2 && (
        <div className="space-y-5">
          <div className="text-center">
            <h2 className="text-xl font-semibold text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: 'welcome.backend.title' })}
            </h2>
            <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'welcome.backend.subtitle' })}
            </p>
          </div>

          <div className="grid gap-3 sm:grid-cols-2">
            {BACKENDS.map(({ id, icon: Icon, recommended }) => {
              const selected = state.backend === id;
              return (
                <button
                  key={id}
                  type="button"
                  onClick={() => patch({ backend: id })}
                  aria-pressed={selected}
                  className={cn(
                    'panel panel-hover flex items-start gap-3 p-4 text-left transition-colors duration-200',
                    'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
                    selected && 'border-amber-500/70 ring-1 ring-amber-500/40',
                  )}
                >
                  <span
                    className={cn(
                      'grid h-10 w-10 shrink-0 place-items-center rounded-lg',
                      selected ? 'bg-amber-500 text-white' : 'bg-stone-500/10 text-stone-500 dark:text-stone-400',
                    )}
                  >
                    <Icon className="h-5 w-5" />
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <p className="text-sm font-semibold text-stone-900 dark:text-stone-50">
                        {intl.formatMessage({ id: `welcome.backend.${id}` })}
                      </p>
                      {recommended && (
                        <Badge tone="accent">{intl.formatMessage({ id: 'welcome.backend.recommended' })}</Badge>
                      )}
                      <DetectBadge ok={detectedFor(id)} intl={intl} />
                    </div>
                    <p className="mt-0.5 text-xs text-stone-500 dark:text-stone-400">
                      {intl.formatMessage({ id: `welcome.backend.${id}.desc` })}
                    </p>
                  </div>
                </button>
              );
            })}
          </div>

          {/* Backend-specific sub-inputs */}
          {state.backend === 'claudeSub' && detect && (
            <Card>
              <p className="text-sm text-stone-600 dark:text-stone-300">
                {detect.claude_oauth
                  ? intl.formatMessage(
                      { id: 'welcome.backend.claudeLoggedIn' },
                      { plan: detect.claude_subscription ?? 'OAuth' },
                    )
                  : intl.formatMessage({ id: 'welcome.backend.claudeLoginHint' })}
              </p>
            </Card>
          )}

          {state.backend === 'claudeApi' && (
            <Card bodyClassName="space-y-4">
              <Field label={intl.formatMessage({ id: 'welcome.backend.apiKey' })} required>
                <input
                  type="password"
                  value={state.apiKey}
                  onChange={(e) => patch({ apiKey: e.target.value })}
                  className={controlClass}
                  placeholder="sk-ant-..."
                  autoComplete="off"
                />
              </Field>
              <Field label={intl.formatMessage({ id: 'welcome.backend.budget' })} help={intl.formatMessage({ id: 'welcome.backend.budget.hint' })}>
                <input
                  type="number"
                  min="0"
                  value={state.apiBudget}
                  onChange={(e) => patch({ apiBudget: e.target.value })}
                  className={controlClass}
                />
              </Field>
              <p className="text-xs text-stone-400 dark:text-stone-500">
                {intl.formatMessage({ id: 'welcome.backend.keyValidateNote' })}
              </p>
            </Card>
          )}

          {state.backend === 'genericApi' && (
            <Card bodyClassName="space-y-4">
              <Field label={intl.formatMessage({ id: 'welcome.backend.baseUrl' })} required>
                <input
                  type="text"
                  value={state.baseUrl}
                  onChange={(e) => patch({ baseUrl: e.target.value })}
                  className={controlClass}
                  placeholder="https://api.openai.com/v1"
                />
              </Field>
              <Field label={intl.formatMessage({ id: 'welcome.backend.modelId' })} required>
                <input
                  type="text"
                  value={state.genericModel}
                  onChange={(e) => patch({ genericModel: e.target.value })}
                  className={controlClass}
                  placeholder="gpt-4o-mini"
                />
              </Field>
              <Field label={intl.formatMessage({ id: 'welcome.backend.apiKey' })} help={intl.formatMessage({ id: 'welcome.backend.apiKey.optional' })}>
                <input
                  type="password"
                  value={state.genericKey}
                  onChange={(e) => patch({ genericKey: e.target.value })}
                  className={controlClass}
                  autoComplete="off"
                />
              </Field>
            </Card>
          )}

          {state.backend === 'local' && (
            <Card bodyClassName="space-y-3">
              <Field label={intl.formatMessage({ id: 'welcome.backend.localModel' })} help={intl.formatMessage({ id: 'welcome.backend.localModel.hint' })}>
                <input
                  type="text"
                  value={state.localModel}
                  onChange={(e) => patch({ localModel: e.target.value })}
                  className={controlClass}
                  placeholder={DEFAULT_LOCAL_MODEL}
                />
              </Field>
              <p className="text-xs text-stone-400 dark:text-stone-500">
                {intl.formatMessage({ id: 'welcome.backend.manageInInference' })}
              </p>
            </Card>
          )}

          {state.backend === 'otherCli' && (
            <Card bodyClassName="space-y-3">
              <p className="text-sm text-stone-600 dark:text-stone-300">
                {intl.formatMessage({ id: 'welcome.backend.otherCli.pick' })}
              </p>
              <div className="flex flex-wrap gap-2">
                {OTHER_CLIS.map((cli) => {
                  const installed = detect
                    ? cli === 'codex'
                      ? detect.codex
                      : cli === 'gemini'
                        ? detect.gemini
                        : detect.antigravity
                    : undefined;
                  const selected = state.otherCli === cli;
                  return (
                    <button
                      key={cli}
                      type="button"
                      onClick={() => patch({ otherCli: cli })}
                      aria-pressed={selected}
                      className={cn(
                        'flex items-center gap-2 rounded-lg border px-3 py-2 text-sm transition-colors',
                        selected
                          ? 'border-amber-500/70 bg-amber-500/10 text-amber-700 dark:text-amber-300'
                          : 'border-[var(--panel-border)] bg-[var(--panel-fill)] text-stone-600 dark:text-stone-300',
                      )}
                    >
                      <span className="font-medium capitalize">{cli}</span>
                      <DetectBadge ok={installed} intl={intl} />
                    </button>
                  );
                })}
              </div>
            </Card>
          )}
        </div>
      )}

      {/* Step 3 — pick an industry (premium template packs) */}
      {step === 3 && (
        <div className="space-y-5">
          <div className="text-center">
            <h2 className="text-xl font-semibold text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: 'welcome.industry.title' })}
            </h2>
            <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'welcome.industry.subtitle' })}
            </p>
          </div>

          {industryInfo === null && (
            <p className="text-center text-sm text-stone-400 dark:text-stone-500">
              {intl.formatMessage({ id: 'common.loading' })}
            </p>
          )}

          {industryInfo?.present_but_locked && (
            <Card bodyClassName="space-y-3">
              <p className="text-sm text-stone-600 dark:text-stone-300">
                {intl.formatMessage(
                  { id: 'welcome.industry.locked' },
                  { feature: intl.formatMessage({ id: 'license.feature.premiumTemplates' }) },
                )}
              </p>
              <Button variant="secondary" onClick={() => navigate('/license')}>
                {intl.formatMessage({ id: 'welcome.industry.lockedCta' })}
              </Button>
            </Card>
          )}

          {industryInfo?.unlocked && industryInfo.industries.length > 0 && (
            <>
              {/* Prominent skip → generic assistant */}
              <button
                type="button"
                onClick={() => setSelectedIndustry(null)}
                aria-pressed={selectedIndustry === null}
                className={cn(
                  'panel panel-hover flex w-full items-start gap-3 p-4 text-left transition-colors duration-200',
                  'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
                  selectedIndustry === null && 'border-amber-500/70 ring-1 ring-amber-500/40',
                )}
              >
                <div className="min-w-0 flex-1">
                  <p className="text-sm font-semibold text-stone-900 dark:text-stone-50">
                    {intl.formatMessage({ id: 'welcome.industry.skip' })}
                  </p>
                  <p className="mt-0.5 text-xs text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'welcome.industry.skip.desc' })}
                  </p>
                </div>
              </button>

              <input
                type="text"
                value={industryFilter}
                onChange={(e) => setIndustryFilter(e.target.value)}
                className={controlClass}
                placeholder={intl.formatMessage({ id: 'welcome.industry.filter' })}
              />

              <div className="max-h-[46vh] overflow-y-auto pr-1">
                <div className="grid gap-3 sm:grid-cols-2">
                  {industryInfo.industries
                    .filter((ind) => {
                      const f = industryFilter.trim().toLowerCase();
                      if (!f) return true;
                      return (
                        ind.label.toLowerCase().includes(f) || ind.industry.toLowerCase().includes(f)
                      );
                    })
                    .map((ind) => {
                      const selected = selectedIndustry === ind.industry;
                      return (
                        <button
                          key={ind.industry}
                          type="button"
                          onClick={() => setSelectedIndustry(ind.industry)}
                          aria-pressed={selected}
                          className={cn(
                            'panel panel-hover flex flex-col items-start gap-1 p-4 text-left transition-colors duration-200',
                            'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40',
                            selected && 'border-amber-500/70 ring-1 ring-amber-500/40',
                          )}
                        >
                          <p className="text-sm font-semibold text-stone-900 dark:text-stone-50">
                            {ind.label}
                          </p>
                          <p className="text-xs text-stone-500 dark:text-stone-400">
                            {intl.formatMessage(
                              { id: 'welcome.industry.workerCount' },
                              { count: ind.worker_count },
                            )}
                          </p>
                        </button>
                      );
                    })}
                </div>
              </div>
            </>
          )}
        </div>
      )}

      {/* Step 4 — create the first AI staff member */}
      {step === 4 && (
        <div className="mx-auto max-w-lg space-y-5">
          <div className="text-center">
            <h2 className="text-xl font-semibold text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: 'welcome.identity.title' })}
            </h2>
          </div>

          {/* Template picker — CEO by default, front-desk when an industry is staged. */}
          {roster && roster.roles.some((r) => r.kind === 'ceo' || r.kind === 'front_desk') && (
            <Field label={intl.formatMessage({ id: 'welcome.template.title' })}>
              <div className="flex flex-wrap gap-2">
                <button
                  type="button"
                  onClick={() => void selectTemplate('')}
                  aria-pressed={templateRoleId === ''}
                  className={cn(
                    'rounded-lg border px-3 py-2 text-sm transition-colors',
                    templateRoleId === ''
                      ? 'border-amber-500/70 bg-amber-500/10 text-amber-700 dark:text-amber-300'
                      : 'border-[var(--panel-border)] bg-[var(--panel-fill)] text-stone-600 dark:text-stone-300',
                  )}
                >
                  {intl.formatMessage({ id: 'welcome.template.blank' })}
                </button>
                {roster.roles
                  .filter((r) => r.kind === 'ceo' || r.kind === 'front_desk')
                  .map((r) => {
                    const selected = templateRoleId === r.role_id;
                    return (
                      <button
                        key={r.role_id}
                        type="button"
                        disabled={r.created}
                        onClick={() => void selectTemplate(r.role_id)}
                        aria-pressed={selected}
                        title={r.summary}
                        className={cn(
                          'rounded-lg border px-3 py-2 text-sm transition-colors disabled:opacity-50',
                          selected
                            ? 'border-amber-500/70 bg-amber-500/10 text-amber-700 dark:text-amber-300'
                            : 'border-[var(--panel-border)] bg-[var(--panel-fill)] text-stone-600 dark:text-stone-300',
                        )}
                      >
                        {r.display_name}
                        {r.created && ` ${intl.formatMessage({ id: 'welcome.template.created' })}`}
                      </button>
                    );
                  })}
              </div>
            </Field>
          )}

          <Field label={intl.formatMessage({ id: 'welcome.identity.displayName' })} required>
            <input
              type="text"
              value={state.displayName}
              onChange={(e) => onDisplayNameChange(e.target.value)}
              className={controlClass}
              placeholder={intl.formatMessage({ id: 'welcome.identity.displayName.placeholder' })}
            />
          </Field>
          <Field label={intl.formatMessage({ id: 'welcome.identity.agentId' })} help={intl.formatMessage({ id: 'welcome.identity.agentId.hint' })}>
            <input
              type="text"
              value={state.agentId}
              onChange={(e) => patch({ agentId: e.target.value })}
              className={controlClass}
              placeholder="assistant"
            />
          </Field>
          <Field label={intl.formatMessage({ id: 'welcome.identity.trigger' })} help={intl.formatMessage({ id: 'welcome.identity.trigger.hint' })}>
            <input
              type="text"
              value={state.trigger}
              onChange={(e) => patch({ trigger: e.target.value })}
              className={controlClass}
              placeholder={`@${state.displayName || 'DuDu'}`}
            />
          </Field>
          {templateRoleId !== '' ? (
            roleLoading ? (
              <p className="text-sm text-stone-400 dark:text-stone-500">
                {intl.formatMessage({ id: 'welcome.template.loading' })}
              </p>
            ) : (
              roleDetail && (
                <Field
                  label={intl.formatMessage({ id: 'welcome.template.soul' })}
                  help={intl.formatMessage({ id: 'welcome.template.soulHint' })}
                >
                  <textarea
                    value={soulMd}
                    onChange={(e) => setSoulMd(e.target.value)}
                    spellCheck={false}
                    className={cn(controlClass, 'min-h-[40vh] resize-y font-mono text-sm leading-relaxed')}
                  />
                </Field>
              )
            )
          ) : (
            <Field label={intl.formatMessage({ id: 'welcome.identity.persona' })}>
              <textarea
                value={state.soul}
                onChange={(e) => patch({ soul: e.target.value })}
                rows={4}
                className={cn(controlClass, 'resize-none')}
                placeholder={intl.formatMessage({ id: 'welcome.identity.persona.placeholder' })}
              />
            </Field>
          )}
        </div>
      )}

      {error && <p className="text-center text-sm text-rose-600 dark:text-rose-400">{error}</p>}

      {/* Navigation */}
      <div className="flex items-center justify-between pt-2">
        <div>
          {step > 1 && (
            <Button
              variant="secondary"
              icon={ChevronLeft}
              onClick={() => setStep((s) => (s === 4 && skipIndustryStep ? 2 : s - 1))}
            >
              {intl.formatMessage({ id: 'welcome.back' })}
            </Button>
          )}
        </div>
        <div>
          {step < TOTAL_STEPS ? (
            <Button
              variant="primary"
              iconRight={ChevronRight}
              disabled={!canAdvance()}
              onClick={() => (step === 3 ? void handleIndustryNext() : setStep((s) => s + 1))}
            >
              {intl.formatMessage({
                id: step === 1 ? 'welcome.start' : step === 3 && staging ? 'welcome.industry.staging' : 'welcome.next',
              })}
            </Button>
          ) : (
            <Button variant="primary" icon={Rocket} disabled={deploying || !canAdvance()} onClick={handleDeploy}>
              {intl.formatMessage({ id: deploying ? 'welcome.creating' : 'welcome.create' })}
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}
