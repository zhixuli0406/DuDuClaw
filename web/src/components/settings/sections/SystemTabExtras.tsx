import { useState } from 'react';
import { useIntl } from 'react-intl';
import { Plus, X } from 'lucide-react';
import { Button, Input, SettingsCard, SettingsRow, SettingsSection } from '@/components/mds';
import { AdvancedSection, type SelectOption } from '@/components/settings/controls';
import { RowSelect, RowSwitch } from '@/pages/agent-form/form-rows';
import { isImeComposing } from '@/lib/keyboard';
import type { FlatValues } from '@/lib/config-toml';
import { vBool, vList, vNum, vStr } from '../useConfigForm';

/**
 * v1.68 (W2) additions to 設定 → 系統. Presentational: `SystemTab` owns the
 * flat value map, loads it from `system.config` and sends only the changed
 * keys. Keys are the `system.update_config` param paths (= TOML paths).
 */

const MIB = 1024 * 1024;

// `[container.sandbox]` defaults — task_sandbox.rs / CLAUDE.md v1.67.0.
export const SANDBOX_DEFAULTS = {
  memory_bytes: 4096 * MIB,
  pids: 128,
  cpu_millis: 1000,
  tmp_bytes: 256 * MIB,
  workspace_bytes: 512 * MIB,
  max_turns: 30,
} as const;
const SANDBOX_MAX_MEMORY_MIB = 64 * 1024;
const SANDBOX_MAX_TMPFS_MIB = 16 * 1024;
const UNAVAILABLE_MODES = ['fail', 'run_unsandboxed'] as const;

type SetFn = (key: string, value: unknown) => void;

/** tmp + workspace must fit in memory (both tmpfs count against it). */
export function sandboxSizesInvalid(values: FlatValues): boolean {
  const mem = vNum(values, 'container.sandbox.memory_bytes', SANDBOX_DEFAULTS.memory_bytes);
  const tmp = vNum(values, 'container.sandbox.tmp_bytes', SANDBOX_DEFAULTS.tmp_bytes);
  const ws = vNum(values, 'container.sandbox.workspace_bytes', SANDBOX_DEFAULTS.workspace_bytes);
  return tmp + ws > mem;
}

function RowNum({
  label,
  description,
  value,
  onChange,
  min,
  max,
  suffix,
}: {
  label: string;
  description?: string;
  value: number;
  onChange: (n: number) => void;
  min?: number;
  max?: number;
  suffix?: string;
}) {
  return (
    <SettingsRow label={label} description={description} tier="select">
      <div className="flex items-center gap-2">
        <Input
          type="number"
          value={Number.isFinite(value) ? value : ''}
          min={min}
          max={max}
          aria-label={label}
          onChange={(e) => onChange(e.target.value === '' ? Number.NaN : Number(e.target.value))}
        />
        {suffix && <span className="shrink-0 text-xs text-muted-foreground">{suffix}</span>}
      </div>
    </SettingsRow>
  );
}

function RowTextValue({
  label,
  description,
  value,
  onChange,
  placeholder,
}: {
  label: string;
  description?: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <SettingsRow label={label} description={description} tier="text">
      <Input type="text" value={value} placeholder={placeholder} aria-label={label} onChange={(e) => onChange(e.target.value)} />
    </SettingsRow>
  );
}

