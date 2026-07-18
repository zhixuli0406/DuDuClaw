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
import { Card, Button, Badge, Input, Textarea } from '@/components/mds';
import { Field, CompletionBadge } from '@/components/onboarding';
import { DuDu } from '@/components/mascot';
import type { DuduFace } from '@/components/mascot/faces';
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

// Shared selection-card styling (spec §4 Card + §5.8): a resting surface card
// that highlights with the brand ring when picked.
const SELECT_CARD =
  'flex text-left rounded-xl border border-surface-border bg-surface shadow-[var(--surface-shadow)] outline-none ' +
  'transition-colors hover:bg-surface-hover ' +
  'focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50';
const SELECT_CARD_ACTIVE = 'border-brand ring-1 ring-brand hover:bg-surface';

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
// Hero / side-panel metadata (§5.8) — one small DuDu illustration + title +
// description per step. DuDu is now the side-panel illustration, not the stage.
// ---------------------------------------------------------------------------

const HERO: Record<number, { face: DuduFace; titleId: string; subtitleId: string }> = {
  1: { face: 'waving', titleId: 'welcome.hero.title', subtitleId: 'welcome.hero.subtitle' },
  2: { face: 'curious', titleId: 'welcome.backend.title', subtitleId: 'welcome.backend.subtitle' },
  3: { face: 'curious', titleId: 'welcome.industry.title', subtitleId: 'welcome.industry.subtitle' },
  4: { face: 'writing', titleId: 'welcome.identity.title', subtitleId: 'welcome.identity.subtitle' },
};

// ---------------------------------------------------------------------------
// Step indicator (progress dots — bg-brand active, muted resting)
// ---------------------------------------------------------------------------

