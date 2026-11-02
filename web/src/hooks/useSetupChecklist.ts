import { useEffect, useState } from 'react';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { loadSetupData, type SetupData } from '@/lib/setup-checklist';

export function useSetupChecklist(enabled = true) {
  const user = useAuthStore(s => s.user);
  const profile = useSystemStore(s => s.status?.edition_profile);
  const allowed = user?.role === 'admin' && profile === 'enterprise';
  const [result, setResult] = useState<{ userId: string; data: SetupData } | null>(null);
  useEffect(() => {
    let active = true;
    setResult(null);
    if (allowed && enabled && user) {
      void loadSetupData((key, value) => {
        if (!active) return;
        setResult(current => {
          const data = current?.userId === user.id ? current.data : {
            company: undefined, knowledge: undefined, runtime: undefined, accounts: undefined,
            google: undefined, odoo: undefined, agents: undefined, users: undefined,
            channels: undefined, skills: undefined, routines: undefined, tasks: undefined,
          };
          return { userId: user.id, data: { ...data, [key]: value } };
        });
      });
    }
    return () => { active = false; };
  }, [allowed, enabled, user?.id]);
  return { allowed, data: allowed && result?.userId === user?.id ? result?.data ?? null : null };
}
