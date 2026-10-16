/**
 * Maintenance-mode SSH helpers for the dashboard
 * (`DESIGN-maintenance-ssh-account-2026-10.md` §4.2, §7).
 *
 * Everything here is advisory: the gateway and sysd parse the key with the
 * same strict parser and are the only authority. The pre-check mirrors the
 * server's order (`duduclaw-sysd/src/ssh_key.rs::parse`) so the hint a person
 * sees while typing is the same message the server would answer with.
 *
 * Pure functions only, so they can be unit-tested without React.
 */

/** The fixed maintenance login account on the OS image (design §3.1). */
export const MAINT_LOGIN_USER = 'duduclaw-maint';
/** The mDNS name the appliance announces on the LAN. */
export const MAINT_MDNS_HOST = 'duduclaw.local';

/** Same limit as the server (`ssh_key::MAX_INPUT_BYTES`). */
export const MAX_KEY_INPUT_BYTES = 2048;
const RSA_MIN_BITS = 3072;
const RSA_MAX_BITS = 8192;
const RSA_MAX_EXPONENT_BITS = 64;

/** Stable reasons of `ssh_key_invalid` (server `KeyError::code`). */
export type KeyReason =
  | 'empty'
  | 'too_long'
  | 'non_ascii'
  | 'multiple_lines'
  | 'private_key'
  | 'rfc4716_format'
  | 'options_not_allowed'
  | 'unsupported_type'
  | 'certificate_not_supported'
  | 'rsa_too_short'
  | 'rsa_too_long'
  | 'rsa_bad_exponent'
  | 'malformed';

const ACCEPTED_TYPES: ReadonlySet<string> = new Set([
  'ssh-ed25519',
  'sk-ssh-ed25519@openssh.com',
  'ecdsa-sha2-nistp256',
  'ecdsa-sha2-nistp384',
  'ecdsa-sha2-nistp521',
  'sk-ecdsa-sha2-nistp256@openssh.com',
  'ssh-rsa',
]);
const KNOWN_REJECTED_TYPES: ReadonlySet<string> = new Set([
  'ssh-dss',
  'ssh-xmss@openssh.com',
  'rsa-sha2-256',
  'rsa-sha2-512',
  'ecdsa-sha2-nistp224',
]);
const PRIVATE_MARKERS = ['PRIVATE KEY', 'PuTTY-User-Key-File', 'PRIVATE-KEY'];
const RFC4716_MARKER = 'BEGIN SSH2 PUBLIC KEY';

const isCertificateType = (name: string) => name.endsWith('-cert-v01@openssh.com');
const looksLikeKeyType = (token: string) =>
  ACCEPTED_TYPES.has(token) || isCertificateType(token) || KNOWN_REJECTED_TYPES.has(token);

/** True when the text carries a private-key marker (it must never be sent). */
export function looksLikePrivateKey(input: string): boolean {
  return PRIVATE_MARKERS.some((m) => input.includes(m));
}

/**
 * Client-side pre-check of a pasted public key. Returns the reason the server
 * would refuse it with, or `null` when it looks acceptable (the server still
 * decides). Never echoes or stores the input.
 */
export function precheckPublicKey(input: string): KeyReason | null {
  if (looksLikePrivateKey(input)) return 'private_key';
  if (input.includes(RFC4716_MARKER)) return 'rfc4716_format';
  if (new TextEncoder().encode(input).length > MAX_KEY_INPUT_BYTES) return 'too_long';
  const trimmed = input.replace(/^[ \t\r\n]+|[ \t\r\n]+$/g, '');
  if (trimmed === '') return 'empty';
  for (const ch of trimmed) {
    const code = ch.codePointAt(0) ?? 0;
    if (ch === '\n' || ch === '\r') return 'multiple_lines';
    if (code > 0x7f) return 'non_ascii';
    if (code < 0x20 || code === 0x7f) return 'malformed';
  }
  const fields = trimmed.split(' ').filter((f) => f !== '');
  const first = fields[0];
  if (!ACCEPTED_TYPES.has(first)) {
    if (isCertificateType(first)) return 'certificate_not_supported';
    if (KNOWN_REJECTED_TYPES.has(first)) return 'unsupported_type';
    if (fields.slice(1).some(looksLikeKeyType) || first.includes('=') || first.includes(',')) {
      return 'options_not_allowed';
    }
    return 'unsupported_type';
  }
  const encoded = fields[1];
  if (!encoded) return 'malformed';
  if (fields.slice(2).some(looksLikeKeyType)) return 'multiple_lines';
  const blob = decodeBase64Strict(encoded);
  if (!blob) return 'malformed';
  return checkBlob(first, blob);
}

