import { useIntl } from 'react-intl';
import type {
  ContainerEnvVar,
  ContainerMount,
  ToolPolicyEffect,
  ToolPolicyOp,
  ToolPolicyRule,
  ToolPolicyWhen,
} from '@/lib/api';
import { SettingField, OptionSelect, Switch as ControlSwitch, type SelectOption } from '@/components/settings/controls';
import { Button, Input } from '@/components/mds';
import { Plus, Trash2, X } from 'lucide-react';
import type { KvRow } from './defaults';

// Small shared editors for the Create / Edit Agent pages. Moved out of
// AgentsPage.tsx when the two dialogs became standalone routes.

/** Labeled on/off row — SettingField + shared Switch with a one-line help. The
 *  single replacement for the ad-hoc <Toggle> across the edit form (spec:
 *  "所有 toggle 換 Switch"). */
export function SwitchRow({
  label,
  help,
  checked,
  onChange,
  disabled,
}: {
  label: string;
  help?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <SettingField label={label} help={help} layout="row">
      <ControlSwitch checked={checked} onChange={onChange} disabled={disabled} label={label} />
    </SettingField>
  );
}

const POLICY_EFFECTS: readonly ToolPolicyEffect[] = ['allow', 'ask', 'forbid'];
const POLICY_OPS: readonly ToolPolicyOp[] = ['equals', 'contains', 'starts_with'];

/**
 * ToolPolicyEditor — a Progent-style tool-authorization rule builder. Each rule
 * names a tool (`*` = any), an effect (allow / ask / forbid), and optional
 * AND-ed argument conditions. All updates are immutable (fresh arrays). The
 * surrounding SettingField carries the "strict allowlist" explanation.
 */
