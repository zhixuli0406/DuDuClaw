import { useIntl } from 'react-intl';
import { ACTION_GROUPS, parseActions, partialRead } from './permissions';

export function OdooPermissionSummary({ actions, effectiveBlockedModels, unblockInherited = false }: {
  actions: string[];
  effectiveBlockedModels?: string[];
  unblockInherited?: boolean;
}) {
  const intl = useIntl();
  const t = (key: string) => intl.formatMessage({ id: `odoo.permissions.${key}` });
  const { rules, other } = parseActions(actions);

  return <div className="space-y-1">
    {!actions.length && <span>{t('actionsUnrestricted')}</span>}
    {rules.map((rule) => <p key={rule.model}>
      {rule.model || t('allTables')}：{ACTION_GROUPS.filter((action) => rule.verbs.includes(action) || (action === 'read' && rule.verbs.includes('search')))
        .map((action) => `${t(action)}${action === 'read' && partialRead(rule) ? ` (${t('partial')})` : ''}`).join('、')}
    </p>)}
    {other.length > 0 && <p>{t('other')}：{other.join('、')}</p>}
    {effectiveBlockedModels !== undefined && <div className="mt-3 space-y-1 text-sm">
      <p>{t('effectiveBlockedModels')} {unblockInherited && <span className="rounded bg-secondary px-2 py-0.5 text-xs text-muted-foreground">{t('unblockInherited')}</span>}</p>
      <p className="break-words text-muted-foreground">{effectiveBlockedModels.length ? effectiveBlockedModels.join('、') : t('noBlockedModels')}</p>
    </div>}
  </div>;
}
