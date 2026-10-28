import { useCallback } from 'react';
import { useNavigate } from 'react-router';
import { ASSIGN_PATH, assignState, type AssignPrefill } from '@/lib/assign-entry';

/**
 * Open the 交辦任務 page, optionally seeded. The seed rides in router state
 * (see `lib/assign-entry`), so it never lands in the URL.
 */
export function useOpenAssign(): (prefill?: AssignPrefill) => void {
  const navigate = useNavigate();
  return useCallback(
    (prefill?: AssignPrefill) => {
      const state = assignState(prefill);
      navigate(ASSIGN_PATH, state ? { state } : undefined);
    },
    [navigate],
  );
}
