import { useIntl } from 'react-intl';
import { ACTION_GROUPS, parseActions, partialRead } from './permissions';

export function OdooPermissionSummary({ actions }: { actions: string[] }) {
  const intl = useIntl();
  const t = (key: string) => intl.formatMessage({ id: `odoo.permissions.${key}` });
  const { rules, other } = parseActions(actions);
  if (!actions.length) return <span>{t('actionsUnrestricted')}</span>;
  return <div className="space-y-1">
    {rules.map((rule) => <p key={rule.model}>
      {rule.model || t('allTables')}：{ACTION_GROUPS.filter((action) => rule.verbs.includes(action) || (action === 'read' && rule.verbs.includes('search')))
        .map((action) => `${t(action)}${action === 'read' && partialRead(rule) ? ` (${t('partial')})` : ''}`).join('、')}
    </p>)}
    {other.length > 0 && <p>{t('other')}：{other.join('、')}</p>}
  </div>;
}