export function ToolPolicyEditor({
  value,
  onChange,
}: {
  value: ToolPolicyRule[];
  onChange: (next: ToolPolicyRule[]) => void;
}) {
  const intl = useIntl();
  const effectOptions: SelectOption[] = POLICY_EFFECTS.map((e) => ({
    value: e,
    label: intl.formatMessage({ id: `agents.cap.policy.effect.${e}` }),
  }));
  const opOptions: SelectOption[] = POLICY_OPS.map((o) => ({
    value: o,
    label: intl.formatMessage({ id: `agents.cap.policy.op.${o}` }),
  }));

  const updateRule = (idx: number, patch: Partial<ToolPolicyRule>) =>
    onChange(value.map((r, i) => (i === idx ? { ...r, ...patch } : r)));
  const removeRule = (idx: number) => onChange(value.filter((_, i) => i !== idx));
  const addRule = () => onChange([...value, { tool: '*', effect: 'allow', when: [] }]);

  const whenOf = (ri: number): ToolPolicyWhen[] => value[ri].when ?? [];
  const updateWhen = (ri: number, wi: number, patch: Partial<ToolPolicyWhen>) =>
    updateRule(ri, { when: whenOf(ri).map((w, i) => (i === wi ? { ...w, ...patch } : w)) });
  const addWhen = (ri: number) =>
    updateRule(ri, { when: [...whenOf(ri), { arg: '', op: 'contains', value: '' }] });
  const removeWhen = (ri: number, wi: number) =>
    updateRule(ri, { when: whenOf(ri).filter((_, i) => i !== wi) });

  return (
    <div className="space-y-3">
      {value.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'agents.cap.policy.empty' })}
        </p>
      ) : (
        value.map((rule, ri) => (
          <div key={ri} className="space-y-2 rounded-lg border border-surface-border p-3">
            <div className="flex flex-wrap items-center gap-2">
              <Input
                className="min-w-[8rem] flex-1"
                value={rule.tool}
                placeholder="*"
                aria-label={intl.formatMessage({ id: 'agents.cap.policy.tool' })}
                onChange={(e) => updateRule(ri, { tool: e.target.value })}
              />
              <div className="w-36">
                <OptionSelect
                  value={rule.effect}
                  onChange={(v) => updateRule(ri, { effect: v as ToolPolicyEffect })}
                  options={effectOptions}
                  showRaw={false}
                />
              </div>
              <button
                type="button"
                onClick={() => removeRule(ri)}
                className="rounded-md p-1.5 text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                aria-label={intl.formatMessage({ id: 'agents.cap.policy.removeRule' })}
              >
                <X className="h-4 w-4" />
              </button>
            </div>

            {whenOf(ri).map((w, wi) => (
              <div key={wi} className="flex flex-wrap items-center gap-2 pl-3">
                <span className="text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'agents.cap.policy.when' })}
                </span>
                <Input
                  className="min-w-[6rem] flex-1"
                  value={w.arg}
                  placeholder={intl.formatMessage({ id: 'agents.cap.policy.arg' })}
                  aria-label={intl.formatMessage({ id: 'agents.cap.policy.arg' })}
                  onChange={(e) => updateWhen(ri, wi, { arg: e.target.value })}
                />
                <div className="w-32">
                  <OptionSelect
                    value={w.op}
                    onChange={(v) => updateWhen(ri, wi, { op: v as ToolPolicyOp })}
                    options={opOptions}
                    showRaw={false}
                  />
                </div>
                <Input
                  className="min-w-[6rem] flex-1"
                  value={w.value}
                  placeholder={intl.formatMessage({ id: 'agents.cap.policy.value' })}
                  aria-label={intl.formatMessage({ id: 'agents.cap.policy.value' })}
                  onChange={(e) => updateWhen(ri, wi, { value: e.target.value })}
                />
                <button
                  type="button"
                  onClick={() => removeWhen(ri, wi)}
                  className="rounded-md p-1.5 text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                  aria-label={intl.formatMessage({ id: 'agents.cap.policy.removeCondition' })}
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              </div>
            ))}

            <button
              type="button"
              onClick={() => addWhen(ri)}
              className="ml-3 inline-flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-brand"
            >
              <Plus className="h-3 w-3" />
              {intl.formatMessage({ id: 'agents.cap.policy.addCondition' })}
            </button>
          </div>
        ))
      )}

      <button
        type="button"
        onClick={addRule}
        className="inline-flex items-center gap-1.5 rounded-lg border border-dashed border-surface-border px-3 py-2 text-xs font-medium text-muted-foreground hover:border-brand hover:text-brand"
      >
        <Plus className="h-3.5 w-3.5" />
        {intl.formatMessage({ id: 'agents.cap.policy.addRule' })}
      </button>
    </div>
  );
}

// ── CT — additional_mounts table editor ──

