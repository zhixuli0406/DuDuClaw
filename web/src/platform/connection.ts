/**
 * Platform layer — connection (N-1, `DESIGN-agent-os-native-apps-2026-08.md`
 * §2 L2). Re-exports the WebSocket client + connection store so app code has
 * exactly one import path for "how do I talk to the gateway", instead of each
 * app reaching into `@/lib/ws-client` / `@/stores/connection-store` directly
 * and drifting (C1/C4 in the design doc's coupling matrix).
 *
 * This is a re-export barrel, not a rewrite: the underlying files stay where
 * they are for now (`@/lib/ws-client`, `@/stores/connection-store`) — moving
 * their actual contents is a later, separate step (§2 L2 note: "既有檔案位置
 * 可不搬"). The point of this file is the import boundary, not a relocation.
 */
export {
  client,
  DuDuClawClient,
  MUST_CHANGE_PASSWORD_ERROR_CODE,
  type ConnectionState,
  type WsFrame,
} from '@/lib/ws-client';
export { useConnectionStore } from '@/stores/connection-store';
