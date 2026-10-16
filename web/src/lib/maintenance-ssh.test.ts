import { describe, it, expect } from 'vitest';
// Public test keys copied from duduclaw-sysd/tests/fixtures/ssh_keys.
import keys from '@/test/fixtures/maintenance-ssh-keys.json';
import { precheckPublicKey, enableErrorMessage, reachableIpv4, sshAvailability, KNOWN_AUDIT_KINDS, auditKindMessageId } from './maintenance-ssh';
import en from '@/i18n/en.json';
import zh from '@/i18n/zh-TW.json';
import ja from '@/i18n/ja-JP.json';
const fixture = (name: string) => keys[name as keyof typeof keys];
describe('maintenance SSH', () => {
  it.each(['ed25519', 'ecdsa256', 'ecdsa384', 'ecdsa521', 'rsa3072', 'rsa8192'])('accepts shared server fixture %s', (name) => expect(precheckPublicKey(fixture(name))).toBeNull());
  it.each([
    ['', 'empty'], ['ssh-ed25519 nope', 'malformed'], ['x'.repeat(2049), 'too_long'],
    ['-----BEGIN OPENSSH PRIVATE KEY-----', 'private_key'], ['PuTTY-User-Key-File-3: ssh-ed25519', 'private_key'],
    ['---- BEGIN SSH2 PUBLIC KEY ----', 'rfc4716_format'], ['ssh-dss AAAA', 'unsupported_type'],
    ['ssh-ed25519-cert-v01@openssh.com AAAA', 'certificate_not_supported'],
  ])('rejects %s safely', (key, reason) => expect(precheckPublicKey(key)).toBe(reason));
  it('rejects options, several keys and non-ASCII comments', () => {
    const key = fixture('ed25519').trim();
    expect(precheckPublicKey(`command="x" ${key}`)).toBe('options_not_allowed');
    expect(precheckPublicKey(`${key}\n${key}`)).toBe('multiple_lines');
    expect(precheckPublicKey(`${key} 中文`)).toBe('non_ascii');
    expect(precheckPublicKey(fixture('rsa2048'))).toBe('rsa_too_short');
  });
  it('only offers explicitly supported SSH', () => {
    expect(sshAvailability(undefined)).toBe('unknown');
    expect(sshAvailability({ supported: null })).toBe('unknown');
    expect(sshAvailability({ supported: false })).toBe('unsupported');
    expect(sshAvailability({ supported: true })).toBe('available');
  });
  it('translates every error and audit kind in all three languages', () => {
    for (const code of ['gateway_not_owner', 'confirm_text_mismatch', 'user_system_unavailable', 'invalid_request', 'ssh_key_required', 'ssh_key_unexpected', 'reauth_failed', 'reauth_locked', 'already_active', 'ssh_unsupported', 'ssh_unreachable', 'ssh_busy', 'ssh_rate_limited', 'ssh_open_failed', 'internal']) {
      const ref = enableErrorMessage({ code, remaining_attempts: 2, retry_after_seconds: 61 });
      expect(ref, code).not.toBeNull();
      for (const cat of [en, zh, ja]) expect(cat, code).toHaveProperty(ref!.id);
    }
    for (const kind of KNOWN_AUDIT_KINDS) for (const cat of [en, zh, ja]) expect(cat, kind).toHaveProperty(auditKindMessageId(kind));
  });
  it('offers valid LAN and VPN addresses without virtual bridges', () => {
    const iface = (name: string, ...addresses: string[]) => ({ name, is_up: true, addresses });
    expect(reachableIpv4([iface('eth0', '10.0.0.2', '100.64.0.2', '100.128.0.1', '10.0.999.2'), iface('docker0', '172.17.0.1')])).toEqual(['10.0.0.2', '100.64.0.2']);
  });
});