export function MountTable({ mounts, onChange }: { mounts: ReadonlyArray<ContainerMount>; onChange: (next: ContainerMount[]) => void }) {
  const intl = useIntl();
  const update = (idx: number, patch: Partial<ContainerMount>) =>
    onChange(mounts.map((m, i) => (i === idx ? { ...m, ...patch } : m)));
  const remove = (idx: number) => onChange(mounts.filter((_, i) => i !== idx));
  const add = () => onChange([...mounts, { host: '', container: '', readonly: true }]);

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <h4 className="text-xs font-semibold uppercase text-muted-foreground">{intl.formatMessage({ id: 'agents.container.mounts' })}</h4>
        <Button type="button" size="sm" variant="ghost" onClick={add}>
          <Plus />
          {intl.formatMessage({ id: 'common.add' })}
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'agents.container.mounts.hint' })}</p>
      {mounts.length === 0 ? (
        <p className="py-2 text-center text-xs text-muted-foreground">{intl.formatMessage({ id: 'agents.container.mounts.empty' })}</p>
      ) : (
        <div className="space-y-2">
          {mounts.map((m, idx) => (
            <div key={idx} className="flex items-center gap-2">
              <Input value={m.host} onChange={(e) => update(idx, { host: e.target.value })} placeholder={intl.formatMessage({ id: 'agents.container.mounts.host' })} className="flex-1" />
              <Input value={m.container} onChange={(e) => update(idx, { container: e.target.value })} placeholder={intl.formatMessage({ id: 'agents.container.mounts.container' })} className="flex-1" />
              <label className="flex shrink-0 items-center gap-1 text-xs text-muted-foreground">
                <input type="checkbox" checked={m.readonly} onChange={(e) => update(idx, { readonly: e.target.checked })} className="accent-brand" />
                {intl.formatMessage({ id: 'agents.container.mounts.readonly' })}
              </label>
              <Button type="button" size="icon-sm" variant="ghost" onClick={() => remove(idx)} className="shrink-0 text-destructive hover:bg-destructive/10 hover:text-destructive" aria-label="remove mount"><Trash2 /></Button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ── Advanced — generic key/value scalar table editor (G.8 ptc/prompt/cultural) ──

export function KvTable({ title, rows, onChange }: { title: string; rows: ReadonlyArray<KvRow>; onChange: (next: KvRow[]) => void }) {
  const intl = useIntl();
  const update = (idx: number, patch: Partial<KvRow>) =>
    onChange(rows.map((r, i) => (i === idx ? { ...r, ...patch } : r)));
  const remove = (idx: number) => onChange(rows.filter((_, i) => i !== idx));
  const add = () => onChange([...rows, { key: '', value: '' }]);

  return (
    <div className="space-y-2 border-t border-surface-border pt-4">
      <div className="flex items-center justify-between">
        <h4 className="text-xs font-semibold uppercase text-muted-foreground">{title}</h4>
        <Button type="button" size="sm" variant="ghost" onClick={add}>
          <Plus />
          {intl.formatMessage({ id: 'common.add' })}
        </Button>
      </div>
      {rows.length === 0 ? (
        <p className="py-1 text-center text-xs text-muted-foreground">{intl.formatMessage({ id: 'agents.adv.kv.empty' })}</p>
      ) : (
        <div className="space-y-2">
          {rows.map((r, idx) => (
            <div key={idx} className="flex items-center gap-2">
              <Input value={r.key} onChange={(e) => update(idx, { key: e.target.value })} placeholder="key" className="flex-1" />
              <Input value={r.value} onChange={(e) => update(idx, { value: e.target.value })} placeholder="value" className="flex-1" />
              <Button type="button" size="icon-sm" variant="ghost" onClick={() => remove(idx)} className="shrink-0 text-destructive hover:bg-destructive/10 hover:text-destructive" aria-label="remove row"><Trash2 /></Button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ── CT — env table editor ──

export function EnvTable({ env, onChange }: { env: ReadonlyArray<ContainerEnvVar>; onChange: (next: ContainerEnvVar[]) => void }) {
  const intl = useIntl();
  const update = (idx: number, patch: Partial<ContainerEnvVar>) =>
    onChange(env.map((e, i) => (i === idx ? { ...e, ...patch } : e)));
  const remove = (idx: number) => onChange(env.filter((_, i) => i !== idx));
  const add = () => onChange([...env, { key: '', value: '' }]);

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <h4 className="text-xs font-semibold uppercase text-muted-foreground">{intl.formatMessage({ id: 'agents.container.env' })}</h4>
        <Button type="button" size="sm" variant="ghost" onClick={add}>
          <Plus />
          {intl.formatMessage({ id: 'common.add' })}
        </Button>
      </div>
      {env.length === 0 ? (
        <p className="py-2 text-center text-xs text-muted-foreground">{intl.formatMessage({ id: 'agents.container.env.empty' })}</p>
      ) : (
        <div className="space-y-2">
          {env.map((e, idx) => (
            <div key={idx} className="flex items-center gap-2">
              <Input value={e.key} onChange={(ev) => update(idx, { key: ev.target.value })} placeholder="KEY" className="flex-1" />
              <Input value={e.value} onChange={(ev) => update(idx, { value: ev.target.value })} placeholder="value" className="flex-1" />
              <Button type="button" size="icon-sm" variant="ghost" onClick={() => remove(idx)} className="shrink-0 text-destructive hover:bg-destructive/10 hover:text-destructive" aria-label="remove env"><Trash2 /></Button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