/** Standard alphabet, correct padding, canonical re-encoding; else `null`. */
function decodeBase64Strict(encoded: string): Uint8Array | null {
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(encoded) || encoded.length % 4 !== 0) return null;
  let binary: string;
  try {
    binary = atob(encoded);
  } catch {
    return null;
  }
  if (btoa(binary) !== encoded) return null;
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
  return out;
}

/** Read one SSH wire string (u32 length + bytes). */
function readString(blob: Uint8Array, offset: number): { value: Uint8Array; next: number } | null {
  if (offset + 4 > blob.length) return null;
  const len =
    ((blob[offset] << 24) >>> 0) + (blob[offset + 1] << 16) + (blob[offset + 2] << 8) + blob[offset + 3];
  const start = offset + 4;
  if (start + len > blob.length) return null;
  return { value: blob.subarray(start, start + len), next: start + len };
}

function bitLength(bytes: Uint8Array): number {
  let i = 0;
  while (i < bytes.length && bytes[i] === 0) i += 1;
  if (i === bytes.length) return 0;
  return (bytes.length - i - 1) * 8 + (32 - Math.clz32(bytes[i]));
}

/** Light structural check: type string, and RSA sizes. The server checks the rest. */
function checkBlob(keyType: string, blob: Uint8Array): KeyReason | null {
  const typeField = readString(blob, 0);
  if (!typeField) return 'malformed';
  if (new TextDecoder().decode(typeField.value) !== keyType) return 'malformed';
  if (keyType !== 'ssh-rsa') return null;
  const e = readString(blob, typeField.next);
  if (!e) return 'malformed';
  const n = readString(blob, e.next);
  if (!n || n.next !== blob.length) return 'malformed';
  const eBits = bitLength(e.value);
  const eLast = e.value[e.value.length - 1] ?? 0;
  if (eBits === 0 || eBits > RSA_MAX_EXPONENT_BITS || (eLast & 1) === 0 || (eBits <= 2 && eLast < 3)) {
    return 'rsa_bad_exponent';
  }
  const nBits = bitLength(n.value);
  if (nBits < RSA_MIN_BITS) return 'rsa_too_short';
  if (nBits > RSA_MAX_BITS) return 'rsa_too_long';
  return null;
}

/** i18n id for a key reason (unknown reasons read as "malformed"). */
export function keyReasonMessageId(reason: string | null | undefined): string {
  switch (reason) {
    case 'empty':
      return 'device.maintenance.ssh.key.empty';
    case 'too_long':
      return 'device.maintenance.ssh.key.tooLong';
    case 'non_ascii':
      return 'device.maintenance.ssh.key.nonAscii';
    case 'multiple_lines':
      return 'device.maintenance.ssh.key.multiple';
    case 'private_key':
      return 'device.maintenance.ssh.key.private';
    case 'rfc4716_format':
      return 'device.maintenance.ssh.key.putty';
    case 'options_not_allowed':
      return 'device.maintenance.ssh.key.options';
    case 'unsupported_type':
      return 'device.maintenance.ssh.key.unsupported';
    case 'certificate_not_supported':
      return 'device.maintenance.ssh.key.certificate';
    case 'rsa_too_short':
    case 'rsa_too_long':
      return 'device.maintenance.ssh.key.rsaSize';
    case 'rsa_bad_exponent':
      return 'device.maintenance.ssh.key.rsaExponent';
    default:
      return 'device.maintenance.ssh.key.malformed';
  }
}

