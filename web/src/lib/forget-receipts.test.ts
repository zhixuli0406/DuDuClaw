import { afterEach, describe, expect, it, vi } from 'vitest';
import { notifyDisconnectForget } from './forget-receipts';
import { toast } from './toast';

afterEach(() => vi.restoreAllMocks());

describe('disconnect forget notices', () => {
  it('keeps the disconnect response and offers its pending approval without claiming deletion', () => {
    const info = vi.spyOn(toast, 'info').mockImplementation(() => {});
    const response = { success: true, forget_memories: { status: 'pending_approval', approval_id: 'approval-1' } };
    expect(notifyDisconnectForget(response)).toBe(response);
    expect(info).toHaveBeenCalledWith(expect.any(String), expect.objectContaining({ action: expect.objectContaining({ onClick: expect.any(Function) }) }));
  });

  it('reports failed planning separately from successful disconnect', () => {
    const error = vi.spyOn(toast, 'error').mockImplementation(() => {});
    const response = { success: true, forget_memories: { status: 'failed' } };
    expect(notifyDisconnectForget(response)).toBe(response);
    expect(error).toHaveBeenCalledOnce();
  });

  it('does not invent an approval when forgetting is disabled or an older gateway omits it', () => {
    const info = vi.spyOn(toast, 'info').mockImplementation(() => {});
    notifyDisconnectForget({ success: true });
    expect(info).not.toHaveBeenCalled();
    notifyDisconnectForget({ success: true, forget_memories: { status: 'disabled' } });
    expect(info).toHaveBeenCalledWith(expect.any(String), { durationMs: 12000 });
  });
});