export function SandboxSection({ values, set }: { values: FlatValues; set: SetFn }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const modeOptions = (prefix: string): SelectOption[] =>
    UNAVAILABLE_MODES.map((m) => ({ value: m, label: t(`${prefix}.${m}`), raw: m }));
  const mib = (key: string, fallback: number) => Math.round(vNum(values, key, fallback) / MIB);
  const setMib = (key: string) => (n: number) => set(key, Number.isFinite(n) ? Math.round(n) * MIB : Number.NaN);
  const invalid = sandboxSizesInvalid(values);

  return (
    <SettingsSection title={t('settings.sandbox')} description={t('settings.sandbox.desc')}>
      <SettingsCard>
        <RowTextValue
          label={t('settings.sandbox.image')}
          description={t('settings.sandbox.image.help')}
          value={vStr(values, 'container.sandbox.image')}
          onChange={(v) => set('container.sandbox.image', v.trim())}
          placeholder={t('settings.sandbox.image.placeholder')}
        />
        <RowSelect
          label={t('settings.sandbox.whenUnavailable')}
          description={t('settings.sandbox.whenUnavailable.help')}
          value={vStr(values, 'container.sandbox.when_unavailable') || 'fail'}
          onChange={(v) => set('container.sandbox.when_unavailable', v)}
          options={modeOptions('settings.sandbox.mode')}
        />
        <RowSelect
          label={t('settings.sandbox.scriptWhenUnavailable')}
          description={t('settings.sandbox.scriptWhenUnavailable.help')}
          value={vStr(values, 'container.sandbox.script_when_unavailable') || 'fail'}
          onChange={(v) => set('container.sandbox.script_when_unavailable', v)}
          options={modeOptions('settings.sandbox.mode')}
        />
      </SettingsCard>
      <AdvancedSection storageKey="settings.system.sandboxLimits" label={t('settings.sandbox.limits')}>
        <SettingsCard>
          <RowNum
            label={t('settings.sandbox.memory')}
            value={mib('container.sandbox.memory_bytes', SANDBOX_DEFAULTS.memory_bytes)}
            min={64}
            max={SANDBOX_MAX_MEMORY_MIB}
            suffix="MiB"
            onChange={setMib('container.sandbox.memory_bytes')}
          />
          <RowNum
            label={t('settings.sandbox.tmp')}
            value={mib('container.sandbox.tmp_bytes', SANDBOX_DEFAULTS.tmp_bytes)}
            min={1}
            max={SANDBOX_MAX_TMPFS_MIB}
            suffix="MiB"
            onChange={setMib('container.sandbox.tmp_bytes')}
          />
          <RowNum
            label={t('settings.sandbox.workspace')}
            value={mib('container.sandbox.workspace_bytes', SANDBOX_DEFAULTS.workspace_bytes)}
            min={1}
            max={SANDBOX_MAX_TMPFS_MIB}
            suffix="MiB"
            onChange={setMib('container.sandbox.workspace_bytes')}
          />
          <RowNum
            label={t('settings.sandbox.pids')}
            value={vNum(values, 'container.sandbox.pids', SANDBOX_DEFAULTS.pids)}
            min={16}
            onChange={(n) => set('container.sandbox.pids', n)}
          />
          <RowNum
            label={t('settings.sandbox.cpu')}
            description={t('settings.sandbox.cpu.help')}
            value={vNum(values, 'container.sandbox.cpu_millis', SANDBOX_DEFAULTS.cpu_millis)}
            min={100}
            onChange={(n) => set('container.sandbox.cpu_millis', n)}
          />
          <RowNum
            label={t('settings.sandbox.maxTurns')}
            value={vNum(values, 'container.sandbox.max_turns', SANDBOX_DEFAULTS.max_turns)}
            min={1}
            onChange={(n) => set('container.sandbox.max_turns', n)}
          />
        </SettingsCard>
        <p className={invalid ? 'mt-2 text-xs text-destructive' : 'mt-2 text-xs text-muted-foreground'} role={invalid ? 'alert' : undefined}>
          {t('settings.sandbox.sizeConstraint')}
        </p>
      </AdvancedSection>
      <SettingsCard>
        <RowTextValue
          label={t('settings.computerUse.image')}
          description={t('settings.computerUse.image.help')}
          value={vStr(values, 'computer_use.image')}
          onChange={(v) => set('computer_use.image', v.trim())}
          placeholder={t('settings.computerUse.image.placeholder')}
        />
      </SettingsCard>
    </SettingsSection>
  );
}

/** Absolute path check: POSIX `/…`, Windows `C:\…` / `C:/…`, or `\\server\…`. */
export function isAbsolutePath(p: string): boolean {
  return /^\//.test(p) || /^[A-Za-z]:[\\/]/.test(p) || /^\\\\/.test(p);
}

