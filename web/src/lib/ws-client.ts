// WebSocket client matching the Rust WsFrame protocol
// (crates/duduclaw-gateway/src/protocol.rs)

export type WsFrame =
  | { type: 'req'; id: string; method: string; params: Record<string, unknown> }
  | { type: 'res'; id: string; ok: boolean; payload?: unknown; error?: unknown }
  | { type: 'event'; event: string; payload: unknown; seq?: number; state_version?: number };

export type ConnectionState = 'disconnected' | 'connecting' | 'connected' | 'authenticated';

/**
 * Stable machine-readable code the gateway returns for any RPC (outside the
 * self-service allowlist — `users.me`/`users.change_password`/`connect`/
 * `connect.challenge`/`ping`) reached by a caller whose account still carries
 * `must_change_password` (`handlers.rs::MUST_CHANGE_PASSWORD_ERROR_CODE`).
 */
export const MUST_CHANGE_PASSWORD_ERROR_CODE = 'must_change_password_required';

/** Best-effort check for the `{ code, message }` shape a structured RPC
 *  rejection carries — mirrors the small helpers pages inline for the same
 *  purpose (`OSPage`, `useIsAppliance`). */
function hasErrorCode(err: unknown, code: string): boolean {
  return (
    !!err &&
    typeof err === 'object' &&
    'code' in err &&
    (err as { code?: unknown }).code === code
  );
}

type PendingRequest = {
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
  timeout: ReturnType<typeof setTimeout>;
};

type EventHandler = (payload: unknown) => void;

// H7 fix: token getter type — called on each connect/reconnect for fresh value
type TokenGetter = () => string | undefined;

// Called before a reconnect when the previous handshake looked like an auth
// failure (e.g., expired JWT). Implementations should refresh the token so
// `getToken()` returns a valid one on the next doConnect.
type AuthRefreshHook = () => Promise<void>;

export class DuDuClawClient {
  private ws: WebSocket | null = null;
  private pendingRequests = new Map<string, PendingRequest>();
  private eventHandlers = new Map<string, Set<EventHandler>>();
  private requestId = 0;
  private reconnectAttempt = 0;
  private maxReconnectAttempts = 10;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private heartbeatTimer: ReturnType<typeof setInterval> | null = null;
  private _state: ConnectionState = 'disconnected';
  private _onStateChange: ((state: ConnectionState) => void) | null = null;
  private url = '';
  private getToken?: TokenGetter;
  private authRefreshHook?: AuthRefreshHook;
  private needsAuthRefresh = false;
  // WP-0 (bootstrap-admin recovery): whether the currently-authenticated
  // account still must change its password. Captured from the `connect`
  // handshake response's `must_change_password` field, AND kept in sync by
  // watching every subsequent RPC rejection for
  // `MUST_CHANGE_PASSWORD_ERROR_CODE` — belt (explicit flag at handshake) and
  // suspenders (any RPC can reveal it, in case a session gets flagged
  // mid-connection by an operator password reset).
  private _mustChangePassword = false;
  private _onMustChangePasswordChange: ((flag: boolean) => void) | null = null;

  get state(): ConnectionState {
    return this._state;
  }

  get mustChangePassword(): boolean {
    return this._mustChangePassword;
  }

  set onStateChange(handler: (state: ConnectionState) => void) {
    this._onStateChange = handler;
  }

  set onMustChangePasswordChange(handler: (flag: boolean) => void) {
    this._onMustChangePasswordChange = handler;
  }

  private setState(state: ConnectionState) {
    this._state = state;
    this._onStateChange?.(state);
  }

  private setMustChangePassword(flag: boolean) {
    if (this._mustChangePassword === flag) return;
    this._mustChangePassword = flag;
    this._onMustChangePasswordChange?.(flag);
  }

  // H7 fix: accept a getter function instead of a static token
  connect(url: string, getToken?: TokenGetter, authRefreshHook?: AuthRefreshHook): Promise<void> {
    this.url = url;
    this.getToken = getToken;
    this.authRefreshHook = authRefreshHook;
    this.maxReconnectAttempts = 10;
    return this.doConnect();
  }

