import { useSystemStore } from '@/stores/system-store';

/**
 * v1.81 F5: whether this install runs in protected mode, as `system.status`
 * reports it (`isolation_readiness.required`). Only protected Linux TEAM
 * installs carry the readiness evidence. Fail-closed: a status that has not
 * loaded yet, or an older gateway, reads as `false`. Presentation only — the
 * isolation RPCs refuse outside protected mode on their own.
 */
export function useProtectedMode(): boolean {
  return useSystemStore((s) => s.status?.isolation_readiness?.required === true);
}
