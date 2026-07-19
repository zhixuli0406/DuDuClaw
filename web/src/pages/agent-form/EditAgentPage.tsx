import { useEffect, useRef, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate, useParams, useSearchParams } from 'react-router';
import { useAgentsStore } from '@/stores/agents-store';
import { useSystemStore } from '@/stores/system-store';
import { departmentsOf } from '@/lib/agents';
import {
  api,
  type AgentDetail,
  type AgentUpdateParams,
  type ComputerUseMode,
  type ComputerUseConfig,
  type ContractConfig,
  type RuntimeProvider,
  type RuntimeDetect,
  type AgentOdooOverride,
} from '@/lib/api';
import { ModelSelect } from '@/components/shared/ModelSelect';
import { useAvailableModels } from '@/hooks/useAvailableModels';
import { ChipEditor } from '@/components/shared/ChipEditor';
import {
  MoneyField,
  DurationField,
  ScheduleBuilder,
  DangerZone,
  ConfirmDialog,
  type SelectOption,
} from '@/components/settings/controls';
import { toast, formatError } from '@/lib/toast';
import {
  Bot,
  Sparkles,
  Wrench,
  Plug,
  User,
  Cpu,
  Server,
  Wallet,
  Repeat,
  Settings2,
} from 'lucide-react';
import {
  Button,
  Empty,
  Spinner,
  Input,
  Textarea,
  BreadcrumbHeader,
  SettingsShell,
  SettingsTab,
  SettingsSection,
  SettingsCard,
  SettingsRow,
  SettingsSaveState,
  type SettingsNavGroup,
  type SettingsSaveStatus,
} from '@/components/mds';
import {
  type KvRow,
  RUNTIME_PROVIDERS,
  AGENT_ROLES,
  DEFAULT_RUNTIME,
  DEFAULT_EVOLUTION_ADVANCED,
  DEFAULT_CONTAINER_ADVANCED,
  DEFAULT_CAPABILITIES,
  DEFAULT_ODOO,
  DEFAULT_ADVANCED,
} from './defaults';
import { ToolPolicyEditor, MountTable, KvTable, EnvTable } from './editors';
import { RowText, RowNumber, RowSwitch, RowSelect, FieldBlock } from './form-rows';

/** Settings sub-tab whitelist (spec §5.3 式2/式3). `?tab=` is validated against
 *  this set; unknown values fall back to `general`. */
const SUBTABS = [
  'skills',
  'tools',
  'integration',
  'general',
  'model',
  'runtime',
  'budget',
  'automation',
  'advanced',
] as const;
type SubTab = (typeof SUBTABS)[number];

/**
 * EditAgentPage — standalone route (/agents/:id/edit) for deep-editing an AI
 * employee. Rebuilt for WP2.3 as a Multica master-detail settings surface
 * (spec §5.3 式2 Capabilities/Settings): a BreadcrumbHeader over a two-pane
 * `SettingsShell` (grouped left rail → `max-w-3xl` scrolling content). Every
 * field from the former 1234-line two-level-tab form is preserved and regrouped
 * into nine sub-tabs across two rail groups (能力 / 設定); the save decomposition,
 * write-only-tab semantics, and lazy prefill are unchanged — only the shell and
 * the control primitives (mds Input/Select/Switch/Textarea) changed.
 */
