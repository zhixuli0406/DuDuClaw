import { useEffect, useState } from 'react';
import { api } from '@/lib/api';
import type { DeptGrantsMineResult, GrantGroup } from '@/lib/dept-grants-types';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';

const EMPTY: DeptGrantsMineResult = { grants: [], agents: [], needs_sync: [] };

/** Department grants are separate from conversational/callable employee access. */
export function useDepartmentAgents(group: GrantGroup) {
  const { user, deptGrants } = useAuthStore();
  const connection = useConnectionStore(s => s.state);
  const active = !!user && user.role !== 'admin' && deptGrants.some(g => g.groups.includes(group));
  const key = JSON.stringify([user?.id, group, deptGrants, connection]);
  const [result, setResult] = useState<{ key: string; data: DeptGrantsMineResult; error?: unknown }>();
  useEffect(() => {
    if (!active || connection !== 'authenticated') return;
    let current = true;
    api.users.deptGrants.mine(group).then(data => {
      if (current) setResult({ key, data: { grants: data?.grants ?? [], agents: data?.agents ?? [], needs_sync: data?.needs_sync ?? [] } });
    }).catch(error => {
      if (current) setResult({ key, data: EMPTY, error });
    });
    return () => { current = false; };
  }, [active, connection, group, key]);
  return result?.key === key && active ? result : { data: EMPTY, error: undefined };
}
