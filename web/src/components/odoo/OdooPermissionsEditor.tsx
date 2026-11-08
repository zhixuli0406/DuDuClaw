import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Badge, Button, Checkbox, Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';
import { useOdooPermissionOptions } from './useOdooPermissionOptions';
import { actionAccess, ACTION_GROUPS, normalizeActions, parseActions, partialRead, reconcileUnblocks, serializeRules, usedModels, type OdooPermissions, type OdooRule } from './permissions';

export function OdooPermissionsEditor({ value, onChange, disabled = false, defaultAccount = true }: {
  value: OdooPermissions; onChange: (value: OdooPermissions) => void; disabled?: boolean; defaultAccount?: boolean;
}) {
  const intl = useIntl();
  const t = (key: string) => intl.formatMessage({ id: `odoo.permissions.${key}` });
  const { options, policyReady, scanning, failed, scan } = useOdooPermissionOptions(!disabled);
  const [filter, setFilter] = useState('');
  const configuredTables = useRef(new Set([...usedModels(value), ...value.unblock_models]));
  const [rules, setRules] = useState(() => parseActions(value.allowed_actions).rules);
  const lastActions = useRef(JSON.stringify(value.allowed_actions));
  const draftApprovals = useRef(new Set<string>());
  const [pending, setPending] = useState<{ model: string; apply: () => void } | null>(null);
  // Keep empty draft rules while editing, but reload when the parent changes employee/config.
  useEffect(() => {
    const signature = JSON.stringify(value.allowed_actions);
    if (signature !== lastActions.current) {
      if (signature === JSON.stringify(normalizeActions(JSON.parse(lastActions.current) as string[]))) {
        // Save acknowledgement: normalize existing rules without losing unsaved drafts.
        setRules((current) => current.map((rule) => rule.verbs.length
          ? parseActions(serializeRules([rule], [], true)).rules[0] : rule));
      } else {
        setRules(parseActions(value.allowed_actions).rules);
        draftApprovals.current.clear();
        setPending(null);
      }
      lastActions.current = signature;
    }
  }, [value.allowed_actions]);
  const { other } = parseActions(value.allowed_actions);
  for (const model of [...usedModels(value), ...value.unblock_models, ...rules.map((r) => r.model).filter(Boolean)]) configuredTables.current.add(model);
  const tableNames = [...new Set([...options.models.map((m) => m.model), ...options.blocked_models,
    ...configuredTables.current])].sort();
  const tableLabel = (model: string) => {
    const name = options.models.find((m) => m.model === model)?.name;
    return `${name && name !== model ? `${name} (${model})` : model}${options.blocked_models.includes(model) ? ` · ${t('sensitive')}` : ''}${options.read_only_models.includes(model) ? ` · ${t('readOnly')}` : ''}`;
  };
  function emit(next: OdooPermissions, nextRules = rules) {
    // A draft with no actions is not stored: never persist its sensitive grant alone.
    const used = usedModels(next);
    next.unblock_models = reconcileUnblocks(value, next, options.blocked_models)
      .filter((model) => value.unblock_models.includes(model) || used.has(model));
    for (const model of draftApprovals.current) {
      if (!next.allowed_models.includes(model) && !nextRules.some((rule) => rule.model === model)) draftApprovals.current.delete(model);
    }
    lastActions.current = JSON.stringify(next.allowed_actions);
    setRules(nextRules);
    onChange(next);
  }
  function choose(model: string, apply: (unblocks: string[]) => void) {
    if (disabled || !policyReady) return;
    if (options.blocked_models.includes(model) && !value.unblock_models.includes(model)) {
      const accept = () => { draftApprovals.current.add(model); apply([...new Set([...value.unblock_models, model])]); };
      if (draftApprovals.current.has(model)) accept();
      else setPending({ model, apply: accept });
    } else apply(value.unblock_models);
  }
  function changeRules(next: OdooRule[], model?: string) {
    const apply = (unblocks: string[]) => emit({ ...value, allowed_actions: serializeRules(next, other), unblock_models: unblocks }, next);
    if (model) choose(model, apply); else apply(value.unblock_models);
  }
  const locked = disabled || !policyReady;
  const standalone = value.unblock_models.filter((model) => !usedModels(value).has(model));
  return <div className="space-y-5 py-3">
    {(!options.scan_available || failed) && <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground" role="status">
      <span>{t(failed ? 'scanFailed' : 'scanMissing')}</span>
      <Button variant="outline" size="sm" onClick={() => void scan()} disabled={disabled || scanning}>{t(scanning ? 'scanning' : 'scan')}</Button>
    </div>}
    {options.scan_available && !failed && <Button variant="outline" size="sm" onClick={() => void scan()} disabled={disabled || scanning}>{t(scanning ? 'scanning' : 'rescan')}</Button>}
    {!defaultAccount && <p className="text-xs text-muted-foreground">{t('otherAccountRights')}</p>}
    {!policyReady && <p role="status" className="text-sm text-muted-foreground">{t('policyUnavailable')}</p>}
    <fieldset className="space-y-3" disabled={disabled}>
      <legend className="text-sm font-medium">{t('tables')}</legend>
      <p className="text-xs text-muted-foreground">{t('modelsUnrestricted')}</p>
      <Input aria-label={t('filter')} placeholder={t('filter')} value={filter} onChange={(e) => setFilter(e.target.value)} />
      <div className="grid max-h-64 gap-3 overflow-y-auto sm:grid-cols-2">
        {tableNames.filter((model) => tableLabel(model).toLowerCase().includes(filter.toLowerCase())).map((model) => <label key={model} className="flex items-start gap-2 text-sm">
          <Checkbox disabled={locked} checked={value.allowed_models.includes(model)} onCheckedChange={(checked) => {
            if (checked) choose(model, (unblocks) => emit({ ...value, allowed_models: [...value.allowed_models, model], unblock_models: unblocks }));
            else emit({ ...value, allowed_models: value.allowed_models.filter((m) => m !== model) });
          }} />
          <span className="break-all">{tableLabel(model)}</span>
        </label>)}
      </div>
    </fieldset>
    <fieldset className="space-y-3" disabled={disabled}>
      <legend className="text-sm font-medium">{t('rules')}</legend>
      <p className="text-xs text-muted-foreground">{t('actionsUnrestricted')}</p>
      <p className="text-xs text-muted-foreground">{t('intersection')}</p>
      <p className="text-xs text-muted-foreground">{t('genericWriteRule')}</p>
      {rules.map((rule, index) => <fieldset key={index} className="space-y-3 rounded-lg border border-border p-3">
        <legend className="px-1 text-xs">{t('rule')} {index + 1}</legend>
        <div className="flex items-center gap-2">
          <Select value={rule.model} disabled={locked} onValueChange={(model) => {
            if (model === null) return;
            changeRules(rules.map((r, i) => i === index ? { ...r, model } : r), model);
          }}>
            <SelectTrigger className="min-w-0 flex-1" aria-label={`${t('table')} ${index + 1}`}><SelectValue>{rule.model ? tableLabel(rule.model) : t('allTables')}</SelectValue></SelectTrigger>
            <SelectContent><SelectItem value="">{t('allTables')}</SelectItem>
              {tableNames.map((model) => <SelectItem key={model} value={model}>{tableLabel(model)}</SelectItem>)}
            </SelectContent>
          </Select>
          <Button variant="ghost" size="sm" disabled={locked} aria-label={`${t('removeRule')} ${index + 1}`} onClick={() => changeRules(rules.filter((_, i) => i !== index))}>{t('remove')}</Button>
        </div>
        <div className="flex flex-wrap gap-4">
          {ACTION_GROUPS.map((action) => {
            const checked = rule.verbs.includes(action) || (action === 'read' && rule.verbs.includes('search'));
            const access = rule.model && defaultAccount ? actionAccess(options.models.find((m) => m.model === rule.model)?.access, action) : null;
            const hint = rule.model ? access === false ? t('accountDenied') : access === null ? t('accountUnknown') : null : null;
            return <div key={action} className="space-y-1"><label className="flex items-center gap-2 text-sm">
            <Checkbox disabled={locked || (access === false && !checked)} checked={checked} onCheckedChange={(checked) => {
              const group = action === 'read' ? ['read', 'search'] : [action];
              const verbs = checked ? [...new Set([...rule.verbs, ...group])] : rule.verbs.filter((verb) => !group.includes(verb));
              changeRules(rules.map((r, i) => i === index ? { ...r, verbs } : r), checked ? rule.model : undefined);
            }} />
            {t(action)}{action === 'read' && partialRead(rule) && <Badge variant="secondary">{t('partial')}</Badge>}
          </label>
            {hint && <span className={access === false ? 'text-xs text-destructive' : 'text-xs text-muted-foreground'}>{hint}{access === false && checked ? ` · ${t('savedUnavailable')}` : ''}</span>}
          </div>; })}
        </div>
        {!rule.model && <p className="text-xs text-muted-foreground">{t('allTablesRights')}</p>}
        <p className="text-xs text-muted-foreground">{t('executeRights')}</p>
        {!rule.verbs.length && <p className="text-xs text-muted-foreground">{t('draftHint')}</p>}
        {options.read_only_models.includes(rule.model) && <p className="text-xs text-muted-foreground">{t('readOnlyHint')}</p>}
      </fieldset>)}
      <Button variant="outline" size="sm" disabled={locked} onClick={() => changeRules([...rules, { model: '', verbs: [] }])}>{t('addRule')}</Button>
    </fieldset>
    {(other.length > 0 || standalone.length > 0) && <div className="space-y-2">
      <p className="text-sm font-medium">{t('other')}</p>
      {other.map((entry, index) => <div key={`action-${index}`} className="flex items-center gap-2 text-sm">
        <code className="break-all whitespace-pre-wrap">{entry || t('emptyEntry')}</code>
        <Button variant="ghost" size="sm" disabled={locked} aria-label={`${t('remove')} ${entry}`} onClick={() => emit({ ...value, allowed_actions: serializeRules(rules, other.filter((_, i) => i !== index)) })}>{t('remove')}</Button>
      </div>)}
      {standalone.map((model) => <div key={`grant-${model}`} className="flex items-center gap-2 text-sm">
        <span>{t('legacyGrant')}：{model}</span>
        <Button variant="ghost" size="sm" disabled={disabled} aria-label={`${t('remove')} ${model}`} onClick={() => emit({ ...value, unblock_models: value.unblock_models.filter((m) => m !== model) })}>{t('remove')}</Button>
      </div>)}
    </div>}
    <ConfirmDialog open={pending !== null} title={t('confirmTitle')} confirmLabel={t('confirm')} confirmDisabled={locked}
      message={intl.formatMessage({ id: 'odoo.permissions.confirmMessage' }, { model: pending?.model ?? '' }) + (options.read_only_models.includes(pending?.model ?? '') ? ` ${t('readOnlyHint')}` : '')}
      onClose={() => setPending(null)} onConfirm={() => { if (!locked) pending?.apply(); setPending(null); }} />
  </div>;
}
