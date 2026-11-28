import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { settingsRefusal } from '@/lib/settings-refusal';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { Button, CollectionPageState, useErrorMessage } from '@/components/mds';
import { AutopilotTab } from '@/components/settings/sections/AutopilotTab';
import { DialogSelect, type DepartmentRuleMode } from '@/components/settings/sections/AutopilotRuleDialog';

/** Mount a separate rule workspace for each department and connection. */
export function DepartmentAutomationTab() {
  const intl = useIntl();
  const grants = useAuthStore(s => s.deptGrants);
  const connection = useConnectionStore(s => s.state);
  const departments = [...new Set(grants.filter(g => g.groups.includes('automation')).map(g => g.department))];
  const [selected, setSelected] = useState(departments[0] ?? '');
  const department = departments.includes(selected) ? selected : departments[0] ?? '';
  return <div className="space-y-4">
    <DialogSelect label={intl.formatMessage({ id: 'department.filter' })} value={department}
      onChange={setSelected} options={departments.map(d => ({ value: d, label: d }))} />
    {department && <DepartmentRuleWorkspace key={`${department}:${connection}`} department={department} />}
  </div>;
}

function DepartmentRuleWorkspace({ department }: { department: string }) {
  const intl = useIntl();
  const errorText = useErrorMessage();
  const connection = useConnectionStore(s => s.state);
  const [mode, setMode] = useState<DepartmentRuleMode>();
  const [error, setError] = useState<unknown>(null);
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    if (connection !== 'authenticated') return;
    let active = true;
    setMode(undefined);
    setError(null);
    void Promise.all([
      api.users.deptGrants.mine('automation'),
      api.agents.list(),
      api.autopilot.notifyDestinations(department),
    ]).then(([access, roster, notifications]) => {
      if (!active) return;
      setMode({ department,
        agents: access.agents.filter(a => a.groups.includes('automation')
          && !access.needs_sync.some(row => row.id === a.id)
          && roster.agents.some(row => row.name === a.id && row.department === department)),
        notifyDestinations: notifications.destinations,
      });
    }).catch(e => { if (active) setError(e); });
    return () => { active = false; };
  }, [connection, department, retry]);
  if (error != null) return <div role="alert" className="space-y-3 text-sm text-destructive">
    <p>{settingsRefusal(error, errorText(error))}</p>
    <Button variant="outline" onClick={() => setRetry(value => value + 1)}>{intl.formatMessage({ id: 'common.retry' })}</Button>
  </div>;
  if (!mode) return <CollectionPageState state="loading" />;
  return <AutopilotTab departmentMode={mode} />;
}
