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
      void loadSetupData().then(data => { if (active) setResult({ userId: user.id, data }); });
    }
    return () => { active = false; };
  }, [allowed, enabled, user?.id]);
  return { allowed, data: allowed && result?.userId === user?.id ? result?.data ?? null : null };
}
