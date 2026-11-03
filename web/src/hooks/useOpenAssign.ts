import { TASK_ENTRY_POINTS_HIDDEN } from '@/lib/task-entry-visibility';
import { useConversationsStore } from '@/stores/conversations-store';
import { useCallback } from 'react';
import { useNavigate } from 'react-router';
import { ASSIGN_PATH, assignState, chatDraftState, type AssignPrefill } from '@/lib/assign-entry';

/**
 * Start a fresh conversation, keeping any supplied text in router state.
 */
export function useOpenAssign(): (prefill?: AssignPrefill) => void {
  const navigate = useNavigate();
  return useCallback(
    (prefill?: AssignPrefill) => {
      if (TASK_ENTRY_POINTS_HIDDEN) {
        useConversationsStore.getState().startNew();
        const state = chatDraftState(prefill);
        navigate('/chat', state ? { state } : undefined);
        return;
      }
      const state = assignState(prefill);
      navigate(ASSIGN_PATH, state ? { state } : undefined);
    },
    [navigate],
  );
}