  private async doConnect(): Promise<void> {
    // If the previous handshake failed with an auth error, refresh the
    // token before re-opening the socket so getToken() returns a fresh JWT.
    if (this.needsAuthRefresh && this.authRefreshHook) {
      this.needsAuthRefresh = false;
      try { await this.authRefreshHook(); } catch { /* refresh failure → use whatever getToken returns */ }
    }

    return new Promise((resolve, reject) => {
      this.setState('connecting');

      try {
        this.ws = new WebSocket(this.url);
      } catch (e) {
        this.setState('disconnected');
        reject(e);
        return;
      }

      // Set ALL handlers before the connection opens
      this.ws.onmessage = (event) => {
        try {
          const frame: WsFrame = JSON.parse(event.data);
          this.handleFrame(frame);
        } catch {
          // Ignore parse errors
        }
      };

      this.ws.onclose = () => {
        this.stopHeartbeat();
        if (this.ws === null) return; // Intentional disconnect — skip
        this.setState('disconnected');
        this.rejectAllPending('Connection closed');
        this.scheduleReconnect();
      };

      this.ws.onerror = () => {
        // onclose will fire after this
      };

      this.ws.onopen = async () => {
        this.reconnectAttempt = 0;
        this.setState('connected');

        // H7 fix: get fresh token on each connect/reconnect
        const token = this.getToken?.();

        if (token) {
          try {
            // JWT if contains dots, otherwise legacy token
            const params = token.includes('.')
              ? { jwt: token }
              : { token };
            const payload = (await this.call('connect', params, true)) as
              | { must_change_password?: boolean }
              | undefined;
            this.setMustChangePassword(payload?.must_change_password === true);
            this.setState('authenticated');
          } catch (e) {
            // H10 fix: do NOT set authenticated on failure
            // If the failure looks like an auth error, flag for token refresh
            // before the next reconnect attempt.
            const msg = String(e).toLowerCase();
            if (msg.includes('jwt') || msg.includes('auth')) {
              this.needsAuthRefresh = true;
            }
            this.ws?.close();
            reject(e);
            return;
          }
        } else {
          // No token — try server handshake for local-only mode
          try {
            const payload = (await this.call('connect', { version: '0.6.5' }, true)) as
              | { must_change_password?: boolean }
              | undefined;
            this.setMustChangePassword(payload?.must_change_password === true);
            this.setState('authenticated');
          } catch {
            // H10 fix: if server requires auth and we have no token, don't fake authenticated
            // Only allow local-only mode if server explicitly accepts it
            this.setState('disconnected');
            this.ws?.close();
            reject(new Error('Authentication required'));
            return;
          }
        }
        this.startHeartbeat();
        resolve();
      };
    });
  }

  private startHeartbeat() {
    this.stopHeartbeat();
    // Send a ping every 25s to keep the connection alive
    // (server expects activity within 60s)
    this.heartbeatTimer = setInterval(() => {
      if (this.ws?.readyState === WebSocket.OPEN) {
        // Send an application-level ping since browser WebSocket
        // does not expose the ping/pong API
        this.ws.send(JSON.stringify({ type: 'req', id: '_ping', method: 'ping', params: {} }));
      }
    }, 25000);
  }

  private stopHeartbeat() {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
  }

  private handleFrame(frame: WsFrame) {
    if (frame.type === 'res') {
      // Suspenders: any RPC rejection can reveal the must-change-password
      // gate, not just the initial handshake (belt) — see the field's doc
      // comment above.
      if (!frame.ok && hasErrorCode(frame.error, MUST_CHANGE_PASSWORD_ERROR_CODE)) {
        this.setMustChangePassword(true);
      }
      const pending = this.pendingRequests.get(frame.id);
      if (pending) {
        clearTimeout(pending.timeout);
        this.pendingRequests.delete(frame.id);
        if (frame.ok) {
          pending.resolve(frame.payload);
        } else {
          pending.reject(frame.error ?? 'Request failed');
        }
      }
    } else if (frame.type === 'event') {
      const handlers = this.eventHandlers.get(frame.event);
      if (handlers) {
        for (const handler of handlers) {
          try { handler(frame.payload); } catch { /* ignore */ }
        }
      }
      // Wildcard handlers
      const wildcardHandlers = this.eventHandlers.get('*');
      if (wildcardHandlers) {
        for (const handler of wildcardHandlers) {
          try { handler({ ...frame, event: frame.event }); } catch { /* ignore */ }
        }
      }
    }
  }

