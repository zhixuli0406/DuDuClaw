// WebSocket client matching the Rust WsFrame protocol
// (crates/duduclaw-gateway/src/protocol.rs)

export type WsFrame =
  | { type: 'req'; id: string; method: string; params: Record<string, unknown> }
  | { type: 'res'; id: string; ok: boolean; payload?: unknown; error?: unknown }
  | { type: 'event'; event: string; payload: unknown; seq?: number; state_version?: number };

export type ConnectionState = 'disconnected' | 'connecting' | 'connected' | 'authenticated';

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

  get state(): ConnectionState {
    return this._state;
  }

  set onStateChange(handler: (state: ConnectionState) => void) {
    this._onStateChange = handler;
  }

  private setState(state: ConnectionState) {
    this._state = state;
    this._onStateChange?.(state);
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
            await this.call('connect', params, true);
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
            await this.call('connect', { version: '0.6.5' }, true);
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
  call(method: string, params: Record<string, unknown> = {}, skipAuthWait = false): Promise<unknown> {
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
      }, 30000);

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
