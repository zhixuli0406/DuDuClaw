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
  const [request, setRequest] = useState<ApprovalRequest | null>(null);
  const [countdown, setCountdown] = useState(0);
  const respondedRef = useRef(false);

  // Subscribe to approval_request events from WebSocket
  useEffect(() => {
    const unsub = client.subscribe('browser.approval_request', (payload: unknown) => {
      const req = payload as ApprovalRequest;
      respondedRef.current = false;
      setRequest(req);
      setCountdown(req.timeout_seconds || 30);
    });
    return unsub;
  }, []);

  const handleResponse = useCallback(async (approved: boolean) => {
    if (respondedRef.current) return;
    respondedRef.current = true;
    if (!request) return;
    try {
      await client.call('browser.respond_approval', {
        request_id: request.request_id,
        approved,
      });
    } catch {
      // Response delivery failed — the timeout will auto-deny on the backend
    }
    setRequest(null);
  }, [request]);

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
