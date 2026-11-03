import { useState } from 'react';
import { useIntl } from 'react-intl';
import { Badge, Button, Checkbox } from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls';

export function SensitiveModelPicker({ models, readOnlyModels, value, onChange, disabled = false }: {
  models: string[]; readOnlyModels: string[]; value: string[];
  onChange: (value: string[]) => void; disabled?: boolean;
}) {
  const intl = useIntl();
  const t = (key: string) => intl.formatMessage({ id: `odoo.permissions.${key}` });
  const [pending, setPending] = useState<string | null>(null);
  return <div className="space-y-3 p-4">
    <p className="text-sm text-muted-foreground">{t('globalHint')}</p>
    <div className="grid max-h-72 gap-3 overflow-y-auto sm:grid-cols-2">
      {models.map((model) => <label key={model} className="flex items-center gap-2 text-sm">
        <Checkbox aria-label={`${model} · ${t('sensitive')}${readOnlyModels.includes(model) ? ` · ${t('readOnly')}` : ''}`} checked={value.includes(model)} disabled={disabled} onCheckedChange={(checked) => {
          if (checked) setPending(model);
          else onChange(value.filter((m) => m !== model));
        }} />
        <span className="break-all">{model}</span>{' '}<Badge variant="secondary">{t('sensitive')}</Badge>
        {readOnlyModels.includes(model) && <span className="text-xs text-muted-foreground">{t('readOnly')}</span>}
      </label>)}
    </div>
    {value.filter((model) => !models.includes(model)).map((model) => <div key={model} className="flex items-center gap-2 text-sm">
      <span>{t('other')}：{model}</span>
      <Button variant="ghost" size="sm" disabled={disabled} onClick={() => onChange(value.filter((m) => m !== model))} aria-label={`${t('remove')} ${model}`}>{t('remove')}</Button>
    </div>)}
    <ConfirmDialog open={pending !== null} title={t('confirmTitle')}
      message={intl.formatMessage({ id: 'odoo.permissions.globalConfirm' }, { model: pending ?? '' }) + (readOnlyModels.includes(pending ?? '') ? ` ${t('readOnlyHint')}` : '')}
      confirmLabel={t('confirm')} confirmDisabled={disabled} onClose={() => setPending(null)} onConfirm={() => {
        if (pending && !disabled) onChange([...new Set([...value, pending])]);
        setPending(null);
      }} />
  </div>;
}