function StepDots({ current }: { current: number }) {
  return (
    <div className="flex items-center gap-2" aria-hidden="true">
      {Array.from({ length: TOTAL_STEPS }, (_, i) => {
        const step = i + 1;
        const done = step < current;
        const active = step === current;
        return (
          <span
            key={step}
            className={cn(
              'h-1.5 rounded-full transition-all duration-200',
              active ? 'w-8 bg-brand' : done ? 'w-2 bg-brand/50' : 'w-2 bg-muted-foreground/25',
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
    <Badge variant="outline" className="border-transparent bg-success/12 text-success">
      <span className="size-1.5 rounded-full bg-success" />
      {intl.formatMessage({ id: 'welcome.backend.detected' })}
    </Badge>
  ) : (
    <Badge variant="ghost">{intl.formatMessage({ id: 'welcome.backend.notInstalled' })}</Badge>
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
      <div className="mx-auto flex min-h-full max-w-xl flex-col items-center justify-center gap-4 px-6 py-20 text-center">
        {/* The completion badge springs in + draws its check (§5.8); DuDu cheers
            as a small side illustration (§7.3). */}
        <CompletionBadge size={80} label={intl.formatMessage({ id: 'welcome.success.title' })} />
        <DuDu face="celebrating" size={56} label="DuDu" />
        <h2 className="text-xl font-semibold text-foreground sm:text-2xl">
          {intl.formatMessage({ id: 'welcome.success.title' })}
        </h2>
        <p className="text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'welcome.success.subtitle' })}
        </p>
        {deployWarning && (
          <p className="rounded-lg bg-warning/10 px-4 py-2 text-sm text-warning">
            {intl.formatMessage({ id: 'welcome.success.partialWarning' })}
          </p>
        )}
        {selectedIndustry && (
          <p className="text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'welcome.success.moreRoles' })}
          </p>
        )}
        <Button variant="brand" size="lg" className="mt-4" onClick={() => navigate('/')}>
          {intl.formatMessage({ id: 'welcome.goToDashboard' })}
        </Button>
      </div>
    );
  }

  const hero = HERO[step] ?? HERO[1];

  return (
    <div className="min-h-full bg-page-canvas">
      <div className="mx-auto grid max-w-5xl gap-8 px-6 py-10 lg:grid-cols-[0.85fr_1.15fr] lg:items-start lg:gap-12 lg:py-16">
        {/* ── Left: hero / side-panel (small DuDu + title + description + dots) ── */}
        <div className="space-y-5 lg:sticky lg:top-16">
          <DuDu face={hero.face} size={64} label="DuDu" />
          <div className="space-y-2">
            <h1 className="text-xl font-semibold text-foreground sm:text-2xl">
              {intl.formatMessage({ id: hero.titleId })}
            </h1>
            <p className="text-sm leading-relaxed text-muted-foreground">
              {intl.formatMessage({ id: hero.subtitleId })}
            </p>
          </div>
          <StepDots current={step} />
        </div>

        {/* ── Right: interactive column. Keyed by step for the pure-opacity
            cross-fade (§5.8 — no positional shift). ── */}
        <div key={step} className="mds-step-fade space-y-5">
          {/* Step 1 — what the wizard will set up (reuses per-step titles). */}
          {step === 1 && (
            <Card className="gap-0 p-2">
              <ol className="divide-y divide-surface-border">
                {[
                  { n: 1, id: 'welcome.backend.title', Icon: Cloud },
                  { n: 2, id: 'welcome.industry.title', Icon: Plug },
                  { n: 3, id: 'welcome.identity.title', Icon: KeyRound },
                ].map(({ n, id, Icon }) => (
                  <li key={n} className="flex items-center gap-3 px-3 py-3.5">
                    <span className="grid size-8 shrink-0 place-items-center rounded-lg bg-muted text-muted-foreground">
                      <Icon className="size-4" />
                    </span>
                    <span className="text-sm font-medium text-foreground">
                      {intl.formatMessage({ id })}
                    </span>
                  </li>
                ))}
              </ol>
            </Card>
          )}

          {/* Step 2 — choose AI backend */}
          {step === 2 && (
            <>
              <div className="grid gap-3 sm:grid-cols-2">
                {BACKENDS.map(({ id, icon: Icon, recommended }) => {
                  const selected = state.backend === id;
                  return (
                    <button
                      key={id}
                      type="button"
                      onClick={() => patch({ backend: id })}
                      aria-pressed={selected}
                      className={cn(SELECT_CARD, 'items-start gap-3 p-4', selected && SELECT_CARD_ACTIVE)}
                    >
                      <span
                        className={cn(
                          'grid size-10 shrink-0 place-items-center rounded-lg',
                          selected ? 'bg-brand text-brand-foreground' : 'bg-muted text-muted-foreground',
                        )}
                      >
                        <Icon className="size-5" />
                      </span>
                      <div className="min-w-0 flex-1">
                        <div className="flex flex-wrap items-center gap-2">
                          <p className="text-sm font-medium text-foreground">
                            {intl.formatMessage({ id: `welcome.backend.${id}` })}
                          </p>
                          {recommended && (
                            <Badge variant="outline" className="border-brand/25 bg-brand/10 text-brand">
                              {intl.formatMessage({ id: 'welcome.backend.recommended' })}
                            </Badge>
                          )}
                          <DetectBadge ok={detectedFor(id)} intl={intl} />
                        </div>
                        <p className="mt-0.5 text-xs text-muted-foreground">
                          {intl.formatMessage({ id: `welcome.backend.${id}.desc` })}
                        </p>
                      </div>
                    </button>
                  );
                })}
              </div>

              {/* Backend-specific sub-inputs */}
              {state.backend === 'claudeSub' && detect && (
                <Card className="p-4">
                  <p className="text-sm text-muted-foreground">
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
                <Card className="p-4">
                  <Field label={intl.formatMessage({ id: 'welcome.backend.apiKey' })} required>
                    <Input
                      type="password"
                      value={state.apiKey}
                      onChange={(e) => patch({ apiKey: e.target.value })}
                      placeholder="sk-ant-..."
                      autoComplete="off"
                    />
                  </Field>
                  <Field
                    label={intl.formatMessage({ id: 'welcome.backend.budget' })}
                    help={intl.formatMessage({ id: 'welcome.backend.budget.hint' })}
                  >
                    <Input
                      type="number"
                      min="0"
                      value={state.apiBudget}
                      onChange={(e) => patch({ apiBudget: e.target.value })}
                    />
                  </Field>
                  <p className="text-xs text-muted-foreground">
                    {intl.formatMessage({ id: 'welcome.backend.keyValidateNote' })}
                  </p>
                </Card>
              )}

              {state.backend === 'genericApi' && (
                <Card className="p-4">
                  <Field label={intl.formatMessage({ id: 'welcome.backend.baseUrl' })} required>
                    <Input
                      type="text"
                      value={state.baseUrl}
                      onChange={(e) => patch({ baseUrl: e.target.value })}
                      placeholder="https://api.openai.com/v1"
                    />
                  </Field>
                  <Field label={intl.formatMessage({ id: 'welcome.backend.modelId' })} required>
                    <Input
                      type="text"
                      value={state.genericModel}
                      onChange={(e) => patch({ genericModel: e.target.value })}
                      placeholder="gpt-4o-mini"
                    />
                  </Field>
                  <Field
                    label={intl.formatMessage({ id: 'welcome.backend.apiKey' })}
                    help={intl.formatMessage({ id: 'welcome.backend.apiKey.optional' })}
                  >
                    <Input
                      type="password"
                      value={state.genericKey}
                      onChange={(e) => patch({ genericKey: e.target.value })}
                      autoComplete="off"
                    />
                  </Field>
                </Card>
              )}

              {state.backend === 'local' && (
                <Card className="gap-3 p-4">
                  <Field
                    label={intl.formatMessage({ id: 'welcome.backend.localModel' })}
                    help={intl.formatMessage({ id: 'welcome.backend.localModel.hint' })}
                  >
                    <Input
                      type="text"
                      value={state.localModel}
                      onChange={(e) => patch({ localModel: e.target.value })}
                      placeholder={DEFAULT_LOCAL_MODEL}
                    />
                  </Field>
                  <p className="text-xs text-muted-foreground">
                    {intl.formatMessage({ id: 'welcome.backend.manageInInference' })}
                  </p>
                </Card>
              )}

              {state.backend === 'otherCli' && (
                <Card className="gap-3 p-4">
                  <p className="text-sm text-muted-foreground">
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
                            'flex items-center gap-2 rounded-lg border px-3 py-2 text-sm outline-none transition-colors',
                            'focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50',
                            selected
                              ? 'border-brand bg-brand/10 text-brand'
                              : 'border-input bg-transparent text-foreground hover:bg-muted dark:bg-input/30',
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
            </>
          )}

          {/* Step 3 — pick an industry (premium template packs) */}
          {step === 3 && (
            <>
              {industryInfo === null && (
                <p className="text-sm text-muted-foreground">
                  {intl.formatMessage({ id: 'common.loading' })}
                </p>
              )}

              {industryInfo?.present_but_locked && (
                <Card className="gap-3 p-4">
                  <p className="text-sm text-muted-foreground">
                    {intl.formatMessage(
                      { id: 'welcome.industry.locked' },
                      { feature: intl.formatMessage({ id: 'license.feature.premiumTemplates' }) },
                    )}
                  </p>
                  <div>
                    <Button variant="outline" onClick={() => navigate('/license')}>
                      {intl.formatMessage({ id: 'welcome.industry.lockedCta' })}
                    </Button>
                  </div>
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
                      SELECT_CARD,
                      'w-full items-start gap-3 p-4',
                      selectedIndustry === null && SELECT_CARD_ACTIVE,
                    )}
                  >
                    <div className="min-w-0 flex-1">
                      <p className="text-sm font-medium text-foreground">
                        {intl.formatMessage({ id: 'welcome.industry.skip' })}
                      </p>
                      <p className="mt-0.5 text-xs text-muted-foreground">
                        {intl.formatMessage({ id: 'welcome.industry.skip.desc' })}
                      </p>
                    </div>
                  </button>

                  <Input
                    type="text"
                    value={industryFilter}
                    onChange={(e) => setIndustryFilter(e.target.value)}
                    placeholder={intl.formatMessage({ id: 'welcome.industry.filter' })}
                  />

                  <div className="max-h-[46vh] overflow-y-auto pr-1">
                    <div className="grid gap-3 sm:grid-cols-2">
                      {industryInfo.industries
                        .filter((ind) => {
                          const f = industryFilter.trim().toLowerCase();
                          if (!f) return true;
                          return (
                            ind.label.toLowerCase().includes(f) ||
                            ind.industry.toLowerCase().includes(f)
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
                                SELECT_CARD,
                                'flex-col items-start gap-1 p-4',
                                selected && SELECT_CARD_ACTIVE,
                              )}
                            >
                              <p className="text-sm font-medium text-foreground">{ind.label}</p>
                              <p className="text-xs text-muted-foreground">
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
            </>
          )}

          {/* Step 4 — create the first AI staff member */}
          {step === 4 && (
            <Card className="p-4">
              {/* Template picker — CEO by default, front-desk when an industry is staged. */}
              {roster && roster.roles.some((r) => r.kind === 'ceo' || r.kind === 'front_desk') && (
                <Field label={intl.formatMessage({ id: 'welcome.template.title' })}>
                  <div className="flex flex-wrap gap-2">
                    <button
                      type="button"
                      onClick={() => void selectTemplate('')}
                      aria-pressed={templateRoleId === ''}
                      className={cn(
                        'rounded-lg border px-3 py-2 text-sm outline-none transition-colors',
                        'focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50',
                        templateRoleId === ''
                          ? 'border-brand bg-brand/10 text-brand'
                          : 'border-input bg-transparent text-foreground hover:bg-muted dark:bg-input/30',
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
                              'rounded-lg border px-3 py-2 text-sm outline-none transition-colors disabled:opacity-50',
                              'focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50',
                              selected
                                ? 'border-brand bg-brand/10 text-brand'
                                : 'border-input bg-transparent text-foreground hover:bg-muted dark:bg-input/30',
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
                <Input
                  type="text"
                  value={state.displayName}
                  onChange={(e) => onDisplayNameChange(e.target.value)}
                  placeholder={intl.formatMessage({ id: 'welcome.identity.displayName.placeholder' })}
                />
              </Field>
              <Field
                label={intl.formatMessage({ id: 'welcome.identity.agentId' })}
                help={intl.formatMessage({ id: 'welcome.identity.agentId.hint' })}
              >
                <Input
                  type="text"
                  value={state.agentId}
                  onChange={(e) => patch({ agentId: e.target.value })}
                  placeholder="assistant"
                />
              </Field>
              <Field
                label={intl.formatMessage({ id: 'welcome.identity.trigger' })}
                help={intl.formatMessage({ id: 'welcome.identity.trigger.hint' })}
              >
                <Input
                  type="text"
                  value={state.trigger}
                  onChange={(e) => patch({ trigger: e.target.value })}
                  placeholder={`@${state.displayName || 'DuDu'}`}
                />
              </Field>
              {templateRoleId !== '' ? (
                roleLoading ? (
                  <p className="text-sm text-muted-foreground">
                    {intl.formatMessage({ id: 'welcome.template.loading' })}
                  </p>
                ) : (
                  roleDetail && (
                    <Field
                      label={intl.formatMessage({ id: 'welcome.template.soul' })}
                      help={intl.formatMessage({ id: 'welcome.template.soulHint' })}
                    >
                      <Textarea
                        value={soulMd}
                        onChange={(e) => setSoulMd(e.target.value)}
                        spellCheck={false}
                        className="min-h-[40vh] resize-y font-mono leading-relaxed"
                      />
                    </Field>
                  )
                )
              ) : (
                <Field label={intl.formatMessage({ id: 'welcome.identity.persona' })}>
                  <Textarea
                    value={state.soul}
                    onChange={(e) => patch({ soul: e.target.value })}
                    rows={4}
                    className="resize-none"
                    placeholder={intl.formatMessage({ id: 'welcome.identity.persona.placeholder' })}
                  />
                </Field>
              )}
            </Card>
          )}

          {error && <p className="text-sm text-destructive">{error}</p>}

          {/* Navigation */}
          <div className="flex items-center justify-between pt-2">
            <div>
              {step > 1 && (
                <Button
                  variant="outline"
                  onClick={() => setStep((s) => (s === 4 && skipIndustryStep ? 2 : s - 1))}
                >
                  <ChevronLeft />
                  {intl.formatMessage({ id: 'welcome.back' })}
                </Button>
              )}
            </div>
            <div>
              {step < TOTAL_STEPS ? (
                <Button
                  variant="brand"
                  disabled={!canAdvance()}
                  onClick={() => (step === 3 ? void handleIndustryNext() : setStep((s) => s + 1))}
                >
                  {intl.formatMessage({
                    id:
                      step === 1
                        ? 'welcome.start'
                        : step === 3 && staging
                          ? 'welcome.industry.staging'
                          : 'welcome.next',
                  })}
                  <ChevronRight />
                </Button>
              ) : (
                <Button variant="brand" disabled={deploying || !canAdvance()} onClick={handleDeploy}>
                  <Rocket />
                  {intl.formatMessage({ id: deploying ? 'welcome.creating' : 'welcome.create' })}
                </Button>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
