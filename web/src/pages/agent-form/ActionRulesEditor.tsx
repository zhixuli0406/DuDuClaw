import { useState } from 'react';
import { useIntl } from 'react-intl';
import { Plus, Trash2 } from 'lucide-react';
import type { ActionRule, ActionVerdict } from '@/lib/api';
import { Button, Input, SettingsCard } from '@/components/mds';
import { selectClass } from '@/components/shared/controlClass';
import { cn } from '@/lib/utils';
import { RowSelect, FieldBlock } from './form-rows';
import {
  ACTION_RULE_TOOL_RE,
  ACTION_VERDICTS,
  TOOL_EFFECTS,
  effectVerdict,
  setEffectVerdict,
  setToolRules,
  toolRules,
} from './actionRules';

// 2026-10 — `[capabilities] action_rules`: one row per kind of action
// (allow / ask / block, or no rule), plus optional per-tool overrides.
// A rule only adds friction; the server never lets `allow` lift an approval
// or a denial set in the tool lists below.

const DEFAULT_VALUE = 'default';

export function ActionRulesEditor({
  rules,
  onChange,
  disabled,
}: {
  rules: ReadonlyArray<ActionRule>;
  onChange: (next: ActionRule[]) => void;
  disabled?: boolean;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string>) => intl.formatMessage({ id }, values);
  const verdictOptions = [
    { value: DEFAULT_VALUE, label: t('agents.actionRules.verdict.default') },
    ...ACTION_VERDICTS.map((v) => ({ value: v, label: t(`agents.actionRules.verdict.${v}`) })),
  ];
  const overrides = toolRules(rules);
  const [draftTool, setDraftTool] = useState('');
  const [draftVerdict, setDraftVerdict] = useState<ActionVerdict>('ask');
  const draftInvalid = draftTool.trim() !== '' && !ACTION_RULE_TOOL_RE.test(draftTool.trim());

  const addOverride = () => {
    const tool = draftTool.trim();
    if (tool === '' || !ACTION_RULE_TOOL_RE.test(tool)) return;
    onChange(setToolRules(rules, [...overrides.filter((r) => r.tool !== tool), { tool, verdict: draftVerdict }]));
    setDraftTool('');
  };

  return (
    <div className="space-y-3">
      <SettingsCard>
        {TOOL_EFFECTS.map((effect) => (
          <RowSelect
            key={effect}
            label={t(`agents.actionRules.effect.${effect}`)}
            description={t(`agents.actionRules.effect.${effect}.help`)}
            value={effectVerdict(rules, effect) || DEFAULT_VALUE}
            onChange={(v) =>
              onChange(setEffectVerdict(rules, effect, v === DEFAULT_VALUE ? '' : (v as ActionVerdict)))
            }
            options={verdictOptions}
            disabled={disabled}
            tier="select"
          />
        ))}
      </SettingsCard>

      <FieldBlock label={t('agents.actionRules.overrides')} description={t('agents.actionRules.overrides.help')}>
        <div className="space-y-2">
          {overrides.length === 0 && (
            <p className="text-xs text-muted-foreground">{t('agents.actionRules.overrides.empty')}</p>
          )}
          {overrides.map((r) => (
            <div key={r.tool} className="flex items-center gap-2">
              <code className="min-w-0 flex-1 truncate rounded-md bg-muted/50 px-2 py-1 text-xs">{r.tool}</code>
              <select
                className={cn(selectClass, 'w-32 shrink-0')}
                value={r.verdict}
                disabled={disabled}
                aria-label={t('agents.actionRules.overrides.verdictFor', { tool: r.tool ?? '' })}
                onChange={(e) =>
                  onChange(
                    setToolRules(
                      rules,
                      overrides.map((o) => (o.tool === r.tool ? { ...o, verdict: e.target.value as ActionVerdict } : o)),
                    ),
                  )
                }
              >
                {ACTION_VERDICTS.map((v) => (
                  <option key={v} value={v}>
                    {t(`agents.actionRules.verdict.${v}`)}
                  </option>
                ))}
              </select>
              {!disabled && (
                <Button
                  type="button"
                  size="icon-sm"
                  variant="ghost"
                  onClick={() => onChange(setToolRules(rules, overrides.filter((o) => o.tool !== r.tool)))}
                  className="shrink-0 text-destructive hover:bg-destructive/10 hover:text-destructive"
                  aria-label={t('agents.actionRules.overrides.remove', { tool: r.tool ?? '' })}
                >
                  <Trash2 />
                </Button>
              )}
            </div>
          ))}
          {!disabled && (
            <div className="flex flex-wrap items-center gap-2 sm:flex-nowrap">
              <Input
                value={draftTool}
                onChange={(e) => setDraftTool(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    addOverride();
                  }
                }}
                placeholder="mail_send"
                aria-label={t('agents.actionRules.overrides.tool')}
                aria-invalid={draftInvalid}
                className="min-w-0 flex-1"
              />
              <select
                className={cn(selectClass, 'w-32 shrink-0')}
                value={draftVerdict}
                aria-label={t('agents.actionRules.overrides.verdict')}
                onChange={(e) => setDraftVerdict(e.target.value as ActionVerdict)}
              >
                {ACTION_VERDICTS.map((v) => (
                  <option key={v} value={v}>
                    {t(`agents.actionRules.verdict.${v}`)}
                  </option>
                ))}
              </select>
              <Button type="button" size="sm" variant="ghost" onClick={addOverride} disabled={draftTool.trim() === '' || draftInvalid}>
                <Plus />
                {t('common.add')}
              </Button>
            </div>
          )}
          {draftInvalid && (
            <p role="alert" className="text-xs text-destructive">
              {t('agents.actionRules.overrides.invalid', { tool: draftTool.trim() })}
            </p>
          )}
        </div>
      </FieldBlock>
    </div>
  );
}
