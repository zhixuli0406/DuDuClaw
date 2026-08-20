import type { ChatArtifact } from './artifact-types';

/**
 * Fixture/mock artifacts (O-3, this round's "先定 contract＋元件＋以 mock/
 * fixture 資料展示" requirement — no backend wiring exists yet to produce
 * these for real, see the WIRING TODO in `artifact-types.ts`). Every card
 * component's test renders against one of these so the contract is proven
 * end-to-end even though nothing on the wire populates it today.
 *
 * Shapes mirror what the real `device.*` / `approvals.*` RPCs return (see
 * `DevicePage.test.tsx`'s own `FULL_STATUS` fixture and `ApprovalItem` in
 * `@/lib/api`) — kept deliberately in sync by inspection so a future O-4
 * integration test can swap a fixture for a live RPC result with zero shape
 * changes.
 */

export const FIXTURE_DEVICE_STATUS: ChatArtifact = {
  type: 'device_status',
  id: 'fixture-device-status',
  payload: {
    cpu_cores: 4,
    load_average: { load1: 0.42, load5: 0.38, load15: 0.31 },
    ram: { total_mb: 8192, available_mb: 4096, used_mb: 4096 },
    disk: { total_mb: 102400, used_mb: 51200, available_mb: 51200 },
    temperature_c: 46.5,
    uptime_secs: 187_320,
    network_interfaces: [
      { name: 'eth0', is_up: true, addresses: ['192.168.1.10'] },
      { name: 'wlan0', is_up: false, addresses: [] },
    ],
  },
};

export const FIXTURE_UPDATE_STATUS: ChatArtifact = {
  type: 'update_status',
  id: 'fixture-update-status',
  payload: {
    action: 'check',
    result: { success: true, stdout: 'duduclaw 1.62.0 -> 1.62.1 available', stderr: '' },
  },
};

/** Same `os_check_update` read as `FIXTURE_UPDATE_STATUS`, but with the
 *  `system` half also populated (self-update version check) — both halves
 *  present, as the backend produces on an appliance install
 *  (`os_operator.rs`'s `readonly_result_to_artifact`), used to exercise
 *  `UpdateStatusCard`'s optional `system` section alongside `result`. */
export const FIXTURE_UPDATE_STATUS_WITH_SYSTEM: ChatArtifact = {
  type: 'update_status',
  id: 'fixture-update-status-with-system',
  payload: {
    action: 'check',
    result: { success: true, stdout: 'duduclaw-sysd: no OS image update', stderr: '' },
    system: {
      available: true,
      current_version: '1.61.2',
      latest_version: '1.62.0',
      release_notes: 'bug fixes',
      published_at: '2026-08-15T00:00:00Z',
      install_method: 'npm',
      containerized: false,
    },
  },
};

/** P5: a non-appliance install's `os_check_update` read — `result` (the
 *  appliance OS image half) is entirely ABSENT (never a fabricated
 *  `DeviceOpResult`), only `system` (DuDuClaw's own version check, which runs
 *  regardless of `is_appliance()`) is present. Exercises
 *  `UpdateStatusCard`'s device-result section being omitted wholesale. */
export const FIXTURE_UPDATE_STATUS_SYSTEM_ONLY: ChatArtifact = {
  type: 'update_status',
  id: 'fixture-update-status-system-only',
  payload: {
    action: 'check',
    system: {
      available: true,
      current_version: '1.61.2',
      latest_version: '1.62.0',
    },
  },
};

export const FIXTURE_BACKUP_CREATED: ChatArtifact = {
  type: 'backup_result',
  id: 'fixture-backup-created',
  payload: {
    mode: 'created',
    result: { filename: 'duduclaw-backup-20260818.tar.gz', stdout: '', stderr: '' },
  },
};

export const FIXTURE_BACKUP_LIST: ChatArtifact = {
  type: 'backup_result',
  id: 'fixture-backup-list',
  payload: {
    mode: 'list',
    files: [
      { name: 'duduclaw-backup-20260818.tar.gz', size: 1_258_291, mtime: Date.now() - 3_600_000 },
      { name: 'duduclaw-backup-20260817.tar.gz', size: 1_198_000, mtime: Date.now() - 90_000_000 },
    ],
  },
};

export const FIXTURE_NETWORK_INFO: ChatArtifact = {
  type: 'network_info',
  id: 'fixture-network-info',
  payload: {
    interfaces: [
      { name: 'eth0', is_up: true, addresses: ['192.168.1.10'] },
      { name: 'wlan0', is_up: false, addresses: [] },
    ],
  },
};

export const FIXTURE_CONFIRM_RESTART: ChatArtifact = {
  type: 'confirm_action',
  id: 'fixture-confirm-restart',
  payload: { action: 'restart' },
};

export const FIXTURE_CONFIRM_FACTORY_RESET: ChatArtifact = {
  type: 'confirm_action',
  id: 'fixture-confirm-factory-reset',
  payload: { action: 'factory_reset' },
};

export const FIXTURE_UPDATE_CONFIRM_DEVICE: ChatArtifact = {
  type: 'update_confirm',
  id: 'fixture-update-confirm-device',
  payload: { target: 'device' },
};

export const FIXTURE_UPDATE_CONFIRM_SYSTEM: ChatArtifact = {
  type: 'update_confirm',
  id: 'fixture-update-confirm-system',
  payload: { target: 'system' },
};

export const FIXTURE_APPROVAL_REQUEST: ChatArtifact = {
  type: 'approval_request',
  id: 'fixture-approval-request',
  payload: {
    id: 'appr-1',
    agent_id: 'ops-agent',
    kind: 'tool_call',
    summary: '安裝系統更新並在完成後重新開機',
    payload: {},
    created_at: new Date().toISOString(),
    ttl_seconds: 300,
    expires_at: Math.floor(Date.now() / 1000) + 300,
    simulation: {
      world_state_change: '這台機器會離線約 2 分鐘，期間所有通道暫時無回應。',
      risk_points: ['服務短暫中斷'],
    },
    channel: null,
    channel_link: null,
  },
};

/** Every fixture, keyed by a stable id — used by the showcase test to render
 *  one of each artifact type in a single pass. */
export const ALL_FIXTURES: readonly ChatArtifact[] = [
  FIXTURE_DEVICE_STATUS,
  FIXTURE_UPDATE_STATUS,
  FIXTURE_UPDATE_STATUS_WITH_SYSTEM,
  FIXTURE_UPDATE_STATUS_SYSTEM_ONLY,
  FIXTURE_BACKUP_CREATED,
  FIXTURE_BACKUP_LIST,
  FIXTURE_NETWORK_INFO,
  FIXTURE_CONFIRM_RESTART,
  FIXTURE_CONFIRM_FACTORY_RESET,
  FIXTURE_UPDATE_CONFIRM_DEVICE,
  FIXTURE_UPDATE_CONFIRM_SYSTEM,
  FIXTURE_APPROVAL_REQUEST,
];
