import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { client } from '@/lib/ws-client';
import { computerViewUrl, type ComputerViewTicket } from '@/lib/computer-session';

export type ViewerEnd = 'closed' | 'failed' | 'refused';

/**
 * The live picture of one computer-use session, drawn by noVNC (loaded on
 * demand, so the dashboard bundle only pays for it when someone watches).
 * Keyboard and mouse reach the screen only when the ticket says this viewer
 * holds the takeover (`input`); the gateway enforces the same rule on its
 * side, so a tampered client gains nothing.
 */
export function ComputerViewer({
  ticket,
  onEnd,
}: {
  ticket: ComputerViewTicket;
  onEnd: (why: ViewerEnd) => void;
}) {
  const intl = useIntl();
  const target = useRef<HTMLDivElement>(null);
  const [connected, setConnected] = useState(false);
  const endRef = useRef(onEnd);
  endRef.current = onEnd;

  useEffect(() => {
    const url = computerViewUrl(client.socketUrl(), window.location.origin, ticket.path, ticket.ticket);
    if (!url || !target.current) {
      endRef.current('refused');
      return;
    }
    let disposed = false;
    let rfb: import('@novnc/novnc').default | null = null;
    const node = target.current;
    import('@novnc/novnc')
      .then(({ default: RFB }) => {
        if (disposed) return;
        rfb = new RFB(node, url, { credentials: { password: ticket.password }, shared: true });
        rfb.viewOnly = !ticket.input;
        rfb.scaleViewport = true;
        rfb.resizeSession = false;
        rfb.clipViewport = false;
        rfb.focusOnClick = ticket.input;
        rfb.background = 'transparent';
        rfb.addEventListener('connect', () => setConnected(true));
        rfb.addEventListener('credentialsrequired', () =>
          rfb?.sendCredentials({ password: ticket.password }),
        );
        rfb.addEventListener('securityfailure', () => endRef.current('refused'));
        rfb.addEventListener('disconnect', (ev) => {
          setConnected(false);
          if (disposed) return;
          const clean = (ev as CustomEvent<{ clean?: boolean }>).detail?.clean;
          endRef.current(clean ? 'closed' : 'failed');
        });
      })
      .catch(() => {
        if (!disposed) endRef.current('failed');
      });
    return () => {
      disposed = true;
      try {
        rfb?.disconnect();
      } catch {
        // already closed
      }
    };
  }, [ticket]);

  return (
    <div className="relative overflow-hidden rounded-xl border border-surface-border bg-stone-900/95">
      <div
        ref={target}
        data-testid="computer-viewer-canvas"
        className="aspect-[16/10] w-full"
        aria-label={intl.formatMessage({ id: 'computerSession.viewer.label' })}
      />
      {!connected && (
        <div className="absolute inset-0 flex items-center justify-center text-sm text-stone-300 motion-safe:animate-pulse">
          {intl.formatMessage({ id: 'computerSession.viewer.connecting' })}
        </div>
      )}
      <div className="pointer-events-none absolute left-3 top-3 rounded-full bg-stone-950/70 px-2.5 py-1 text-xs text-stone-100 backdrop-blur-sm">
        {intl.formatMessage({
          id: ticket.input ? 'computerSession.viewer.controlling' : 'computerSession.viewer.watching',
        })}
      </div>
    </div>
  );
}