  /**
   * Send an RPC request over the WebSocket.
   *
   * If `skipAuthWait` is true, only waits for `WebSocket.OPEN` (used for
   * the handshake `connect` call itself). Otherwise waits until state
   * reaches `authenticated` to prevent race conditions where API calls
   * fire before the handshake completes.
   */
  call(
    method: string,
    params: Record<string, unknown> = {},
    skipAuthWait = false,
    // Per-call response timeout. Defaults to 30s; long-running RPCs (e.g.
    // `migrate.apply`, which may run up to 300s server-side) pass a larger value
    // so the dashboard doesn't reject a request the gateway is still working on.
    timeoutMs = 30000,
  ): Promise<unknown> {
    const waitForReady = (): Promise<void> => {
      // For handshake calls, only need WS to be open
      const isReady = skipAuthWait
        ? this.ws?.readyState === WebSocket.OPEN
        : this._state === 'authenticated';

      if (isReady) return Promise.resolve();
      if (this._state === 'disconnected' && !this.reconnectTimer) {
        return Promise.reject(new Error('Not connected'));
      }
      return new Promise((resolve, reject) => {
        const maxWait = setTimeout(() => {
          reject(new Error(`WebSocket not ready after 10s (state: ${this._state})`));
        }, 10000);
        const check = setInterval(() => {
          const ready = skipAuthWait
            ? this.ws?.readyState === WebSocket.OPEN
            : this._state === 'authenticated';
          if (ready) {
            clearInterval(check);
            clearTimeout(maxWait);
            resolve();
          } else if (this._state === 'disconnected' && !this.reconnectTimer) {
            clearInterval(check);
            clearTimeout(maxWait);
            reject(new Error('Connection lost'));
          }
        }, 100);
      });
    };

    return waitForReady().then(() => new Promise((resolve, reject) => {
      const id = String(++this.requestId);
      const timeout = setTimeout(() => {
        this.pendingRequests.delete(id);
        reject(new Error(`Request timeout: ${method}`));
      }, timeoutMs);

      this.pendingRequests.set(id, { resolve, reject, timeout });

      const frame: WsFrame = { type: 'req', id, method, params };
      this.ws!.send(JSON.stringify(frame));
    }));
  }

  subscribe(event: string, handler: EventHandler): () => void {
    if (!this.eventHandlers.has(event)) {
      this.eventHandlers.set(event, new Set());
    }
    this.eventHandlers.get(event)!.add(handler);
    return () => {
      this.eventHandlers.get(event)?.delete(handler);
    };
  }

  disconnect() {
    this.stopHeartbeat();
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.maxReconnectAttempts = 0;
    const ws = this.ws;
    this.ws = null; // Clear ref BEFORE close — onclose guard checks this
    ws?.close();
    this.setState('disconnected');
    // An explicit disconnect (logout, or the WP-0 forced reconnect after a
    // successful password change) starts the next session fresh — never
    // carry a stale must-change-password flag across it. A mid-session drop
    // that auto-reconnects does NOT go through this method, so a genuine
    // still-must-change-password account keeps the gate through a network blip.
    this.setMustChangePassword(false);
    this.rejectAllPending('Disconnected');
  }

  private scheduleReconnect() {
    if (this.reconnectAttempt >= this.maxReconnectAttempts) return;

    const delay = Math.min(1000 * Math.pow(2, this.reconnectAttempt), 30000);
    this.reconnectAttempt++;

    this.reconnectTimer = setTimeout(() => {
      this.doConnect().catch(() => { /* reconnect will retry */ });
    }, delay);
  }

  private rejectAllPending(reason: string) {
    for (const [, pending] of this.pendingRequests) {
      clearTimeout(pending.timeout);
      pending.reject(new Error(reason));
    }
    this.pendingRequests.clear();
  }
}

// Singleton client instance
export const client = new DuDuClawClient();
