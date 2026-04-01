import { create } from 'zustand';

export interface ChatMessage {
  readonly id: string;
  readonly role: 'user' | 'assistant' | 'system';
  readonly content: string;
  readonly timestamp: number;
  readonly tokens?: number;
}

interface ChatStore {
  readonly messages: readonly ChatMessage[];
  readonly isStreaming: boolean;
  readonly sessionId: string | null;
  readonly agentName: string;
  readonly agentIcon: string;
  readonly connectionState: 'disconnected' | 'connecting' | 'connected';

  connect: () => void;
  disconnect: () => void;
  send: (text: string) => void;
  reset: () => void;
}

let msgCounter = 0;

function nextId(): string {
  msgCounter += 1;
  return `msg-${Date.now()}-${msgCounter}`;
}

// Module-level WebSocket reference — kept outside Zustand to avoid
// serialization issues and enable reconnection logic.
let wsRef: WebSocket | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let reconnectAttempt = 0;
let intentionalDisconnect = false;
const MAX_RECONNECT_ATTEMPTS = 10;

function scheduleReconnect(connectFn: () => void) {
  if (intentionalDisconnect) return;
  if (reconnectAttempt >= MAX_RECONNECT_ATTEMPTS) return;

  const delay = Math.min(1000 * Math.pow(2, reconnectAttempt), 30000);
  reconnectAttempt += 1;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connectFn();
  }, delay);
}

export const useChatStore = create<ChatStore>((set, get) => {
  function connect() {
    if (wsRef && wsRef.readyState === WebSocket.OPEN) return;

    intentionalDisconnect = false;
    set({ connectionState: 'connecting' });

    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${protocol}//${window.location.host}/ws/chat`;
    const socket = new WebSocket(url);
    wsRef = socket;

    socket.onopen = () => {
      reconnectAttempt = 0;
      set({ connectionState: 'connected' });
    };

    socket.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data);
        switch (data.type) {
          case 'session_info':
            set({
              sessionId: data.session_id,
              agentName: data.agent_name,
              agentIcon: data.agent_icon,
            });
            break;

          case 'assistant_chunk':
            // Streaming chunk — append to last assistant message or create new
            set((state) => {
              const msgs = [...state.messages];
              const last = msgs[msgs.length - 1];
              if (last && last.role === 'assistant' && !('tokens' in last)) {
                msgs[msgs.length - 1] = {
                  ...last,
                  content: last.content + data.content,
                };
              } else {
                msgs.push({
                  id: nextId(),
                  role: 'assistant',
                  content: data.content,
                  timestamp: Date.now(),
                });
              }
              return { messages: msgs };
            });
            break;

          case 'assistant_done':
            set((state) => {
              const msgs = [...state.messages];
              const last = msgs[msgs.length - 1];
              if (last && last.role === 'assistant') {
                // Update with full content + tokens
                msgs[msgs.length - 1] = {
                  ...last,
                  content: data.content,
                  tokens: data.tokens_used,
                };
              } else {
                msgs.push({
                  id: nextId(),
                  role: 'assistant',
                  content: data.content,
                  timestamp: Date.now(),
                  tokens: data.tokens_used,
                });
              }
              return { messages: msgs, isStreaming: false };
            });
            break;

          case 'error':
            set((state) => ({
              messages: [
                ...state.messages,
                {
                  id: nextId(),
                  role: 'system',
                  content: `⚠️ ${data.message}`,
                  timestamp: Date.now(),
                },
              ],
              isStreaming: false,
            }));
            break;
        }
      } catch {
        // Ignore malformed messages
      }
    };

    socket.onclose = () => {
      if (wsRef === socket) wsRef = null;
      set({ connectionState: 'disconnected' });
      scheduleReconnect(connect);
    };

    socket.onerror = () => {
      // onclose will fire after this — reconnect is handled there
    };
  }

  return {
    messages: [],
    isStreaming: false,
    sessionId: null,
    agentName: 'DuDuClaw',
    agentIcon: '🐾',
    connectionState: 'disconnected',

    connect,

    disconnect: () => {
      intentionalDisconnect = true;
      if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      reconnectAttempt = 0;
      if (wsRef) {
        wsRef.close();
        wsRef = null;
      }
      set({ connectionState: 'disconnected' });
    },

    send: (text: string) => {
      if (!wsRef || wsRef.readyState !== WebSocket.OPEN) return;

      // Add user message to local state immediately
      set((state) => ({
        messages: [
          ...state.messages,
          {
            id: nextId(),
            role: 'user' as const,
            content: text,
            timestamp: Date.now(),
          },
        ],
        isStreaming: true,
      }));

      wsRef.send(
        JSON.stringify({
          type: 'user_message',
          content: text,
          session_id: get().sessionId,
        })
      );
    },

    reset: () => {
      const { sessionId } = get();
      // Send /new command to clear server session
      if (wsRef && wsRef.readyState === WebSocket.OPEN && sessionId) {
        wsRef.send(
          JSON.stringify({
            type: 'user_message',
            content: '/new',
            session_id: sessionId,
          })
        );
      }
      set({ messages: [], isStreaming: false });
    },
  };
});