export function EditAgentPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const { id } = useParams<{ id: string }>();
  const [searchParams, setSearchParams] = useSearchParams();
  const { updateAgent, agents, fetchAgents } = useAgentsStore();
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';

  // Sub-tab state lives in `?tab=` (replace + whitelist).
  const rawTab = searchParams.get('tab') ?? 'general';
  const tab: SubTab = (SUBTABS as readonly string[]).includes(rawTab)
    ? (rawTab as SubTab)
    : 'general';
  const setTab = useCallback(
    (next: string) => {
      const nextParams = new URLSearchParams(searchParams);
      nextParams.set('tab', next);
      setSearchParams(nextParams, { replace: true });
    },
    [searchParams, setSearchParams],
  );

  // ── Agent detail load (the dialog received it as a prop; the page owns it) ──
  const [agent, setAgent] = useState<AgentDetail | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  useEffect(() => {
    if (!id) return;
    let cancelled = false;
    setLoading(true);
    setLoadError(null);
    api.agents.inspect(id)
      .then((detail) => {
        if (cancelled) return;
        setAgent(detail);
        setLoading(false);
      })
      .catch((e) => {
        if (cancelled) return;
        setLoadError(formatError(e));
        setLoading(false);
      });
    return () => { cancelled = true; };
  }, [id]);

  // Deep-link support: the 上級 dropdown lists existing agents — make sure the
  // roster is loaded even when this page is the first one visited.
  useEffect(() => {
    if (agents.length === 0) void fetchAgents();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ── Autosave engine ────────────────────────────────────────────────
  // There is no manual 儲存 button: every user edit schedules a debounced,
  // single-flight save. `saveStatus` drives the header SettingsSaveState.
  const [saveStatus, setSaveStatus] = useState<SettingsSaveStatus>('idle');
  const [saveError, setSaveError] = useState<string | null>(null);
  // Monotonic change counter — bumped by every USER edit (never by the
  // programmatic agent-load / lazy prefills). A change reschedules the debounce;
  // the ref mirror is read synchronously by the single-flight loop.
  const [changeCounter, setChangeCounter] = useState(0);
  const changeCounterRef = useRef(0);
  const bumpChange = useCallback(() => {
    changeCounterRef.current += 1;
    setChangeCounter(changeCounterRef.current);
  }, []);
  // Cycle-level dirty gates (refs so they read/reset synchronously across the
  // async save loop). `sectionsDirtyRef` ⇒ call updateAgent; `contractDirtyRef`
  // ⇒ call contract.update. A contract-only edit thus writes only the contract.
  const sectionsDirtyRef = useRef(false);
  const contractDirtyRef = useRef(false);
  // Every USER edit to a non-contract section marks it dirty and reschedules the
  // debounce; contract editors mark the contract gate instead.
  const markSectionEdit = useCallback(() => {
    sectionsDirtyRef.current = true;
    bumpChange();
  }, [bumpChange]);
  const markContractEdit = useCallback(() => {
    contractDirtyRef.current = true;
    bumpChange();
  }, [bumpChange]);
  // Single-flight guards: never two saves at once; one trailing save if edits
  // land while a save is in flight.
  const saveInFlightRef = useRef(false);
  const trailingSaveRef = useRef(false);
  const performSaveRef = useRef<() => Promise<void>>(async () => {});
  const savedResetTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Change 2 — one shared confirm dialog for high-risk (DangerZone) switches.
  // Turning a switch ON opens it; confirming runs `apply` (the normal updater,
  // so autosave fires too). Turning OFF applies immediately, no dialog.
  const [dangerConfirm, setDangerConfirm] = useState<{ label: string; apply: () => void } | null>(null);
  const guardDanger = useCallback(
    (label: string, apply: (v: boolean) => void) => (v: boolean) => {
      if (v === true) {
        setDangerConfirm({ label, apply: () => apply(true) });
      } else {
        apply(false);
      }
    },
    [],
  );

  // Change 3 — cached runtime.detect result (claude_oauth drives PTY default).
  const [runtimeDetect, setRuntimeDetect] = useState<RuntimeDetect | null>(null);
  // True for the session when the PTY-pool toggles were auto-enabled by the
  // OAuth default-enable materialization (surfaces a hint under the PTY section).
  const [ptyDefaultedThisSession, setPtyDefaultedThisSession] = useState(false);
  // One-time guard for the PTY-pool OAuth materialization per agent load — reset
  // in the agent-load effect so switching agents re-evaluates the default.
  const ptyDefaultAppliedRef = useRef(false);
  // Fetch runtime.detect once; errors are non-fatal (treat as not detected, so
  // the PTY default-enable simply never fires).
  useEffect(() => {
    let alive = true;
    api.runtime
      .detect()
      .then((d) => alive && setRuntimeDetect(d))
      .catch(() => {/* not detected → no PTY-pool default-enable */});
    return () => {
      alive = false;
    };
  }, []);
  // Departments pre-created in the registry (may have no members yet) — merged
  // into the free-input datalist alongside the in-use set below.
  const [registryDepartments, setRegistryDepartments] = useState<string[]>([]);
  useEffect(() => {
    let alive = true;
    api.departments
      .list()
      .then((r) => alive && setRegistryDepartments((r.departments ?? []).map((d) => d.name)))
      .catch(() => {/* list is manager+; degrade to in-use-only options */});
    return () => {
      alive = false;
    };
  }, []);

  // Available models (cloud + local) — live from the registry, deduped/cached.
  const {
    models: availableModels,
    loading: modelsLoading,
    error: modelsError,
    discoveredAt: modelsDiscoveredAt,
    refreshing: modelsRefreshing,
    refresh: modelsRefresh,
  } = useAvailableModels();

  // Local form state — initialized from agent once the detail loads
  const [form, setForm] = useState<AgentUpdateParams>({});

  // CAP — capabilities form. Prefilled from agents.inspect on tab open (see the
  // lazy effect below); a partial update is still written only when touched.
  const [caps, setCaps] = useState<typeof DEFAULT_CAPABILITIES>(DEFAULT_CAPABILITIES);
  // Tracks whether the operator touched the Capabilities tab — if untouched we
  // omit `capabilities` from the update so we don't overwrite existing config.
  const [capsDirty, setCapsDirty] = useState(false);
  // Mirror capsDirty into a ref so the async prefill can tell, at resolution
  // time, whether the operator already edited the tab (avoid clobbering edits).
  const capsDirtyRef = useRef(false);
  useEffect(() => { capsDirtyRef.current = capsDirty; }, [capsDirty]);
  // Prefill the capability form (incl. the Progent policy rules) from
  // agents.inspect the first time the 工具與權限 tab opens, so existing values are
  // visible and editable rather than reset to defaults.
  const [capsLoaded, setCapsLoaded] = useState(false);

  // CON — contract form, loaded lazily via contract.get on first tab open
  const [contract, setContract] = useState<ContractConfig>({ must_not: [], must_always: [], max_tool_calls_per_turn: 0 });
  const [contractLoaded, setContractLoaded] = useState(false);

  // RT — runtime form (write-only; inspect doesn't return [runtime])
  const [runtime, setRuntime] = useState<typeof DEFAULT_RUNTIME>(DEFAULT_RUNTIME);
  const [runtimeDirty, setRuntimeDirty] = useState(false);

  // EVO — advanced evolution form (write-only)
  const [evoAdv, setEvoAdv] = useState<typeof DEFAULT_EVOLUTION_ADVANCED>(DEFAULT_EVOLUTION_ADVANCED);
  const [evoAdvDirty, setEvoAdvDirty] = useState(false);

  // CT — advanced container form (write-only)
  const [ctAdv, setCtAdv] = useState<typeof DEFAULT_CONTAINER_ADVANCED>(DEFAULT_CONTAINER_ADVANCED);
  const [ctAdvDirty, setCtAdvDirty] = useState(false);

  // ODO — per-agent Odoo override form (write-only)
  const [odoo, setOdoo] = useState<typeof DEFAULT_ODOO>(DEFAULT_ODOO);
  const [odooDirty, setOdooDirty] = useState(false);

  // Advanced — G.8 scattered fields (write-only); account_pool prefilled from inspect.
  const [adv, setAdv] = useState<typeof DEFAULT_ADVANCED>(DEFAULT_ADVANCED);
  const [advDirty, setAdvDirty] = useState(false);

  useEffect(() => {
    if (agent) {
      // Determine current preferred/fallback as unified IDs. No hardcoded model
      // default — fall back to empty so ModelSelect prompts a live choice rather
      // than fabricating a model that may not exist for this deployment.
      const localModel = agent.model?.local?.model ?? '';
      const preferLocal = agent.model?.local?.prefer_local ?? false;
      const currentPreferred = preferLocal && localModel
        ? `local:${localModel}`
        : agent.model?.preferred ?? '';
      const currentFallback = agent.model?.fallback ?? '';

      setForm({
        display_name: agent.display_name,
        role: agent.role,
        trigger: agent.trigger,
        icon: agent.icon,
        reports_to: agent.reports_to,
        department: agent.department ?? '',
        preferred: currentPreferred,
        fallback: currentFallback,
        api_mode: (agent.model?.api_mode ?? 'cli') as 'cli' | 'direct' | 'auto',
        local_model: localModel,
        local_backend: agent.model?.local?.backend ?? 'llama_cpp',
        local_context_length: agent.model?.local?.context_length ?? 4096,
        local_gpu_layers: agent.model?.local?.gpu_layers ?? -1,
        prefer_local: preferLocal,
        use_router: agent.model?.local?.use_router ?? false,
        monthly_limit_cents: agent.budget?.monthly_limit_cents ?? 5000,
        warn_threshold_percent: agent.budget?.warn_threshold_percent ?? 80,
        hard_stop: agent.budget?.hard_stop ?? true,
        heartbeat_enabled: agent.heartbeat?.enabled ?? false,
        heartbeat_interval: agent.heartbeat?.interval_seconds ?? 3600,
        heartbeat_cron: '',
        can_create_agents: agent.permissions?.can_create_agents ?? false,
        can_send_cross_agent: agent.permissions?.can_send_cross_agent ?? true,
        can_modify_own_skills: agent.permissions?.can_modify_own_skills ?? true,
        can_modify_own_soul: agent.permissions?.can_modify_own_soul ?? false,
        can_schedule_tasks: agent.permissions?.can_schedule_tasks ?? false,
        skill_auto_activate: agent.evolution?.skill_auto_activate ?? false,
        skill_security_scan: agent.evolution?.skill_security_scan ?? true,
        gvu_enabled: agent.evolution?.gvu_enabled ?? true,
        cognitive_memory: agent.evolution?.cognitive_memory ?? true,
        sticker_enabled: agent.sticker?.enabled ?? false,
        sticker_probability: agent.sticker?.probability ?? 0.3,
        sticker_intensity_threshold: agent.sticker?.intensity_threshold ?? 0.7,
        sticker_cooldown_messages: agent.sticker?.cooldown_messages ?? 5,
        sticker_expressiveness: (agent.sticker?.expressiveness ?? 'moderate') as 'minimal' | 'moderate' | 'expressive',
      });
      setSaveError(null);
      setSaveStatus('idle');
      // Reset CAP/CON state for the newly-loaded agent.
      setCaps(DEFAULT_CAPABILITIES);
      setCapsDirty(false);
      setCapsLoaded(false);
      setContract({ must_not: [], must_always: [], max_tool_calls_per_turn: 0 });
      setContractLoaded(false);
      // RT — prefill the runtime form from the `[runtime]` block agents.inspect
      // now returns (only keys present in agent.toml; missing ones fall back to
      // DEFAULT_RUNTIME). A pure prefill keeps runtimeDirty false. Re-arm the
      // one-time PTY-pool OAuth materialization guard for the new agent.
      const rt = agent.runtime;
      setRuntime({
        provider: (rt?.provider as RuntimeProvider) ?? DEFAULT_RUNTIME.provider,
        fallback: rt?.fallback ?? DEFAULT_RUNTIME.fallback,
        pty_pool_enabled: rt?.pty_pool_enabled ?? DEFAULT_RUNTIME.pty_pool_enabled,
        worker_managed: rt?.worker_managed ?? DEFAULT_RUNTIME.worker_managed,
      });
      setRuntimeDirty(false);
      ptyDefaultAppliedRef.current = false;
      setPtyDefaultedThisSession(false);
      setEvoAdv(DEFAULT_EVOLUTION_ADVANCED);
      setEvoAdvDirty(false);
      setCtAdv(DEFAULT_CONTAINER_ADVANCED);
      setCtAdvDirty(false);
      // ODO — reset write-only Odoo override form.
      setOdoo(DEFAULT_ODOO);
      setOdooDirty(false);
      // Advanced — seed account_pool from inspect; rest are write-only defaults.
      setAdv({ ...DEFAULT_ADVANCED, account_pool: agent.model?.account_pool ?? [] });
      setAdvDirty(false);
    }
  }, [agent]);

  // CON — lazily load CONTRACT.toml when the 工具與權限 tab (which hosts the
  // contract editor) is first opened.
  useEffect(() => {
    if (tab !== 'tools' || !agent || contractLoaded) return;
    api.contract.get(agent.name).then((res) => {
      setContract({
        must_not: res.must_not ?? [],
        must_always: res.must_always ?? [],
        max_tool_calls_per_turn: res.max_tool_calls_per_turn ?? 0,
      });
      setContractLoaded(true);
    }).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setContractLoaded(true);
    });
  }, [tab, agent, contractLoaded, intl]);

  // CAP — lazily prefill the [capabilities] form (incl. Progent policy rules)
  // from agents.inspect when the 工具與權限 tab first opens. Keeps capsDirty false
  // so an untouched tab still omits `capabilities` from the update.
  useEffect(() => {
    if (tab !== 'tools' || !agent || capsLoaded) return;
    // Guard both races: (1) cross-agent — if the page switches agents while
    // this inspect is in flight, `cancelled` (set by cleanup) drops the stale
    // result so agent A's policy never lands in agent B's form; (2) operator
    // edits made during the load window are preserved by skipping the merge
    // when the tab is already dirty.
    let cancelled = false;
    api.agents.inspect(agent.name).then((detail) => {
      if (cancelled) return;
      const c = detail.capabilities;
      if (c && !capsDirtyRef.current) {
        setCaps((prev) => ({
          ...prev,
          ...c,
          computer_use_config: { ...prev.computer_use_config, ...(c.computer_use_config ?? {}) },
        }));
      }
      setCapsLoaded(true);
    }).catch((e) => {
      if (cancelled) return;
      console.warn('[api]', e);
      setCapsLoaded(true);
    });
    return () => { cancelled = true; };
  }, [tab, agent, capsLoaded]);

  const updateCap = useCallback(<K extends keyof typeof DEFAULT_CAPABILITIES>(key: K, value: (typeof DEFAULT_CAPABILITIES)[K]) => {
    setCapsDirty(true);
    markSectionEdit();
    setCaps((prev) => ({ ...prev, [key]: value }));
  }, [markSectionEdit]);

  const updateCapConfig = useCallback(<K extends keyof Required<ComputerUseConfig>>(key: K, value: Required<ComputerUseConfig>[K]) => {
    setCapsDirty(true);
    markSectionEdit();
    setCaps((prev) => ({ ...prev, computer_use_config: { ...prev.computer_use_config, [key]: value } }));
  }, [markSectionEdit]);

  // RT — runtime field updater.
  const updateRuntime = useCallback(<K extends keyof typeof DEFAULT_RUNTIME>(key: K, value: (typeof DEFAULT_RUNTIME)[K]) => {
    setRuntimeDirty(true);
    markSectionEdit();
    setRuntime((prev) => ({ ...prev, [key]: value }));
  }, [markSectionEdit]);

  // EVO — advanced evolution field updater.
  const updateEvoAdv = useCallback(<K extends keyof typeof DEFAULT_EVOLUTION_ADVANCED>(key: K, value: (typeof DEFAULT_EVOLUTION_ADVANCED)[K]) => {
    setEvoAdvDirty(true);
    markSectionEdit();
    setEvoAdv((prev) => ({ ...prev, [key]: value }));
  }, [markSectionEdit]);

  const updateEvoFactor = useCallback((key: keyof typeof DEFAULT_EVOLUTION_ADVANCED.external_factors, value: boolean) => {
    setEvoAdvDirty(true);
    markSectionEdit();
    setEvoAdv((prev) => ({ ...prev, external_factors: { ...prev.external_factors, [key]: value } }));
  }, [markSectionEdit]);

  // CT — advanced container field updater.
  const updateCtAdv = useCallback(<K extends keyof typeof DEFAULT_CONTAINER_ADVANCED>(key: K, value: (typeof DEFAULT_CONTAINER_ADVANCED)[K]) => {
    setCtAdvDirty(true);
    markSectionEdit();
    setCtAdv((prev) => ({ ...prev, [key]: value }));
  }, [markSectionEdit]);

  // ODO — per-agent Odoo override field updater.
  const updateOdoo = useCallback(<K extends keyof typeof DEFAULT_ODOO>(key: K, value: (typeof DEFAULT_ODOO)[K]) => {
    setOdooDirty(true);
    markSectionEdit();
    setOdoo((prev) => ({ ...prev, [key]: value }));
  }, [markSectionEdit]);

  // Advanced — G.8 field updater.
  const updateAdv = useCallback(<K extends keyof typeof DEFAULT_ADVANCED>(key: K, value: (typeof DEFAULT_ADVANCED)[K]) => {
    setAdvDirty(true);
    markSectionEdit();
    setAdv((prev) => ({ ...prev, [key]: value }));
  }, [markSectionEdit]);

  const updateField = useCallback(<K extends keyof AgentUpdateParams>(key: K, value: AgentUpdateParams[K]) => {
    markSectionEdit();
    setForm((prev) => ({ ...prev, [key]: value }));
  }, [markSectionEdit]);

  // CON — contract editor updater. Marks the contract gate (not the section gate)
  // so a contract-only edit debounces into a lone contract.update.
  const editContract = useCallback((updater: (prev: ContractConfig) => ContractConfig) => {
    markContractEdit();
    setContract(updater);
  }, [markContractEdit]);

  // performSave — one autosave pass. Assembles the same partial payload the old
  // manual 儲存 built (model-ID decomposition + per-section dirty-flag gating),
  // but does NOT navigate or reset the forms. `sectionsDirtyRef` gates the
  // updateAgent call and `contractDirtyRef` the contract.update, so a
  // contract-only edit writes only the contract. Both gates are consumed up
  // front and re-armed on failure so the next cycle retries.
  const performSave = useCallback(async () => {
    if (!agent) return;
    const doSections = sectionsDirtyRef.current;
    const doContract = contractDirtyRef.current;
    if (!doSections && !doContract) return;
    sectionsDirtyRef.current = false;
    contractDirtyRef.current = false;
    if (savedResetTimerRef.current) {
      clearTimeout(savedResetTimerRef.current);
      savedResetTimerRef.current = null;
    }
    setSaveStatus('saving');
    setSaveError(null);
    try {
      if (doSections) {
        // Decompose unified model IDs into cloud preferred + local config.
        const submitForm = { ...form };
      const pref = submitForm.preferred ?? '';
      const fb = submitForm.fallback ?? '';

      // When a local model occupies the preferred/fallback slot the backend still
      // needs a cloud model in the cloud slot. Derive it from live data — the
      // agent's existing cloud preferred/fallback, else the first cloud model the
      // registry reports — instead of hardcoding a model id.
      const firstCloud = availableModels.find((m) => m.type === 'cloud')?.id ?? '';
      const existingCloudPref = agent.model?.preferred && !agent.model.preferred.startsWith('local:')
        ? agent.model.preferred : '';
      const existingCloudFb = agent.model?.fallback && !agent.model.fallback.startsWith('local:')
        ? agent.model.fallback : '';
      const cloudPrefSlot = existingCloudPref || firstCloud;
      const cloudFbSlot = existingCloudFb || firstCloud;

      if (pref.startsWith('local:')) {
        // Local model as preferred: set prefer_local + local_model, keep a cloud fallback
        submitForm.local_model = pref.replace('local:', '');
        submitForm.prefer_local = true;
        submitForm.preferred = fb.startsWith('local:') ? cloudPrefSlot : (fb || cloudPrefSlot);
      } else {
        // Cloud model as preferred
        submitForm.prefer_local = false;
      }

      if (fb.startsWith('local:')) {
        submitForm.local_model = submitForm.local_model || fb.replace('local:', '');
        submitForm.fallback = cloudFbSlot;
      }

      // CAP — only include capabilities when the operator edited that tab, so we
      // never clobber an existing [capabilities] block with defaults.
      if (capsDirty) {
        submitForm.capabilities = {
          computer_use: caps.computer_use,
          computer_use_mode: caps.computer_use_mode,
          browser_via_bash: caps.browser_via_bash,
          allowed_tools: caps.allowed_tools,
          denied_tools: caps.denied_tools,
          wiki_visible_to: caps.wiki_visible_to,
          native_sandbox: caps.native_sandbox,
          policy: caps.policy,
          computer_use_config: { ...caps.computer_use_config },
        };
      }

      // RT — only include runtime when the operator edited that tab.
      if (runtimeDirty) {
        submitForm.runtime = {
          provider: runtime.provider,
          fallback: runtime.fallback,
          pty_pool_enabled: runtime.pty_pool_enabled,
          worker_managed: runtime.worker_managed,
        };
      }

      // EVO — only include evolution_advanced when edited.
      if (evoAdvDirty) {
        submitForm.evolution_advanced = {
          external_factors: { ...evoAdv.external_factors },
          skill_synthesis_enabled: evoAdv.skill_synthesis_enabled,
          skill_synthesis_threshold: evoAdv.skill_synthesis_threshold,
          skill_synthesis_cooldown_hours: evoAdv.skill_synthesis_cooldown_hours,
          skill_trial_ttl: evoAdv.skill_trial_ttl,
          skill_graduation_enabled: evoAdv.skill_graduation_enabled,
          skill_graduation_min_lift: evoAdv.skill_graduation_min_lift,
          skill_recommendation_enabled: evoAdv.skill_recommendation_enabled,
          skill_recommendation_threshold: evoAdv.skill_recommendation_threshold,
          curiosity_enabled: evoAdv.curiosity_enabled,
          curiosity_threshold: evoAdv.curiosity_threshold,
          curiosity_max_daily: evoAdv.curiosity_max_daily,
          skill_behavior_monitor_enabled: evoAdv.skill_behavior_monitor_enabled,
          skill_behavior_drift_threshold: evoAdv.skill_behavior_drift_threshold,
        };
      }

      // CT — only include container_advanced when edited. Drop env vars with an
      // empty key (backend rejects them).
      if (ctAdvDirty) {
        submitForm.container_advanced = {
          worktree_enabled: ctAdv.worktree_enabled,
          worktree_auto_merge: ctAdv.worktree_auto_merge,
          worktree_cleanup_on_exit: ctAdv.worktree_cleanup_on_exit,
          worktree_copy_files: ctAdv.worktree_copy_files,
          additional_mounts: ctAdv.additional_mounts.filter(
            (m) => m.host.trim() !== '' && m.container.trim() !== ''
          ),
          cmd: ctAdv.cmd,
          env: ctAdv.env.filter((e) => e.key.trim() !== ''),
        };
      }

      // ODO — only include odoo when the operator edited that tab. company_ids
      // are parsed from the comma-separated form. api_key/password are sent only
      // when non-empty (write-only — never echoed back).
      if (odooDirty) {
        const companyIds = odoo.company_ids
          .split(',')
          .map((s) => s.trim())
          .filter((s) => s !== '')
          .map((s) => Number(s))
          .filter((n) => Number.isInteger(n) && n >= 0);
        const odooPayload: AgentOdooOverride = {
          profile: odoo.profile,
          allowed_models: odoo.allowed_models,
          allowed_actions: odoo.allowed_actions,
          company_ids: companyIds,
          url: odoo.url,
          db: odoo.db,
          username: odoo.username,
        };
        if (odoo.api_key.trim() !== '') odooPayload.api_key = odoo.api_key;
        if (odoo.password.trim() !== '') odooPayload.password = odoo.password;
        submitForm.odoo = odooPayload;
      }

      // Advanced — G.8 scattered fields. Only include when edited.
      if (advDirty) {
        submitForm.account_pool = adv.account_pool;
        submitForm.utility = adv.utility;
        submitForm.heartbeat_max_concurrent_runs = adv.heartbeat_max_concurrent_runs;
        if (adv.heartbeat_cron_timezone.trim() !== '') submitForm.heartbeat_cron_timezone = adv.heartbeat_cron_timezone.trim();
        // proactive extras go under the nested proactive object.
        submitForm.proactive = {
          ...(submitForm.proactive ?? {}),
          token_budget_per_check: adv.proactive_token_budget_per_check,
          max_turns: adv.proactive_max_turns,
          ...(adv.proactive_timezone.trim() !== '' ? { timezone: adv.proactive_timezone.trim() } : {}),
        };
        // UI.3 — stagnation detection.
        submitForm.stagnation_enabled = adv.stagnation_enabled;
        submitForm.stagnation_window_seconds = adv.stagnation_window_seconds;
        submitForm.stagnation_trigger_threshold = adv.stagnation_trigger_threshold;
        submitForm.stagnation_action = adv.stagnation_action;
        // Free-form scalar tables — drop empty keys.
        const kvToObj = (rows: ReadonlyArray<KvRow>): Record<string, string> =>
          Object.fromEntries(rows.filter((r) => r.key.trim() !== '').map((r) => [r.key.trim(), r.value]));
        const ptc = kvToObj(adv.ptc);
        const prompt = kvToObj(adv.prompt);
        const cultural = kvToObj(adv.cultural_context);
        if (Object.keys(ptc).length > 0) submitForm.ptc = ptc;
        if (Object.keys(prompt).length > 0) submitForm.prompt = prompt;
        if (Object.keys(cultural).length > 0) submitForm.cultural_context = cultural;
      }

      // updateAgent re-fetches the roster internally. Autosave never navigates
      // away — the header SettingsSaveState indicator is the only feedback.
        await updateAgent(agent.name, submitForm);
      }
      // CON — a dirty contract writes through its own RPC (in addition to the
      // section update above; a contract-only edit runs only this branch).
      if (doContract) {
        await api.contract.update(agent.name, contract);
      }
      setSaveStatus('saved');
      // Fade the 'saved' badge back to idle after ~2s.
      savedResetTimerRef.current = setTimeout(() => setSaveStatus('idle'), 2000);
    } catch (e) {
      // Re-arm the gates so the next edit's cycle retries the failed write.
      if (doSections) sectionsDirtyRef.current = true;
      if (doContract) contractDirtyRef.current = true;
      setSaveStatus('error');
      setSaveError(formatError(e));
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    }
  }, [
    agent, form, availableModels, updateAgent, intl,
    capsDirty, caps, runtimeDirty, runtime, evoAdvDirty, evoAdv,
    ctAdvDirty, ctAdv, odooDirty, odoo, advDirty, adv, contract,
  ]);

  // Keep the ref pointed at the latest closure so the debounce timer and the
  // unmount flush always call the current-state save.
  useEffect(() => {
    performSaveRef.current = performSave;
  }, [performSave]);

  // Single-flight save loop. Never two saves at once; if edits land while a save
  // is in flight (trailing flag) or the change counter advanced mid-save, run
  // one more pass. Consumes the trailing flag before each attempt.
  const runSaveCycle = useCallback(async () => {
    if (saveInFlightRef.current) {
      trailingSaveRef.current = true;
      return;
    }
    saveInFlightRef.current = true;
    try {
      let started: number;
      do {
        started = changeCounterRef.current;
        trailingSaveRef.current = false;
        await performSaveRef.current();
      } while (trailingSaveRef.current || changeCounterRef.current !== started);
    } finally {
      saveInFlightRef.current = false;
    }
  }, []);

  // Debounce: every USER edit bumps changeCounter, which reschedules this ~1s
  // timer. The programmatic agent-load / lazy prefills never bump it.
  useEffect(() => {
    if (changeCounter === 0) return;
    const timer = setTimeout(() => { void runSaveCycle(); }, 1000);
    return () => clearTimeout(timer);
  }, [changeCounter, runSaveCycle]);

  // Flush on exit: fire any pending debounced-but-unsaved edits best-effort.
  // No await is possible in a cleanup — just invoke the current save closure.
  useEffect(() => {
    return () => {
      if (savedResetTimerRef.current) clearTimeout(savedResetTimerRef.current);
      if (sectionsDirtyRef.current || contractDirtyRef.current) {
        void performSaveRef.current();
      }
    };
  }, []);

  // Change 3c — one-time PTY-pool + Worker default-enable. Fires once per agent
  // load when Claude Code CLI OAuth is detected, the effective runtime provider
  // is 'claude', api_mode is cli/auto, and pty_pool_enabled was never
  // materialized in agent.toml (absent from inspect). Marks the runtime section
  // dirty + bumps the counter so autosave persists it; once written, inspect
  // returns the explicit value and this never fires again.
  useEffect(() => {
    if (!agent || !runtimeDetect || ptyDefaultAppliedRef.current) return;
    const providerEff = agent.runtime?.provider ?? 'claude';
    const apiMode = agent.model?.api_mode ?? 'cli';
    const ptyUnset = agent.runtime?.pty_pool_enabled === undefined;
    if (
      runtimeDetect.claude_oauth === true &&
      providerEff === 'claude' &&
      (apiMode === 'cli' || apiMode === 'auto') &&
      ptyUnset
    ) {
      ptyDefaultAppliedRef.current = true;
      setRuntime((prev) => ({ ...prev, pty_pool_enabled: true, worker_managed: true }));
      setRuntimeDirty(true);
      setPtyDefaultedThisSession(true);
      sectionsDirtyRef.current = true;
      bumpChange();
    }
  }, [agent, runtimeDetect, bumpChange]);

  if (loading) {
    return (
      <div className="flex items-center justify-center py-20" role="status" aria-live="polite">
        <Spinner className="size-6 text-muted-foreground" />
      </div>
    );
  }

  if (!agent || loadError) {
    return (
      <Empty
        icon={Bot}
        title={intl.formatMessage({ id: 'agentDetail.notFound' })}
        description={loadError ?? undefined}
        action={
          <Button variant="outline" size="sm" onClick={() => navigate('/agents')}>
            {intl.formatMessage({ id: 'agentDetail.back' })}
          </Button>
        }
      />
    );
  }

  // Plain-language option sets (label + raw technical value).
  const roleOptions: SelectOption[] = AGENT_ROLES.map((r) => ({ value: r, label: intl.formatMessage({ id: `agents.role.${r}` }), raw: r }));
  const apiModeOptions: SelectOption[] = [
    { value: 'cli', label: intl.formatMessage({ id: 'agents.apiMode.cli' }), raw: 'cli' },
    { value: 'direct', label: intl.formatMessage({ id: 'agents.apiMode.direct' }), raw: 'direct' },
    { value: 'auto', label: intl.formatMessage({ id: 'agents.apiMode.auto' }), raw: 'auto' },
  ];
  const providerOptions: SelectOption[] = RUNTIME_PROVIDERS.map((p) => ({ value: p, label: intl.formatMessage({ id: `agents.runtime.provider.${p}` }), raw: p }));
  const fallbackProviderOptions: SelectOption[] = [
    { value: '', label: intl.formatMessage({ id: 'agents.runtime.fallback.none' }), raw: '' },
    ...providerOptions,
  ];
  const localBackendOptions: SelectOption[] = [
    { value: 'llama_cpp', label: intl.formatMessage({ id: 'agents.backend.llamaCpp' }), raw: 'llama_cpp' },
    { value: 'mistral_rs', label: intl.formatMessage({ id: 'agents.backend.mistralRs' }), raw: 'mistral_rs' },
    { value: 'openai_compat', label: intl.formatMessage({ id: 'agents.backend.openaiCompat' }), raw: 'openai_compat' },
  ];
  const expressivenessOptions: SelectOption[] = [
    { value: 'minimal', label: intl.formatMessage({ id: 'agents.edit.stickerMinimal' }), raw: 'minimal' },
    { value: 'moderate', label: intl.formatMessage({ id: 'agents.edit.stickerModerate' }), raw: 'moderate' },
    { value: 'expressive', label: intl.formatMessage({ id: 'agents.edit.stickerExpressive' }), raw: 'expressive' },
  ];
  const computerUseModeOptions: SelectOption[] = [
    { value: 'container', label: intl.formatMessage({ id: 'agents.cap.mode.container' }), raw: 'container' },
    { value: 'native', label: intl.formatMessage({ id: 'agents.cap.mode.native' }), raw: 'native' },
    { value: 'auto', label: intl.formatMessage({ id: 'agents.cap.mode.auto' }), raw: 'auto' },
  ];
  const stagnationActionOptions: SelectOption[] = [
    { value: 'log_only', label: intl.formatMessage({ id: 'agents.adv.stagnation.logOnly' }), raw: 'log_only' },
    { value: 'suppress', label: intl.formatMessage({ id: 'agents.adv.stagnation.suppress' }), raw: 'suppress' },
  ];
  const statusOptions: SelectOption[] = ['active', 'paused', 'terminated'].map((s) => ({ value: s, label: intl.formatMessage({ id: `status.${s}` }), raw: s }));

  // 上級 dropdown — existing agents (excluding self); keep the current value even
  // if it isn't in the live roster so it is never silently dropped.
  const reportsToOptions: SelectOption[] = [
    { value: '', label: intl.formatMessage({ id: 'agents.edit.reportsTo.none' }), raw: '' },
    ...agents.filter((a) => a.name !== agent.name).map((a) => ({ value: a.name, label: a.display_name || a.name, raw: a.name })),
  ];
  if (form.reports_to && !reportsToOptions.some((o) => o.value === form.reports_to)) {
    reportsToOptions.push({ value: form.reports_to, label: form.reports_to, raw: form.reports_to });
  }

  // WP7 — in-use departments ∪ pre-created registry departments, for the
  // free-input datalist.
  const departmentOptions: string[] = Array.from(
    new Set([...departmentsOf(agents), ...registryDepartments]),
  ).sort();

  const usesLocalModel =
    (form.preferred ?? '').startsWith('local:') || (form.fallback ?? '').startsWith('local:');

  // Rail groups (spec §5.3): 能力 (skills/tools/integration) + 設定 (general/…).
  const navGroups: SettingsNavGroup[] = [
    {
      label: intl.formatMessage({ id: 'agents.edit.navGroup.capabilities' }),
      items: [
        { value: 'skills', label: intl.formatMessage({ id: 'agents.edit.nav.skills' }), icon: Sparkles },
        { value: 'tools', label: intl.formatMessage({ id: 'agents.edit.nav.tools' }), icon: Wrench },
        { value: 'integration', label: intl.formatMessage({ id: 'agents.edit.nav.integration' }), icon: Plug },
      ],
    },
    {
      label: intl.formatMessage({ id: 'agents.edit.navGroup.settings' }),
      items: [
        { value: 'general', label: intl.formatMessage({ id: 'agents.edit.nav.general' }), icon: User },
        { value: 'model', label: intl.formatMessage({ id: 'agents.edit.nav.model' }), icon: Cpu },
        { value: 'runtime', label: intl.formatMessage({ id: 'agents.edit.nav.runtime' }), icon: Server },
        { value: 'budget', label: intl.formatMessage({ id: 'agents.edit.nav.budget' }), icon: Wallet },
        { value: 'automation', label: intl.formatMessage({ id: 'agents.edit.nav.automation' }), icon: Repeat },
        { value: 'advanced', label: intl.formatMessage({ id: 'agents.edit.nav.advanced' }), icon: Settings2 },
      ],
    },
  ];

  const t = (key: string) => intl.formatMessage({ id: key });

  return (
    <div className="-mx-4 -mt-4 -mb-20 flex min-h-0 flex-1 flex-col md:-mx-6 md:-mt-6 md:-mb-6">
      <BreadcrumbHeader
        segments={[
          { label: t('nav.agents'), onClick: () => navigate('/agents') },
          {
            label: agent.display_name || agent.name,
            onClick: () => navigate(`/agents/${encodeURIComponent(agent.name)}`),
          },
          { label: t('agents.edit') },
        ]}
        actions={
          <>
            <SettingsSaveState
              status={saveStatus}
              savingLabel={t('common.saving')}
              savedLabel={t('agents.edit.saved')}
              errorLabel={saveError ?? t('common.saveError')}
              className="mr-1"
            />
            <Button variant="ghost" size="sm" onClick={() => navigate('/agents')}>
              {t('common.back')}
            </Button>
          </>
        }
      />

      <SettingsShell value={tab} onValueChange={setTab} groups={navGroups}>
        {/* ── 技能 ─────────────────────────────────────────── */}
        <SettingsTab value="skills" title={t('agents.edit.nav.skills')} description={t('agents.edit.nav.skills.desc')}>
          <SettingsSection>
            <SettingsCard>
              <RowNumber label={t('agents.edit.maxActiveSkills')} value={form.max_active_skills ?? 5} min={1} max={20} onChange={(v) => updateField('max_active_skills', v)} />
              <RowSwitch label={t('agents.edit.canModifySkills')} description={t('agents.edit.canModifySkills.help')} checked={form.can_modify_own_skills ?? true} onChange={(v) => updateField('can_modify_own_skills', v)} />
              <RowSwitch label={t('agents.edit.skillSecurityScan')} description={t('agents.edit.skillSecurityScan.help')} checked={form.skill_security_scan ?? true} onChange={(v) => updateField('skill_security_scan', v)} />
              <RowNumber label={t('agents.adv.skillTokenBudget')} value={form.skill_token_budget ?? 0} min={0} onChange={(v) => updateField('skill_token_budget', v)} />
            </SettingsCard>
          </SettingsSection>

          <SettingsSection title={t('agents.evo.skillSynthesis')}>
            <SettingsCard>
              <RowSwitch label={t('agents.evo.enabled')} checked={evoAdv.skill_synthesis_enabled} onChange={(v) => updateEvoAdv('skill_synthesis_enabled', v)} />
              <RowNumber label={t('agents.evo.threshold')} description={t('agents.evo.synthesisThreshold.hint')} value={evoAdv.skill_synthesis_threshold} min={1} step={1} onChange={(v) => updateEvoAdv('skill_synthesis_threshold', Math.round(v))} />
              <RowNumber label={t('agents.evo.cooldownHours')} value={evoAdv.skill_synthesis_cooldown_hours} min={0} onChange={(v) => updateEvoAdv('skill_synthesis_cooldown_hours', v)} />
              <RowNumber label={t('agents.evo.trialTtl')} description={t('agents.evo.trialTtl.hint')} value={evoAdv.skill_trial_ttl} min={0} onChange={(v) => updateEvoAdv('skill_trial_ttl', v)} />
            </SettingsCard>
          </SettingsSection>

          <SettingsSection title={t('agents.evo.skillGraduation')}>
            <SettingsCard>
              <RowSwitch label={t('agents.evo.enabled')} checked={evoAdv.skill_graduation_enabled} onChange={(v) => updateEvoAdv('skill_graduation_enabled', v)} />
              <RowNumber label={t('agents.evo.minLift')} description="0.0-1.0" value={evoAdv.skill_graduation_min_lift} min={0} max={1} step={0.05} onChange={(v) => updateEvoAdv('skill_graduation_min_lift', v)} />
            </SettingsCard>
          </SettingsSection>

          <SettingsSection title={t('agents.evo.skillRecommendation')}>
            <SettingsCard>
              <RowSwitch label={t('agents.evo.enabled')} checked={evoAdv.skill_recommendation_enabled} onChange={(v) => updateEvoAdv('skill_recommendation_enabled', v)} />
              <RowNumber label={t('agents.evo.threshold')} description="0.0-1.0" value={evoAdv.skill_recommendation_threshold} min={0} max={1} step={0.05} onChange={(v) => updateEvoAdv('skill_recommendation_threshold', v)} />
            </SettingsCard>
          </SettingsSection>

          <SettingsSection title={t('agents.evo.curiosity')}>
            <SettingsCard>
              <RowSwitch label={t('agents.evo.enabled')} checked={evoAdv.curiosity_enabled} onChange={(v) => updateEvoAdv('curiosity_enabled', v)} />
              <RowNumber label={t('agents.evo.threshold')} description="0.0-1.0" value={evoAdv.curiosity_threshold} min={0} max={1} step={0.05} onChange={(v) => updateEvoAdv('curiosity_threshold', v)} />
              <RowNumber label={t('agents.evo.maxDaily')} value={evoAdv.curiosity_max_daily} min={0} onChange={(v) => updateEvoAdv('curiosity_max_daily', v)} />
            </SettingsCard>
          </SettingsSection>

          <SettingsSection title={t('agents.evo.behaviorMonitor')}>
            <SettingsCard>
              <RowSwitch label={t('agents.evo.enabled')} checked={evoAdv.skill_behavior_monitor_enabled} onChange={(v) => updateEvoAdv('skill_behavior_monitor_enabled', v)} />
              <RowNumber label={t('agents.evo.driftThreshold')} description="0.0-1.0" value={evoAdv.skill_behavior_drift_threshold} min={0} max={1} step={0.05} onChange={(v) => updateEvoAdv('skill_behavior_drift_threshold', v)} />
            </SettingsCard>
          </SettingsSection>

          <DangerZone title={t('agents.perm.danger.title')} description={t('agents.perm.danger.desc')}>
            <SettingsCard>
              <RowSwitch label={t('agents.edit.skillAutoActivate')} description={t('agents.edit.skillAutoActivate.help')} checked={form.skill_auto_activate ?? false} onChange={guardDanger(t('agents.edit.skillAutoActivate'), (v) => updateField('skill_auto_activate', v))} />
            </SettingsCard>
          </DangerZone>
        </SettingsTab>

        {/* ── 工具與權限 ────────────────────────────────────── */}
        <SettingsTab value="tools" title={t('agents.edit.nav.tools')} description={t('agents.edit.nav.tools.desc')}>
          <SettingsSection title={t('agents.edit.section.permissions')}>
            <SettingsCard>
              <RowSwitch label={t('agents.edit.canSendCrossAgent')} description={t('agents.edit.canSendCrossAgent.help')} checked={form.can_send_cross_agent ?? true} onChange={(v) => updateField('can_send_cross_agent', v)} />
              <RowSwitch label={t('agents.edit.canScheduleTasks')} description={t('agents.edit.canScheduleTasks.help')} checked={form.can_schedule_tasks ?? false} onChange={(v) => updateField('can_schedule_tasks', v)} />
            </SettingsCard>
          </SettingsSection>

          <SettingsSection title={t('agents.edit.capabilities')} description={t('agents.cap.desc')}>
            <FieldBlock label={t('agents.cap.allowedTools')} description={t('agents.cap.allowedTools.hint')}>
              <ChipEditor values={caps.allowed_tools} onChange={(v) => updateCap('allowed_tools', v)} placeholder="Read" addLabel={t('common.add')} />
            </FieldBlock>
            <FieldBlock label={t('agents.cap.deniedTools')} description={t('agents.cap.deniedTools.hint')}>
              <ChipEditor values={caps.denied_tools} onChange={(v) => updateCap('denied_tools', v)} placeholder="Bash" addLabel={t('common.add')} />
            </FieldBlock>
            <FieldBlock label={t('agents.cap.wikiVisibleTo')} description={t('agents.cap.wikiVisibleTo.hint')}>
              <ChipEditor values={caps.wiki_visible_to} onChange={(v) => updateCap('wiki_visible_to', v)} placeholder="coder" addLabel={t('common.add')} />
            </FieldBlock>
            <SettingsCard>
              <RowSwitch label={t('agents.cap.nativeSandbox')} description={t('agents.cap.nativeSandbox.help')} checked={caps.native_sandbox} onChange={(v) => updateCap('native_sandbox', v)} />
            </SettingsCard>
            <FieldBlock label={t('agents.cap.policy')} description={t('agents.cap.policy.help')}>
              <ToolPolicyEditor value={caps.policy} onChange={(v) => updateCap('policy', v)} />
            </FieldBlock>
          </SettingsSection>

          <SettingsSection title={t('agents.contract.title')} description={t('agents.contract.desc')}>
            {!contractLoaded ? (
              <p className="py-8 text-center text-sm text-muted-foreground">{t('common.loading')}</p>
            ) : (
              <>
                <FieldBlock label={t('agents.contract.mustNot')} description={t('agents.contract.mustNot.hint')}>
                  <Textarea
                    value={contract.must_not.join('\n')}
                    onChange={(e) => editContract((p) => ({ ...p, must_not: e.target.value.split('\n').map((s) => s.trimEnd()).filter((s) => s.trim() !== '') }))}
                    rows={4}
                    placeholder={t('agents.contract.mustNot.placeholder')}
                    className="resize-none font-mono"
                  />
                </FieldBlock>
                <FieldBlock label={t('agents.contract.mustAlways')} description={t('agents.contract.mustAlways.hint')}>
                  <Textarea
                    value={contract.must_always.join('\n')}
                    onChange={(e) => editContract((p) => ({ ...p, must_always: e.target.value.split('\n').map((s) => s.trimEnd()).filter((s) => s.trim() !== '') }))}
                    rows={4}
                    placeholder={t('agents.contract.mustAlways.placeholder')}
                    className="resize-none font-mono"
                  />
                </FieldBlock>
                <SettingsCard>
                  <RowNumber label={t('agents.contract.maxToolCalls')} description={t('agents.contract.maxToolCalls.hint')} value={contract.max_tool_calls_per_turn} min={0} max={1000} onChange={(v) => editContract((p) => ({ ...p, max_tool_calls_per_turn: v }))} />
                </SettingsCard>
              </>
            )}
          </SettingsSection>

          <DangerZone title={t('agents.perm.danger.title')} description={t('agents.perm.danger.desc')}>
            <SettingsCard>
              <RowSwitch label={t('agents.edit.canCreateAgents')} description={t('agents.edit.canCreateAgents.help')} checked={form.can_create_agents ?? false} onChange={guardDanger(t('agents.edit.canCreateAgents'), (v) => updateField('can_create_agents', v))} />
              <RowSwitch label={t('agents.edit.canModifySoul')} description={t('agents.edit.canModifySoul.help')} checked={form.can_modify_own_soul ?? false} onChange={guardDanger(t('agents.edit.canModifySoul'), (v) => updateField('can_modify_own_soul', v))} />
            </SettingsCard>
          </DangerZone>

          <DangerZone title={t('agents.cap.danger.title')} description={t('agents.cap.danger.desc')}>
            <SettingsCard>
              <RowSwitch label={t('agents.cap.computerUse')} description={t('agents.cap.computerUse.help')} checked={caps.computer_use} onChange={guardDanger(t('agents.cap.computerUse'), (v) => updateCap('computer_use', v))} />
              <RowSelect label={t('agents.cap.computerUseMode')} description={t('agents.cap.computerUseMode.help')} value={caps.computer_use_mode} onChange={(v) => updateCap('computer_use_mode', v as ComputerUseMode)} options={computerUseModeOptions} />
              <RowSwitch label={t('agents.cap.browserViaBash')} description={t('agents.cap.browserViaBash.help')} checked={caps.browser_via_bash} onChange={guardDanger(t('agents.cap.browserViaBash'), (v) => updateCap('browser_via_bash', v))} />
            </SettingsCard>
            {caps.computer_use_mode === 'native' && (
              <p className="rounded-md bg-destructive/10 px-3 py-2 text-xs text-destructive">{t('agents.cap.nativeWarning')}</p>
            )}
            <FieldBlock label={t('agents.cap.allowedApps')}>
              <ChipEditor values={caps.computer_use_config.allowed_apps ?? []} onChange={(v) => updateCapConfig('allowed_apps', v)} placeholder="Safari" addLabel={t('common.add')} />
            </FieldBlock>
            <FieldBlock label={t('agents.cap.blockedActions')}>
              <ChipEditor values={caps.computer_use_config.blocked_actions ?? []} onChange={(v) => updateCapConfig('blocked_actions', v)} placeholder="key:cmd+q" addLabel={t('common.add')} />
            </FieldBlock>
            <SettingsCard>
              <RowNumber label={t('agents.cap.maxSessionMinutes')} description="1-1440" value={caps.computer_use_config.max_session_minutes} min={1} max={1440} onChange={(v) => updateCapConfig('max_session_minutes', v)} />
              <RowNumber label={t('agents.cap.maxActions')} description="1-10000" value={caps.computer_use_config.max_actions} min={1} max={10000} onChange={(v) => updateCapConfig('max_actions', v)} />
              <RowNumber label={t('agents.cap.displayWidth')} description="320-7680" value={caps.computer_use_config.display_width} min={320} max={7680} onChange={(v) => updateCapConfig('display_width', v)} />
              <RowNumber label={t('agents.cap.displayHeight')} description="240-4320" value={caps.computer_use_config.display_height} min={240} max={4320} onChange={(v) => updateCapConfig('display_height', v)} />
              <RowSwitch label={t('agents.cap.autoConfirmTrusted')} description={t('agents.cap.autoConfirmTrusted.help')} checked={caps.computer_use_config.auto_confirm_trusted ?? false} onChange={(v) => updateCapConfig('auto_confirm_trusted', v)} />
            </SettingsCard>
          </DangerZone>
        </SettingsTab>

        {/* ── 整合 ─────────────────────────────────────────── */}
        <SettingsTab value="integration" title={t('agents.edit.nav.integration')} description={t('agents.edit.nav.integration.desc')}>
          <SettingsSection title="Odoo" description={t('agents.odoo.desc')}>
            <SettingsCard>
              <RowText label={t('agents.odoo.profile')} description={t('agents.odoo.profile.hint')} value={odoo.profile} placeholder="default" onChange={(v) => updateOdoo('profile', v)} />
            </SettingsCard>
            <FieldBlock label={t('agents.odoo.allowedModels')} description={t('agents.odoo.allowedModels.hint')}>
              <ChipEditor values={odoo.allowed_models} onChange={(v) => updateOdoo('allowed_models', v)} placeholder="crm.lead" addLabel={t('common.add')} />
            </FieldBlock>
            <FieldBlock label={t('agents.odoo.allowedActions')} description={t('agents.odoo.allowedActions.hint')}>
              <ChipEditor values={odoo.allowed_actions} onChange={(v) => updateOdoo('allowed_actions', v)} placeholder="write:crm.lead" addLabel={t('common.add')} />
            </FieldBlock>
            <SettingsCard>
              <RowText label={t('agents.odoo.companyIds')} description={t('agents.odoo.companyIds.hint')} value={odoo.company_ids} placeholder="1, 2" onChange={(v) => updateOdoo('company_ids', v)} />
              <RowText label="URL" value={odoo.url} placeholder="https://erp.example.com" onChange={(v) => updateOdoo('url', v)} />
              <RowText label="DB" value={odoo.db} onChange={(v) => updateOdoo('db', v)} />
              <RowText label={t('agents.odoo.username')} value={odoo.username} onChange={(v) => updateOdoo('username', v)} />
              <RowText label={t('agents.odoo.apiKey')} description={t('agents.odoo.secret.hint')} type="password" autoComplete="off" value={odoo.api_key} onChange={(v) => updateOdoo('api_key', v)} />
              <RowText label={t('agents.odoo.password')} description={t('agents.odoo.secret.hint')} type="password" autoComplete="off" value={odoo.password} onChange={(v) => updateOdoo('password', v)} />
            </SettingsCard>
          </SettingsSection>

          <SettingsSection title={t('agents.edit.group.integration')} description={t('agents.edit.channelsDesc')}>
            <SettingsCard>
              <RowText label="Discord Bot Token" type="password" autoComplete="off" value={form.discord_bot_token ?? ''} placeholder="MTIzNDU2Nzg5..." onChange={(v) => updateField('discord_bot_token', v)} />
              <RowText label="Telegram Bot Token" type="password" autoComplete="off" value={form.telegram_bot_token ?? ''} placeholder="123456:ABC-DEF..." onChange={(v) => updateField('telegram_bot_token', v)} />
              <RowText label="LINE Channel Token" type="password" autoComplete="off" value={form.line_channel_token ?? ''} onChange={(v) => updateField('line_channel_token', v)} />
              <RowText label="LINE Channel Secret" type="password" autoComplete="off" value={form.line_channel_secret ?? ''} onChange={(v) => updateField('line_channel_secret', v)} />
              <RowText label="Slack App Token" type="password" autoComplete="off" value={form.slack_app_token ?? ''} placeholder="xapp-1-..." onChange={(v) => updateField('slack_app_token', v)} />
              <RowText label="Slack Bot Token" type="password" autoComplete="off" value={form.slack_bot_token ?? ''} placeholder="xoxb-..." onChange={(v) => updateField('slack_bot_token', v)} />
              <RowText label="WhatsApp Access Token" type="password" autoComplete="off" value={form.whatsapp_access_token ?? ''} onChange={(v) => updateField('whatsapp_access_token', v)} />
              <RowText label="WhatsApp Verify Token" value={form.whatsapp_verify_token ?? ''} onChange={(v) => updateField('whatsapp_verify_token', v)} />
              <RowText label="WhatsApp Phone Number ID" value={form.whatsapp_phone_number_id ?? ''} onChange={(v) => updateField('whatsapp_phone_number_id', v)} />
              <RowText label="WhatsApp App Secret" type="password" autoComplete="off" value={form.whatsapp_app_secret ?? ''} onChange={(v) => updateField('whatsapp_app_secret', v)} />
              <RowText label="Feishu App ID" type="password" autoComplete="off" value={form.feishu_app_id ?? ''} onChange={(v) => updateField('feishu_app_id', v)} />
              <RowText label="Feishu App Secret" type="password" autoComplete="off" value={form.feishu_app_secret ?? ''} onChange={(v) => updateField('feishu_app_secret', v)} />
              <RowText label="Feishu Verification Token" type="password" autoComplete="off" value={form.feishu_verification_token ?? ''} onChange={(v) => updateField('feishu_verification_token', v)} />
            </SettingsCard>
          </SettingsSection>
        </SettingsTab>

        {/* ── 一般 ─────────────────────────────────────────── */}
        <SettingsTab value="general" title={t('agents.edit.nav.general')} description={t('agents.edit.nav.general.desc')}>
          <SettingsSection title={t('agents.edit.section.identity')}>
            <SettingsCard>
              <RowText label={t('agents.edit.displayName')} description={t('agents.edit.displayName.help')} value={form.display_name ?? ''} onChange={(v) => updateField('display_name', v)} />
              <RowText label={t('agents.edit.icon')} description={t('agents.edit.icon.help')} tier="code" value={form.icon ?? ''} onChange={(v) => updateField('icon', v)} />
              <RowSelect label={t('agents.edit.role')} description={t('agents.edit.role.help')} value={form.role ?? 'specialist'} onChange={(v) => updateField('role', v)} options={roleOptions} />
              <RowText label={t('agents.edit.trigger')} description={t('agents.edit.trigger.help')} value={form.trigger ?? ''} onChange={(v) => updateField('trigger', v)} />
              <RowSelect label={t('agents.edit.reportsTo')} description={t('agents.edit.reportsTo.help')} value={form.reports_to ?? ''} onChange={(v) => updateField('reports_to', v)} options={reportsToOptions} />
              {!isPersonal && (
                <SettingsRow label={t('agents.department.label')} description={t('agents.department.help')} tier="text">
                  <Input
                    list="agent-department-options"
                    value={form.department ?? ''}
                    placeholder={t('agents.department.placeholder')}
                    onChange={(e) => updateField('department', e.target.value)}
                  />
                  <datalist id="agent-department-options">
                    {departmentOptions.map((d) => (
                      <option key={d} value={d} />
                    ))}
                  </datalist>
                </SettingsRow>
              )}
              <RowSelect label={t('agents.adv.statusField')} value={form.status ?? 'active'} onChange={(v) => updateField('status', v)} options={statusOptions} />
            </SettingsCard>
          </SettingsSection>
        </SettingsTab>

        {/* ── 模型 ─────────────────────────────────────────── */}
        <SettingsTab value="model" title={t('agents.edit.nav.model')} description={t('agents.edit.nav.model.desc')}>
          <SettingsSection title={t('agents.edit.section.model')}>
            <FieldBlock label={t('agents.edit.preferredModel')} description={t('agents.edit.preferredModel.help')}>
              <ModelSelect value={form.preferred ?? ''} onChange={(v) => updateField('preferred', v)} models={availableModels} loading={modelsLoading} error={modelsError} discoveredAt={modelsDiscoveredAt} refreshing={modelsRefreshing} onRefresh={modelsRefresh} ariaLabel={t('agents.edit.preferredModel')} />
            </FieldBlock>
            <FieldBlock label={t('agents.edit.fallbackModel')} description={t('agents.edit.fallbackModel.help')}>
              <ModelSelect value={form.fallback ?? ''} onChange={(v) => updateField('fallback', v)} models={availableModels} loading={modelsLoading} error={modelsError} discoveredAt={modelsDiscoveredAt} refreshing={modelsRefreshing} onRefresh={modelsRefresh} ariaLabel={t('agents.edit.fallbackModel')} />
            </FieldBlock>
            <SettingsCard>
              <RowSelect label={t('agents.edit.apiMode')} description={t('agents.edit.apiMode.help')} value={form.api_mode ?? 'cli'} onChange={(v) => updateField('api_mode', v as 'cli' | 'direct' | 'auto')} options={apiModeOptions} />
              <RowSwitch label={t('agents.edit.confidenceRouter')} description={t('agents.edit.confidenceRouter.help')} checked={form.use_router ?? false} onChange={(v) => updateField('use_router', v)} />
            </SettingsCard>
          </SettingsSection>

          {usesLocalModel && (
            <SettingsSection title={t('agents.edit.localInference')}>
              <SettingsCard>
                <RowSelect label={t('agents.edit.inferenceBackend')} value={form.local_backend ?? 'llama_cpp'} onChange={(v) => updateField('local_backend', v)} options={localBackendOptions} />
                <RowNumber label={t('agents.edit.contextLength')} value={form.local_context_length ?? 4096} min={512} onChange={(v) => updateField('local_context_length', v)} />
                <RowNumber label={t('agents.edit.gpuLayers')} value={form.local_gpu_layers ?? -1} min={-1} onChange={(v) => updateField('local_gpu_layers', v)} />
              </SettingsCard>
            </SettingsSection>
          )}

          <SettingsSection title={t('agents.adv.modelExtras')}>
            <FieldBlock label={t('agents.adv.accountPool')} description={t('agents.adv.accountPool.hint')}>
              <ChipEditor values={adv.account_pool} onChange={(v) => updateAdv('account_pool', v)} placeholder="oauth-pro" addLabel={t('common.add')} />
            </FieldBlock>
            <FieldBlock label={t('agents.adv.utility')} description={t('agents.adv.utility.hint')}>
              <ModelSelect value={adv.utility} onChange={(v) => updateAdv('utility', v)} models={availableModels} loading={modelsLoading} error={modelsError} discoveredAt={modelsDiscoveredAt} refreshing={modelsRefreshing} onRefresh={modelsRefresh} ariaLabel={t('agents.adv.utility')} />
            </FieldBlock>
          </SettingsSection>
        </SettingsTab>

        {/* ── 執行環境 (runtime) ────────────────────────────── */}
        <SettingsTab value="runtime" title={t('agents.edit.nav.runtime')} description={t('agents.edit.nav.runtime.desc')}>
          <SettingsSection title={t('agents.edit.group.run')} description={t('agents.runtime.desc')}>
            <SettingsCard>
              <RowSelect label={t('agents.runtime.provider')} description={t('agents.runtime.provider.hint')} value={runtime.provider} onChange={(v) => updateRuntime('provider', v as RuntimeProvider)} options={providerOptions} />
              <RowSelect label={t('agents.runtime.fallback')} description={t('agents.runtime.fallback.hint')} value={runtime.fallback} onChange={(v) => updateRuntime('fallback', v)} options={fallbackProviderOptions} />
            </SettingsCard>
          </SettingsSection>

          <SettingsSection title={t('agents.runtime.ptyTitle')} description={t('agents.runtime.pty.hint')}>
            <SettingsCard>
              <RowSwitch label={t('agents.runtime.ptyPoolEnabled')} checked={runtime.pty_pool_enabled} onChange={(v) => updateRuntime('pty_pool_enabled', v)} />
              <RowSwitch label={t('agents.runtime.workerManaged')} checked={runtime.worker_managed} onChange={(v) => updateRuntime('worker_managed', v)} />
            </SettingsCard>
            {ptyDefaultedThisSession && (
              <p className="rounded-md bg-primary/10 px-3 py-2 text-xs text-primary">{t('agents.runtime.ptyOauthDefault')}</p>
            )}
          </SettingsSection>

          <SettingsSection title={t('settings.container')}>
            <SettingsCard>
              <RowSwitch label={t('agents.edit.sandbox')} description={t('agents.edit.sandbox.help')} checked={form.sandbox_enabled ?? false} onChange={(v) => updateField('sandbox_enabled', v)} />
              <RowSwitch label={t('agents.edit.readonlyProject')} description={t('agents.edit.readonlyProject.help')} checked={form.readonly_project ?? true} onChange={(v) => updateField('readonly_project', v)} />
              <SettingsRow label={t('agents.edit.taskTimeout')} description={t('agents.edit.taskTimeout.help')} tier="select-wide">
                <DurationField seconds={Math.round((form.timeout_ms ?? 1800000) / 1000)} onChange={(s) => updateField('timeout_ms', s * 1000)} units={['sec', 'min', 'hour']} min={0} />
              </SettingsRow>
              <RowNumber label={t('agents.edit.maxConcurrent')} description={t('agents.edit.maxConcurrent.help')} value={form.max_concurrent ?? 1} min={1} max={10} onChange={(v) => updateField('max_concurrent', v)} />
              <RowSwitch label={t('agents.container.worktreeEnabled')} checked={ctAdv.worktree_enabled} onChange={(v) => updateCtAdv('worktree_enabled', v)} />
              <RowSwitch label={t('agents.container.worktreeCleanup')} checked={ctAdv.worktree_cleanup_on_exit} onChange={(v) => updateCtAdv('worktree_cleanup_on_exit', v)} />
            </SettingsCard>
            <FieldBlock label={t('agents.container.worktreeCopyFiles')} description={t('agents.container.worktreeCopyFiles.hint')}>
              <ChipEditor values={ctAdv.worktree_copy_files} onChange={(v) => updateCtAdv('worktree_copy_files', v)} placeholder=".env" addLabel={t('common.add')} />
            </FieldBlock>
            <FieldBlock label={t('agents.container.cmd')} description={t('agents.container.cmd.hint')}>
              <ChipEditor values={ctAdv.cmd} onChange={(v) => updateCtAdv('cmd', v)} placeholder="bash" addLabel={t('common.add')} />
            </FieldBlock>
            <EnvTable env={ctAdv.env} onChange={(v) => updateCtAdv('env', v)} />
          </SettingsSection>

          <DangerZone title={t('agents.container.danger.title')} description={t('agents.container.danger.desc')}>
            <SettingsCard>
              <RowSwitch label={t('agents.edit.networkAccess')} description={t('agents.edit.networkAccess.help')} checked={form.network_access ?? false} onChange={guardDanger(t('agents.edit.networkAccess'), (v) => updateField('network_access', v))} />
              <RowSwitch label={t('agents.container.worktreeAutoMerge')} description={t('agents.container.worktreeAutoMerge.help')} checked={ctAdv.worktree_auto_merge} onChange={guardDanger(t('agents.container.worktreeAutoMerge'), (v) => updateCtAdv('worktree_auto_merge', v))} />
            </SettingsCard>
            <MountTable mounts={ctAdv.additional_mounts} onChange={(v) => updateCtAdv('additional_mounts', v)} />
          </DangerZone>
        </SettingsTab>

        {/* ── 預算 ─────────────────────────────────────────── */}
        <SettingsTab value="budget" title={t('agents.edit.nav.budget')} description={t('agents.edit.nav.budget.desc')}>
          <SettingsSection title={t('agents.edit.section.budget')}>
            <SettingsCard>
              <SettingsRow label={t('agents.edit.budgetLimit')} description={t('agents.edit.budgetLimit.help')} tier="select">
                <MoneyField cents={form.monthly_limit_cents ?? 5000} onChange={(c) => updateField('monthly_limit_cents', c)} />
              </SettingsRow>
              <RowNumber label={t('agents.edit.warnThreshold')} description={t('agents.edit.warnThreshold.help')} value={form.warn_threshold_percent ?? 80} min={0} max={100} onChange={(v) => updateField('warn_threshold_percent', v)} />
              <RowSwitch label={t('agents.edit.hardStop')} description={t('agents.edit.hardStop.help')} checked={form.hard_stop ?? true} onChange={(v) => updateField('hard_stop', v)} />
            </SettingsCard>
          </SettingsSection>
        </SettingsTab>

        {/* ── 自動化 ───────────────────────────────────────── */}
        <SettingsTab value="automation" title={t('agents.edit.nav.automation')} description={t('agents.edit.nav.automation.desc')}>
          <SettingsSection title={t('agents.edit.heartbeat')}>
            <SettingsCard>
              <RowSwitch label={t('agents.edit.heartbeatEnabled')} description={t('agents.edit.heartbeatEnabled.help')} checked={form.heartbeat_enabled ?? false} onChange={(v) => updateField('heartbeat_enabled', v)} />
              <SettingsRow label={t('agents.edit.heartbeatInterval')} description={t('agents.edit.heartbeatInterval.help')} tier="select-wide">
                <DurationField seconds={form.heartbeat_interval ?? 3600} onChange={(s) => updateField('heartbeat_interval', s)} units={['sec', 'min', 'hour']} min={60} />
              </SettingsRow>
              <RowNumber label={t('agents.adv.maxConcurrentRuns')} value={adv.heartbeat_max_concurrent_runs} min={1} max={64} onChange={(v) => updateAdv('heartbeat_max_concurrent_runs', v)} />
              <RowText label={t('agents.adv.cronTimezone')} description="Asia/Taipei" tier="select-wide" value={adv.heartbeat_cron_timezone} placeholder="Asia/Taipei" onChange={(v) => updateAdv('heartbeat_cron_timezone', v)} />
            </SettingsCard>
            <FieldBlock label={t('agents.edit.heartbeatCron')} description={t('agents.edit.heartbeatCron.help')}>
              <ScheduleBuilder value={form.heartbeat_cron ?? ''} onChange={(c) => updateField('heartbeat_cron', c)} />
            </FieldBlock>
          </SettingsSection>

          <SettingsSection title={t('agents.adv.status')}>
            <SettingsCard>
              <RowNumber label={t('agents.adv.tokenBudgetPerCheck')} value={adv.proactive_token_budget_per_check} min={0} onChange={(v) => updateAdv('proactive_token_budget_per_check', v)} />
              <RowNumber label={t('agents.adv.proactiveMaxTurns')} value={adv.proactive_max_turns} min={1} max={100} onChange={(v) => updateAdv('proactive_max_turns', v)} />
              <RowText label={t('agents.adv.proactiveTimezone')} description="Asia/Taipei" tier="select-wide" value={adv.proactive_timezone} placeholder="Asia/Taipei" onChange={(v) => updateAdv('proactive_timezone', v)} />
              <RowNumber label={t('agents.edit.maxSilenceHours')} value={form.max_silence_hours ?? 12} min={1} step={0.5} onChange={(v) => updateField('max_silence_hours', v)} />
            </SettingsCard>
          </SettingsSection>

          <SettingsSection title={t('agents.evo.desc')}>
            <SettingsCard>
              <RowSwitch label={t('agents.edit.gvuEnabled')} description={t('agents.edit.gvuEnabled.help')} checked={form.gvu_enabled ?? true} onChange={(v) => updateField('gvu_enabled', v)} />
              <RowSwitch label={t('agents.edit.cognitiveMemory')} description={t('agents.edit.cognitiveMemory.help')} checked={form.cognitive_memory ?? false} onChange={(v) => updateField('cognitive_memory', v)} />
              <RowNumber label={t('agents.adv.maxGvuGenerations')} value={form.max_gvu_generations ?? 3} min={0} onChange={(v) => updateField('max_gvu_generations', v)} />
              <RowNumber label={t('agents.adv.observationHours')} value={form.observation_period_hours ?? 24} min={0} step={0.5} onChange={(v) => updateField('observation_period_hours', v)} />
            </SettingsCard>
          </SettingsSection>

          <SettingsSection title={t('agents.evo.externalFactors')}>
            <SettingsCard>
              <RowSwitch label={t('agents.evo.userFeedback')} checked={evoAdv.external_factors.user_feedback} onChange={(v) => updateEvoFactor('user_feedback', v)} />
              <RowSwitch label={t('agents.evo.securityEvents')} checked={evoAdv.external_factors.security_events} onChange={(v) => updateEvoFactor('security_events', v)} />
              <RowSwitch label={t('agents.evo.channelMetrics')} checked={evoAdv.external_factors.channel_metrics} onChange={(v) => updateEvoFactor('channel_metrics', v)} />
              <RowSwitch label={t('agents.evo.businessContext')} checked={evoAdv.external_factors.business_context} onChange={(v) => updateEvoFactor('business_context', v)} />
              <RowSwitch label={t('agents.evo.peerSignals')} checked={evoAdv.external_factors.peer_signals} onChange={(v) => updateEvoFactor('peer_signals', v)} />
            </SettingsCard>
          </SettingsSection>

          <SettingsSection title={t('agents.adv.stagnation')}>
            <SettingsCard>
              <RowSwitch label={t('agents.evo.enabled')} checked={adv.stagnation_enabled} onChange={(v) => updateAdv('stagnation_enabled', v)} />
              <RowNumber label={t('agents.adv.stagnationWindow')} value={adv.stagnation_window_seconds} min={1} onChange={(v) => updateAdv('stagnation_window_seconds', v)} />
              <RowNumber label={t('agents.adv.stagnationThreshold')} value={adv.stagnation_trigger_threshold} min={1} onChange={(v) => updateAdv('stagnation_trigger_threshold', v)} />
              <RowSelect label={t('agents.adv.stagnationAction')} value={adv.stagnation_action} onChange={(v) => updateAdv('stagnation_action', v as 'log_only' | 'suppress')} options={stagnationActionOptions} />
            </SettingsCard>
          </SettingsSection>
        </SettingsTab>

        {/* ── 進階 ─────────────────────────────────────────── */}
        <SettingsTab value="advanced" title={t('agents.edit.nav.advanced')} description={t('agents.edit.nav.advanced.desc')}>
          <SettingsSection title={t('agents.edit.sticker')} description={t('agents.edit.stickerDesc')}>
            <SettingsCard>
              <RowSwitch label={t('agents.edit.stickerEnabled')} checked={form.sticker_enabled ?? false} onChange={(v) => updateField('sticker_enabled', v)} />
              <SettingsRow label={t('agents.edit.stickerProbability')} tier="text">
                <div className="flex items-center gap-2">
                  <input type="range" min={0} max={1} step={0.05} value={form.sticker_probability ?? 0.3} onChange={(e) => updateField('sticker_probability', Number(e.target.value))} className="w-full accent-primary" aria-label={t('agents.edit.stickerProbability')} />
                  <span className="w-10 shrink-0 text-right font-mono text-xs tabular-nums text-muted-foreground">{((form.sticker_probability ?? 0.3) * 100).toFixed(0)}%</span>
                </div>
              </SettingsRow>
              <SettingsRow label={t('agents.edit.stickerIntensity')} tier="text">
                <div className="flex items-center gap-2">
                  <input type="range" min={0} max={1} step={0.05} value={form.sticker_intensity_threshold ?? 0.7} onChange={(e) => updateField('sticker_intensity_threshold', Number(e.target.value))} className="w-full accent-primary" aria-label={t('agents.edit.stickerIntensity')} />
                  <span className="w-10 shrink-0 text-right font-mono text-xs tabular-nums text-muted-foreground">{((form.sticker_intensity_threshold ?? 0.7) * 100).toFixed(0)}%</span>
                </div>
              </SettingsRow>
              <RowNumber label={t('agents.edit.stickerCooldown')} value={form.sticker_cooldown_messages ?? 5} min={0} max={100} onChange={(v) => updateField('sticker_cooldown_messages', v)} />
              <RowSelect label={t('agents.edit.stickerExpressiveness')} value={form.sticker_expressiveness ?? 'moderate'} onChange={(v) => updateField('sticker_expressiveness', v as 'minimal' | 'moderate' | 'expressive')} options={expressivenessOptions} />
            </SettingsCard>
          </SettingsSection>

          <SettingsSection title={t('agents.adv.modelExtras')} description={t('agents.adv.desc')}>
            <p className="rounded-md bg-warning/10 px-3 py-2 text-xs text-warning">{t('agents.adv.kv.warning')}</p>
            <KvTable title={t('agents.adv.ptc')} rows={adv.ptc} onChange={(v) => updateAdv('ptc', v)} />
            <KvTable title={t('agents.adv.prompt')} rows={adv.prompt} onChange={(v) => updateAdv('prompt', v)} />
            <KvTable title={t('agents.adv.culturalContext')} rows={adv.cultural_context} onChange={(v) => updateAdv('cultural_context', v)} />
          </SettingsSection>
        </SettingsTab>
      </SettingsShell>

      {/* Change 2 — shared confirm for enabling any high-risk (DangerZone)
          switch. Confirm runs the captured `apply` (the normal updater, so
          autosave fires); cancel leaves the switch off. */}
      <ConfirmDialog
        open={dangerConfirm !== null}
        onClose={() => setDangerConfirm(null)}
        onConfirm={() => { dangerConfirm?.apply(); setDangerConfirm(null); }}
        title={t('agents.edit.dangerConfirm.title')}
        message={intl.formatMessage(
          { id: 'agents.edit.dangerConfirm.message' },
          { label: dangerConfirm?.label ?? '' },
        )}
        confirmLabel={t('common.confirm')}
      />
    </div>
  );
}
