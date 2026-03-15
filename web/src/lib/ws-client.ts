// Must match crates/duduclaw-gateway/src/protocol.rs exactly

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
  private maxReconnectAttempt = 10;
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
    return this.doConnect();
  }

  private doConnect(): Promise<void> {
    return new Promise((resolve, reject) => {
      this.setState('connecting');

      try {
        this.ws = new WebSocket(this.url);
      } catch (e) {
        this.setState('disconnected');
        reject(e);
        return;
      }

      this.ws.onopen = async () => {
        this.reconnectAttempt = 0;
        this.setState('connected');

        // Authenticate if token provided
        if (this.token) {
          try {
            await this.call('connect', { token: this.token });
            this.setState('authenticated');
          } catch (e) {
            console.error('Authentication failed:', e);
            this.ws?.close();
            reject(e);
            return;
          }
        } else {
          this.setState('authenticated');
        }
        resolve();
      };

      this.ws.onmessage = (event) => {
        try {
          const frame: WsFrame = JSON.parse(event.data);
          this.handleFrame(frame);
        } catch (e) {
          console.error('Failed to parse WsFrame:', e);
        }
      };

      this.ws.onclose = () => {
        this.setState('disconnected');
        this.rejectAllPending('Connection closed');
        this.scheduleReconnect();
      };

      this.ws.onerror = () => {
        // onclose will fire after this
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
          try {
            handler(frame.payload);
          } catch (e) {
            console.error('Event handler error:', e);
          }
        }
      }
      // Also fire wildcard handlers
      const wildcardHandlers = this.eventHandlers.get('*');
      if (wildcardHandlers) {
        for (const handler of wildcardHandlers) {
          try {
            handler({ ...frame, event: frame.event });
          } catch (e) {
            /* ignore */
          }
        }
      }
    }
  }

  call(method: string, params: Record<string, unknown> = {}): Promise<unknown> {
    return new Promise((resolve, reject) => {
      if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
        reject(new Error('Not connected'));
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

    // Return unsubscribe function
    return () => {
      this.eventHandlers.get(event)?.delete(handler);
    };
  }

  disconnect() {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.maxReconnectAttempt = 0; // prevent reconnect
    this.ws?.close();
    this.ws = null;
    this.setState('disconnected');
  }

  private scheduleReconnect() {
    if (this.reconnectAttempt >= this.maxReconnectAttempt) return;

    const delay = Math.min(1000 * Math.pow(2, this.reconnectAttempt), 30000);
    this.reconnectAttempt++;

    console.log(`Reconnecting in ${delay}ms (attempt ${this.reconnectAttempt})`);
    this.reconnectTimer = setTimeout(() => {
      this.doConnect().catch(() => {
        /* reconnect will retry */
      });
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
