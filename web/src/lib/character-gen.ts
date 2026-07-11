/**
 * character-gen — the deterministic seed behind every AI-staff character
 * (dashboard-redesign-v2 §3.2). One agent id maps, forever, to the same face:
 * a brand tint (1–10, → the `--agent-{n}{a,b}` gradient pair), an accessory,
 * and a blink phase offset so a room full of characters never blinks in unison.
 *
 * Pure + stable by contract: same id → byte-identical result, no globals, no
 * time. The hash is FNV-1a (32-bit) mixed with a per-field salt so the three
 * fields are independent (tint and accessory don't correlate).
 */

export type CharacterAccessory =
  | 'antenna'
  | 'bow'
  | 'glasses'
  | 'cap'
  | 'scarf'
  | 'flower';

export interface CharacterTraits {
  /** 1–10, selects the `--agent-{n}a` / `--agent-{n}b` gradient pair. */
  readonly tintIndex: number;
  /** Head accessory. `antenna` is weighted highest (the house look). */
  readonly accessory: CharacterAccessory;
  /** Blink-animation start offset in ms, so characters blink out of phase. */
  readonly blinkSeedMs: number;
}

/** FNV-1a 32-bit hash of a string, returned as an unsigned 32-bit integer. */
function fnv1a(input: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    h ^= input.charCodeAt(i);
    // 32-bit FNV prime multiply via Math.imul to stay in int32 range.
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/**
 * Weighted accessory table. Antenna is the signature look (weight 5); the rest
 * share the tail evenly. Order + weights are load-bearing for the distribution
 * test — change them and update `character-gen.test.ts`.
 */
const ACCESSORY_WEIGHTS: ReadonlyArray<readonly [CharacterAccessory, number]> = [
  ['antenna', 5],
  ['bow', 2],
  ['glasses', 2],
  ['cap', 2],
  ['scarf', 2],
  ['flower', 2],
];

const ACCESSORY_TOTAL = ACCESSORY_WEIGHTS.reduce((sum, [, w]) => sum + w, 0);

function pickAccessory(roll: number): CharacterAccessory {
  let acc = roll % ACCESSORY_TOTAL;
  for (const [name, weight] of ACCESSORY_WEIGHTS) {
    if (acc < weight) return name;
    acc -= weight;
  }
  // Unreachable (weights sum to ACCESSORY_TOTAL); fail-safe to the house look.
  return 'antenna';
}

/** Blink phase window (ms). Wide enough that no two nearby seeds sync up. */
const BLINK_WINDOW_MS = 3600;

/**
 * Resolve the stable visual traits for an agent id. Empty / missing ids fall
 * back to a fixed neutral character so callers never crash on a blank name.
 */
export function characterFor(agentId: string | null | undefined): CharacterTraits {
  const id = agentId && agentId.length > 0 ? agentId : 'unknown';
  const tintHash = fnv1a(`${id}::tint`);
  const accHash = fnv1a(`${id}::accessory`);
  const blinkHash = fnv1a(`${id}::blink`);

  return {
    tintIndex: (tintHash % 10) + 1,
    accessory: pickAccessory(accHash),
    blinkSeedMs: blinkHash % BLINK_WINDOW_MS,
  };
}
