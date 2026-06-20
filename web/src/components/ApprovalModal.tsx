import { useEffect, useRef, useState, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { client } from '@/lib/ws-client';
import { ShieldAlert, Check, X, Clock } from 'lucide-react';

interface ApprovalRequest {
  request_id: string;
  agent_id: string;
  action: string;
  url: string;
  details: Record<string, unknown>;
  requested_at: string;
  timeout_seconds: number;
}

export function ApprovalModal() {
  const intl = useIntl();
  // M58 fix: queue incoming approval requests so a second request arriving
  // before the first is answered no longer silently overwrites it. We show
  // the queue head (`request`) and advance to the next one after each response.
  const [queue, setQueue] = useState<ApprovalRequest[]>([]);
  const request = queue[0] ?? null;
  const [countdown, setCountdown] = useState(0);
  const [responseError, setResponseError] = useState<string | null>(null);
  const respondedRef = useRef(false);

  // Subscribe to approval_request events from WebSocket
  useEffect(() => {
    const unsub = client.subscribe('browser.approval_request', (payload: unknown) => {
      const req = payload as ApprovalRequest;
      // Append to the queue (immutable). De-dupe on request_id in case the
      // same event is delivered twice (e.g. WebSocket reconnect replay).
      setQueue((prev) =>
        prev.some((r) => r.request_id === req.request_id) ? prev : [...prev, req],
      );
    });
    return unsub;
  }, []);

  // Reset per-request UI state whenever the queue head changes.
  useEffect(() => {
    respondedRef.current = false;
    setResponseError(null);
    setCountdown(request ? request.timeout_seconds || 30 : 0);
  }, [request?.request_id]);

  const handleResponse = useCallback(async (approved: boolean) => {
    if (respondedRef.current) return;
    if (!request) return;
    respondedRef.current = true;
    const requestId = request.request_id;
    try {
      await client.call('browser.respond_approval', {
        request_id: requestId,
        approved,
      });
      setResponseError(null);
      // Advance to the next queued request (immutable removal by id).
      setQueue((prev) => prev.filter((r) => r.request_id !== requestId));
    } catch (err) {
      // Response delivery failed — keep the modal open and surface the error.
      // The backend will auto-deny on timeout, but the user needs visible feedback.
      const message = err instanceof Error ? err.message : String(err);
      setResponseError(
        intl.formatMessage({ id: 'browser.approvals.responseError' }, { message }),
      );
      respondedRef.current = false;
    }
  }, [request, intl]);

  // Countdown timer — auto-deny when it reaches zero
  useEffect(() => {
    if (!request || countdown <= 0) {
      if (request && countdown <= 0) {
        handleResponse(false);
      }
      return;
    }
    const timer = setTimeout(() => setCountdown((c) => c - 1), 1000);
    return () => clearTimeout(timer);
  }, [request, countdown, handleResponse]);

  if (!request) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm">
      <div className="mx-4 w-full max-w-md rounded-2xl border border-stone-200 bg-white p-6 shadow-2xl dark:border-stone-700 dark:bg-stone-800">
        {/* Header */}
        <div className="mb-4 flex items-center gap-3">
          <div className="flex h-10 w-10 items-center justify-center rounded-full bg-amber-100 dark:bg-amber-900/30">
            <ShieldAlert className="h-5 w-5 text-amber-600 dark:text-amber-400" />
          </div>
          <div>
            <h3 className="font-semibold text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: 'browser.approvals.required', defaultMessage: 'Approval Required' })}
            </h3>
            <p className="text-xs text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'browser.approvals.agentId' })}: {request.agent_id}
            </p>
            {queue.length > 1 && (
              <p className="text-xs font-medium text-amber-600 dark:text-amber-400">
                {intl.formatMessage(
                  { id: 'browser.approvals.queued' },
                  { count: queue.length - 1 },
                )}
              </p>
            )}
          </div>
          <div className="ml-auto flex items-center gap-1 font-mono text-sm">
            <Clock className="h-4 w-4 text-stone-400" />
            <span className={countdown <= 10 ? 'font-bold text-rose-500' : 'text-stone-500'}>
              {countdown}s
            </span>
          </div>
        </div>

        {/* Details */}
        <div className="mb-5 space-y-2 rounded-lg bg-stone-50 p-3 dark:bg-stone-900">
          <div className="flex gap-2 text-sm">
            <span className="font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'browser.approvals.action', defaultMessage: 'Action' })}:</span>
            <span className="font-medium text-stone-800 dark:text-stone-200">{request.action}</span>
          </div>
          {request.url && (
            <div className="flex gap-2 text-sm">
              <span className="font-medium text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'browser.approvals.url', defaultMessage: 'URL' })}:</span>
              <span className="break-all text-stone-700 dark:text-stone-300">{request.url}</span>
            </div>
          )}
          {Object.keys(request.details).length > 0 && (
            <pre className="mt-2 max-h-24 overflow-auto text-xs text-stone-600 dark:text-stone-400">
              {JSON.stringify(request.details, null, 2)}
            </pre>
          )}
        </div>

        {/* Response error */}
        {responseError && (
          <div
            role="alert"
            className="mb-4 rounded-lg border border-rose-200 bg-rose-50 px-3 py-2 text-sm text-rose-700 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-300"
          >
            {responseError}
          </div>
        )}

        {/* Actions */}
        <div className="flex gap-3">
          <button
            onClick={() => handleResponse(false)}
            className="flex flex-1 items-center justify-center gap-2 rounded-xl border border-stone-300 px-4 py-2.5 text-sm font-medium text-stone-700 transition-colors hover:bg-stone-100 dark:border-stone-600 dark:text-stone-300 dark:hover:bg-stone-700"
          >
            <X className="h-4 w-4" />
            {intl.formatMessage({ id: 'browser.approvals.deny', defaultMessage: 'Deny' })}
          </button>
          <button
            onClick={() => handleResponse(true)}
            className="flex flex-1 items-center justify-center gap-2 rounded-xl bg-amber-500 px-4 py-2.5 text-sm font-medium text-white transition-colors hover:bg-amber-600"
          >
            <Check className="h-4 w-4" />
            {intl.formatMessage({ id: 'browser.approvals.approve' })}
          </button>
        </div>
      </div>
    </div>
  );
}
