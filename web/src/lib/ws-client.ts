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

export class DuDuClawClient {
  private ws: WebSocket | null = null;
  private pendingRequests = new Map<string, PendingRequest>();
  private eventHandlers = new Map<string, Set<EventHandler>>();
  private requestId = 0;
  private reconnectAttempt = 0;
  private maxReconnectAttempts = 10;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private _state: ConnectionState = 'disconnected';
  private _onStateChange: ((state: ConnectionState) => void) | null = null;
  private url = '';
  private token?: string;

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

  connect(url: string, token?: string): Promise<void> {
    this.url = url;
    this.token = token;
    this.maxReconnectAttempts = 10;
    return this.doConnect();
  }

  private doConnect(): Promise<void> {
    return new Promise((resolve, reject) => {
      this.setState('connecting');

      try {
        this.ws = new WebSocket(this.url);
      } catch (e) {
        console.error('[WS] Failed to create WebSocket:', e);
        this.setState('disconnected');
        reject(e);
        return;
      }

      // Set ALL handlers before the connection opens
      this.ws.onmessage = (event) => {
        try {
          const frame: WsFrame = JSON.parse(event.data);
          this.handleFrame(frame);
        } catch (e) {
          console.warn('[WS] Failed to parse frame:', event.data, e);
        }
      };

      this.ws.onclose = (event) => {
        console.log('[WS] Connection closed:', event.code, event.reason);
        this.setState('disconnected');
        this.rejectAllPending('Connection closed');
        this.scheduleReconnect();
      };

      this.ws.onerror = (event) => {
        console.error('[WS] Connection error:', event);
        // onclose will fire after this
      };

      this.ws.onopen = async () => {
        console.log('[WS] Connection opened');
        this.reconnectAttempt = 0;
        this.setState('connected');

        // If token is provided, authenticate first
        if (this.token) {
          try {
            await this.call('connect', { token: this.token });
            this.setState('authenticated');
          } catch (e) {
            console.error('[WS] Authentication failed:', e);
            this.ws?.close();
            reject(e);
            return;
          }
        } else {
          // No token configured — still verify connection with server handshake
          try {
            await this.call('connect', { version: '0.6.5' });
            this.setState('authenticated');
          } catch {
            // Server may not require auth — allow connection for local-only dashboard
            console.warn('[WS] No auth configured — operating in local-only mode');
            this.setState('authenticated');
          }
        }
        resolve();
      };
    });
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
          try { handler(frame.payload); } catch (e) { console.error('[WS] Event handler error:', e); }
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

  call(method: string, params: Record<string, unknown> = {}): Promise<unknown> {
    return new Promise((resolve, reject) => {
      if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
        reject(new Error(`Not connected (state: ${this._state})`));
        return;
      }

      const id = String(++this.requestId);
      const timeout = setTimeout(() => {
        this.pendingRequests.delete(id);
        reject(new Error(`Request timeout: ${method}`));
      }, 30000);

      this.pendingRequests.set(id, { resolve, reject, timeout });

      const frame: WsFrame = { type: 'req', id, method, params };
      this.ws.send(JSON.stringify(frame));
    });
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
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.maxReconnectAttempts = 0;
    this.ws?.close();
    this.ws = null;
    this.setState('disconnected');
  }

  private scheduleReconnect() {
    if (this.reconnectAttempt >= this.maxReconnectAttempts) return;

    const delay = Math.min(1000 * Math.pow(2, this.reconnectAttempt), 30000);
    this.reconnectAttempt++;

    console.log(`[WS] Reconnecting in ${delay}ms (attempt ${this.reconnectAttempt})`);
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