/** A message to show: an i18n id and its values. */
export interface MessageRef {
  id: string;
  values?: Record<string, string | number>;
}

/** The `{code, message, ...}` error a refused `maintenance.enable` rejects with. */
export interface MaintenanceRpcError {
  code: string;
  message?: string;
  reason?: string;
  remaining_attempts?: number;
  retry_after_seconds?: number;
  [key: string]: unknown;
}

export function asMaintenanceError(err: unknown): MaintenanceRpcError | null {
  if (err && typeof err === 'object' && typeof (err as { code?: unknown }).code === 'string') {
    return err as MaintenanceRpcError;
  }
  return null;
}

/** Whole minutes, rounded up, at least 1. */
export function minutesUntil(seconds: number | null | undefined): number {
  return Math.max(1, Math.ceil((seconds ?? 60) / 60));
}

/**
 * Map a refused `maintenance.enable` to a message. `null` for errors without
 * a known code (the caller falls back to the generic error formatter).
 */
export function enableErrorMessage(err: unknown): MessageRef | null {
  const e = asMaintenanceError(err);
  if (!e) return null;
  const p = 'device.maintenance.error.';
  switch (e.code) {
    case 'confirm_text_mismatch':
      return { id: `${p}confirmText` };
    case 'user_system_unavailable':
      return { id: `${p}userSystemUnavailable` };
    case 'invalid_request':
      return { id: `${p}invalidRequest` };
    case 'ssh_key_required':
      return { id: 'device.maintenance.ssh.key.empty' };
    case 'ssh_key_unexpected':
      return { id: `${p}keyUnexpected` };
    case 'ssh_key_invalid':
      return { id: keyReasonMessageId(e.reason) };
    case 'reauth_failed':
      return typeof e.remaining_attempts === 'number'
        ? { id: `${p}reauthFailed`, values: { n: e.remaining_attempts } }
        : { id: `${p}reauthFailedPlain` };
    case 'reauth_locked':
      return { id: 'device.maintenance.ssh.locked', values: { m: minutesUntil(e.retry_after_seconds) } };
    case 'gateway_not_owner':
      return { id: `${p}gatewayNotOwner` };
    case 'already_active':
      return { id: `${p}alreadyActive` };
    case 'ssh_unsupported':
      return { id: unsupportedMessageId(e.reason) };
    case 'ssh_unreachable':
      return { id: `${p}unreachable` };
    case 'ssh_busy':
      return { id: busyMessageId(e.reason) };
    case 'ssh_rate_limited':
      return { id: `${p}rateLimited` };
    case 'ssh_open_failed':
      return { id: `${p}openFailed` };
    case 'internal':
      return { id: `${p}internal` };
    default:
      return null;
  }
}

function busyMessageId(reason: string | undefined): string {
  switch (reason) {
    case 'key_installed':
      return 'device.maintenance.error.busy.keyInstalled';
    case 'cleanup_pending':
      return 'device.maintenance.error.busy.cleanupPending';
    case 'audit_pending':
      return 'device.maintenance.error.busy.auditPending';
    default:
      return 'device.maintenance.error.busy.unknown';
  }
}

/** Why the SSH option is off (status `ssh.reason` or `ssh_unsupported.reason`). */
export function unsupportedMessageId(reason: string | null | undefined): string {
  return reason === 'off_appliance'
    ? 'device.maintenance.ssh.offAppliance'
    : 'device.maintenance.ssh.unsupported';
}

/** Can the SSH option be offered? `false` = no; `null` = can't tell right now. */
export type SshAvailability = 'available' | 'unsupported' | 'unknown';