export function FilesRootsSection({ values, set }: { values: FlatValues; set: SetFn }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [draft, setDraft] = useState('');
  const [error, setError] = useState<string | null>(null);
  const roots = vList(values, 'files.allowed_roots');

  const add = () => {
    const v = draft.trim();
    if (v === '') return;
    if (!isAbsolutePath(v)) {
      setError(t('settings.files.roots.notAbsolute'));
      return;
    }
    setError(null);
    if (!roots.includes(v)) set('files.allowed_roots', [...roots, v]);
    setDraft('');
  };

  return (
    <SettingsSection title={t('settings.files.roots')} description={t('settings.files.roots.desc')}>
      <SettingsCard>
        <SettingsRow label={t('settings.files.roots.add')} tier="text">
          <div className="flex gap-2">
            <Input
              type="text"
              value={draft}
              placeholder="/srv/exports"
              aria-label={t('settings.files.roots.add')}
              onChange={(e) => {
                setDraft(e.target.value);
                setError(null);
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !isImeComposing(e)) {
                  e.preventDefault();
                  add();
                }
              }}
            />
            <Button
              type="button"
              variant="secondary"
              size="sm"
              onClick={add}
              disabled={draft.trim() === ''}
              aria-label={t('settings.files.roots.addButton')}
            >
              <Plus className="size-4" />
              {t('common.add')}
            </Button>
          </div>
        </SettingsRow>
        <div className="px-4 py-3.5">
          {error && <p role="alert" className="mb-2 text-xs text-destructive">{error}</p>}
          {roots.length === 0 ? (
            <p className="text-xs text-muted-foreground">{t('settings.files.roots.empty')}</p>
          ) : (
            <ul className="space-y-1" aria-label={t('settings.files.roots')}>
              {roots.map((r) => (
                <li key={r} className="flex items-center justify-between gap-2 font-mono text-xs">
                  <span className="truncate">{r}</span>
                  <button
                    type="button"
                    onClick={() => set('files.allowed_roots', roots.filter((x) => x !== r))}
                    className="rounded-full text-muted-foreground hover:text-destructive"
                    aria-label={intl.formatMessage({ id: 'settings.files.roots.remove' }, { path: r })}
                  >
                    <X className="size-3.5" />
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </SettingsCard>
    </SettingsSection>
  );
}

export function TrustAndObservabilitySection({
  values,
  set,
  otelCompiled,
  isAdmin,
}: {
  values: FlatValues;
  set: SetFn;
  otelCompiled: boolean;
  isAdmin: boolean;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const acpOn = vBool(values, 'acp.trusted');
  return (
    <SettingsSection title={t('settings.observability')}>
      <SettingsCard>
        <RowSwitch
          label={t('settings.memory.trustGuard')}
          description={t('settings.memory.trustGuard.help')}
          checked={vBool(values, 'memory.supersession_trust_guard')}
          onChange={(v) => set('memory.supersession_trust_guard', v)}
        />
        {otelCompiled ? (
          <RowTextValue
            label={t('settings.telemetry.otlp')}
            description={`${t('settings.telemetry.otlp.help')} ${t('settings.restartNote')}`}
            value={vStr(values, 'telemetry.otlp_endpoint')}
            onChange={(v) => set('telemetry.otlp_endpoint', v.trim())}
            placeholder="http://localhost:4317"
          />
        ) : (
          <SettingsRow label={t('settings.telemetry.otlp')}>
            <span className="text-xs text-muted-foreground" data-testid="otel-not-compiled">
              {t('settings.telemetry.notCompiled')}
            </span>
          </SettingsRow>
        )}
        <RowSwitch
          label={t('settings.integrations.github')}
          description={t('settings.integrations.github.help')}
          checked={vBool(values, 'integrations.github')}
          onChange={(v) => set('integrations.github', v)}
        />
        {isAdmin && (
          <RowSwitch
            label={t('settings.acp.trusted')}
            description={t('settings.acp.trusted.help')}
            checked={acpOn}
            onChange={(v) => set('acp.trusted', v)}
          />
        )}
      </SettingsCard>
      {isAdmin && acpOn && (
        <p role="alert" className="rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-xs">
          {t('settings.acp.trusted.warning')}
        </p>
      )}
    </SettingsSection>
  );
}

/** onepassword / infisical connection fields (tokens are write-only). */
export function SecretBackendFields({
  backend,
  values,
  set,
  tokens,
  setToken,
  tokenSet,
}: {
  backend: string;
  values: FlatValues;
  set: SetFn;
  tokens: { onepassword_token: string; infisical_token: string };
  setToken: (key: 'onepassword_token' | 'infisical_token', v: string) => void;
  tokenSet: { onepassword_token: boolean; infisical_token: boolean };
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const secretRow = (key: 'onepassword_token' | 'infisical_token', label: string) => (
    <SettingsRow
      label={label}
      description={tokenSet[key] ? t('settings.system.secretStored') : t('settings.system.writeOnly')}
      tier="text"
    >
      <Input
        type="password"
        value={tokens[key]}
        autoComplete="off"
        placeholder="••••••••"
        aria-label={label}
        onChange={(e) => setToken(key, e.target.value)}
      />
    </SettingsRow>
  );
  if (backend === 'onepassword') {
    return (
      <>
        <RowTextValue
          label={t('settings.system.onepasswordHost')}
          value={vStr(values, 'secret_manager.onepassword_host')}
          onChange={(v) => set('secret_manager.onepassword_host', v.trim())}
          placeholder="http://localhost:8080"
        />
        {secretRow('onepassword_token', t('settings.system.onepasswordToken'))}
        <RowTextValue
          label={t('settings.system.onepasswordVault')}
          value={vStr(values, 'secret_manager.onepassword_vault')}
          onChange={(v) => set('secret_manager.onepassword_vault', v.trim())}
        />
      </>
    );
  }
  if (backend === 'infisical') {
    return (
      <>
        <RowTextValue
          label={t('settings.system.infisicalAddr')}
          value={vStr(values, 'secret_manager.infisical_addr')}
          onChange={(v) => set('secret_manager.infisical_addr', v.trim())}
          placeholder="https://app.infisical.com"
        />
        {secretRow('infisical_token', t('settings.system.infisicalToken'))}
        <RowTextValue
          label={t('settings.system.infisicalProject')}
          value={vStr(values, 'secret_manager.infisical_project_id')}
          onChange={(v) => set('secret_manager.infisical_project_id', v.trim())}
        />
        <RowTextValue
          label={t('settings.system.infisicalEnv')}
          value={vStr(values, 'secret_manager.infisical_environment')}
          onChange={(v) => set('secret_manager.infisical_environment', v.trim())}
          placeholder="prod"
        />
      </>
    );
  }
  return null;
}
