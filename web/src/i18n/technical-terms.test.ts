import { describe, it, expect } from 'vitest';
import en from './en.json';
import zhTW from './zh-TW.json';
import jaJP from './ja-JP.json';

/**
 * DRAFT-no-linux-surface-2026-08 item 6: "內部技術詞不得進使用者文案" turned
 * into a mechanical check instead of a rule people have to remember to
 * follow. Mirrors `duduclaw-shell`'s existing convention for exactly this
 * kind of guard — `overlay/codrive_row.rs`'s
 * `no_user_facing_string_leaks_internal_vocabulary` test: a closed
 * lowercase-substring blacklist scanned over every user-facing string, with
 * named, justified exceptions rather than a softened rule.
 *
 * The appliance kiosk's stated bar (MAP-agent-native-os-2026-08 judgement
 * ①) is "開機即 agent，全程無『這是 Linux』的外露" — a person using the
 * device should never see systemd/journald/D-Bus vocabulary, a unit file
 * name, or a raw filesystem path. At the time this test was written a full
 * sweep of all three catalogues found zero violations (see
 * `commercial/docs/DRAFT-no-linux-surface-2026-08.md` item 6's own citation)
 * — this test is what keeps that true as new copy is added, not a one-time
 * cleanup.
 *
 * Scope: every string VALUE in the three locale catalogues (message ids are
 * internal identifiers, not shown to anyone, so they are not scanned).
 */

/** Closed blacklist. Grouped by category purely for readability — the check
 *  itself treats every entry the same way (lowercase substring match). Each
 *  group corresponds to a row in the DRAFT doc's item 6/7 evidence:
 *   - systemd unit vocabulary + the exact suffixes a unit file name ends in
 *   - the six binaries `duduclaw-sysd/src/dispatch.rs` shells out to
 *     (systemctl / bootctl / hostnamectl / timedatectl / networkctl /
 *     systemd-sysupdate) plus systemd-bless-boot (H3f)
 *   - boot/rescue vocabulary (items 1a/1b/5/12)
 *   - raw filesystem paths a shell-out's stdout/stderr could carry
 *   - other Linux plumbing named elsewhere in the appliance stack
 *     (nftables.conf, avahi-daemon, iwd, mkosi) that has no business in
 *     copy a person reads */
const FORBIDDEN_TERMS: readonly string[] = [
  // systemd unit vocabulary
  'systemd',
  'systemctl',
  'journalctl',
  'journal',
  '.service',
  '.timer',
  '.socket',
  '.mount',
  '.target',
  // the binaries dispatch.rs shells out to
  'bootctl',
  'hostnamectl',
  'timedatectl',
  'networkctl',
  'systemd-sysupdate',
  'systemd-bless-boot',
  'sysupdate',
  // boot / rescue vocabulary (items 1a/1b/5/12 — Yocto-line backlog, but the
  // words themselves must still never reach a dashboard string today)
  'getty',
  'sulogin',
  'rescue.target',
  'emergency.target',
  'initramfs',
  'plymouth',
  'efivarfs',
  'grub',
  // raw filesystem paths
  '/dev/',
  '/sys/',
  '/proc/',
  '/etc/',
  '/usr/lib/',
  '/run/',
  'zoneinfo',
  // other appliance-stack Linux plumbing (mkosi.conf / nftables.conf /
  // postinst.d — see this test's module doc comment)
  'dbus',
  'd-bus',
  'polkit',
  'nftables',
  'avahi',
  'iwd',
  'mkosi',
];

/** Keys with a deliberate, reviewed exception — named the same way
 *  `codrive_row.rs` carves out `HINT_HAND_BACK`: an explicit entry with a
 *  one-line reason, not a loosened rule. Both entries below are the
 *  `secret://file/...` advanced credential-source feature (`SecretRef`,
 *  see CLAUDE.md's "credentials 讀取單一化" note) — a power-user desktop
 *  settings screen a technical operator opts into, not appliance kiosk
 *  copy. Adding a key here must come with the same justification. */
const ALLOWED_EXCEPTIONS: ReadonlySet<string> = new Set([
  'security.credentialInventory.referenceHint',
  'secretSource.file.placeholder',
]);

function scanCatalogue(name: string, catalogue: Record<string, unknown>) {
  const violations: string[] = [];
  for (const [key, value] of Object.entries(catalogue)) {
    if (typeof value !== 'string') continue;
    if (ALLOWED_EXCEPTIONS.has(key)) continue;
    const lowered = value.toLowerCase();
    for (const term of FORBIDDEN_TERMS) {
      if (lowered.includes(term)) {
        violations.push(`${name}["${key}"] leaks internal token ${JSON.stringify(term)}: ${JSON.stringify(value)}`);
      }
    }
  }
  return violations;
}

describe('i18n catalogues never leak Linux/systemd implementation vocabulary', () => {
  it('en.json has no forbidden technical terms outside the reviewed exceptions', () => {
    const violations = scanCatalogue('en.json', en as Record<string, unknown>);
    expect(violations).toEqual([]);
  });

  it('zh-TW.json has no forbidden technical terms outside the reviewed exceptions', () => {
    const violations = scanCatalogue('zh-TW.json', zhTW as Record<string, unknown>);
    expect(violations).toEqual([]);
  });

  it('ja-JP.json has no forbidden technical terms outside the reviewed exceptions', () => {
    const violations = scanCatalogue('ja-JP.json', jaJP as Record<string, unknown>);
    expect(violations).toEqual([]);
  });

  it('every reviewed exception key still actually exists in each catalogue', () => {
    // Guards the allowlist itself against drift: if a key is ever renamed or
    // removed, its ALLOWED_EXCEPTIONS entry becomes silent dead weight that
    // no longer exempts anything real — this fails loudly instead.
    for (const key of ALLOWED_EXCEPTIONS) {
      expect(en, `en.json is missing exception key ${key}`).toHaveProperty(key);
      expect(zhTW, `zh-TW.json is missing exception key ${key}`).toHaveProperty(key);
      expect(jaJP as Record<string, unknown>, `ja-JP.json is missing exception key ${key}`).toHaveProperty(key);
    }
  });
});