export function sshAvailability(ssh: { supported: boolean | null } | null | undefined): SshAvailability {
  if (!ssh) return 'unknown';
  if (ssh.supported === true) return 'available';
  if (ssh.supported === false) return 'unsupported';
  return 'unknown';
}

/** Close triggers sysd and the gateway report (`ssh.last_close.trigger`, `os_trigger`). */
export function closeTriggerMessageId(trigger: string | null | undefined): string {
  switch (trigger) {
    case 'reboot':
      return 'device.maintenance.ssh.trigger.reboot';
    case 'firewall_changed':
      return 'device.maintenance.ssh.trigger.firewallChanged';
    case 'deadline':
      return 'device.maintenance.ssh.trigger.deadline';
    case 'fallback_timer':
      return 'device.maintenance.ssh.trigger.fallbackTimer';
    case 'startup_cleanup':
      return 'device.maintenance.ssh.trigger.startupCleanup';
    case 'stop':
      return 'device.maintenance.ssh.trigger.stop';
    default:
      return 'device.maintenance.ssh.trigger.other';
  }
}

/** Every audit kind the history can show; anything else reads as "other". */
const AUDIT_KIND_IDS: Readonly<Record<string, string>> = {
  enabled: 'enabled',
  disabled: 'disabled',
  ttl_expired: 'ttlExpired',
  gateway_restart_revoked: 'gatewayRestartRevoked',
  reminder_sent: 'reminderSent',
  access_view_detail: 'accessViewDetail',
  enable_failed: 'enableFailed',
  enable_race_rolled_back: 'enableRaceRolledBack',
  enable_reauth_failed: 'enableReauthFailed',
  reauth_locked: 'reauthLocked',
  ssh_key_installed: 'sshKeyInstalled',
  ssh_key_removed: 'sshKeyRemoved',
  ssh_session_started: 'sshSessionStarted',
  ssh_session_ended: 'sshSessionEnded',
  ssh_auth_failed: 'sshAuthFailed',
  ssh_menu_action: 'sshMenuAction',
  ssh_orphan_closed: 'sshOrphanClosed',
  ssh_closed_by_os: 'sshClosedByOs',
  ssh_os_close_record: 'sshOsCloseRecord',
  ssh_reconcile_unconfirmed: 'sshReconcileUnconfirmed',
};

export const KNOWN_AUDIT_KINDS: readonly string[] = Object.keys(AUDIT_KIND_IDS);

export function auditKindMessageId(kind: string): string {
  const id = AUDIT_KIND_IDS[kind];
  return id ? `device.maintenance.history.kind.${id}` : 'device.maintenance.history.kind.other';
}

/**
 * Private IPv4 addresses a technician on the LAN or VPN could reach (port 22
 * only accepts RFC 1918, link-local and 100.64.0.0/10 sources), skipping
 * loopback and container bridges.
 */
export function reachableIpv4(
  interfaces: ReadonlyArray<{ name: string; is_up: boolean; addresses: readonly string[] }> | null | undefined,
): string[] {
  if (!interfaces) return [];
  const skip = /^(lo|docker|podman|veth|br-|cni|virbr|waydroid|lxc)/;
  const out: string[] = [];
  for (const iface of interfaces) {
    if (!iface.is_up || skip.test(iface.name)) continue;
    for (const addr of iface.addresses) {
      if (isPrivateIpv4(addr) && !out.includes(addr)) out.push(addr);
    }
  }
  return out;
}

function isPrivateIpv4(addr: string): boolean {
  const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(addr);
  if (!m || m.slice(1).some((part) => Number(part) > 255)) return false;
  const [a, b] = [Number(m[1]), Number(m[2])];
  if (a === 10) return true;
  if (a === 172 && b >= 16 && b <= 31) return true;
  if (a === 192 && b === 168) return true;
  if (a === 169 && b === 254) return true;
  if (a === 100 && b >= 64 && b <= 127) return true;
  return false;
}
