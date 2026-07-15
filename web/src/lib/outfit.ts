import { characterFor } from '@/lib/character-gen';

/**
 * outfit — the wardrobe (衣帽間) model behind every AI-staff character.
 *
 * Six slots (hat / head / body / hands / feet / accessory) plus a tint
 * override compose the agent's look. The same outfit renders in three places:
 * the SVG `CharacterAvatar` (roster rows, detail hero, dialogs), the PixiJS
 * world scene, and the wardrobe dialog preview — one model, no drift.
 *
 * An agent with NO saved outfit falls back to `defaultOutfitFor(id)`, which
 * maps the legacy seeded accessory (character-gen) into slot form — existing
 * agents keep their exact pre-wardrobe look until someone dresses them.
 */

export const OUTFIT_SLOTS = ['hat', 'head', 'body', 'hands', 'feet', 'accessory'] as const;
export type OutfitSlot = (typeof OUTFIT_SLOTS)[number];

export interface AgentOutfit {
  schema: 1;
  /** 1–10 gradient tint override; 0 = keep the seeded tint. */
  tint: number;
  /** Item id per slot; '' = nothing in that slot. */
  hat: string;
  head: string;
  body: string;
  hands: string;
  feet: string;
  accessory: string;
}

/** Per-slot item vocabulary. Ids are stable wire values (persisted server-side);
 *  display names come from i18n keys `outfit.item.<slot>.<id>`. */
export const OUTFIT_CATALOG: Readonly<Record<OutfitSlot, readonly string[]>> = {
  hat: ['', 'cap', 'tophat', 'beret', 'crown', 'beanie', 'helmet'],
  head: ['', 'glasses', 'sunglasses', 'monocle', 'mask'],
  body: ['', 'tie', 'bowtie', 'suit', 'apron', 'sash'],
  hands: ['', 'coffee', 'briefcase', 'wrench', 'clipboard', 'phone'],
  feet: ['', 'sneakers', 'boots', 'skates'],
  accessory: ['', 'antenna', 'bow', 'flower', 'scarf', 'badge', 'halo'],
};

/** Empty outfit (all slots bare, seeded tint). */
export function emptyOutfit(): AgentOutfit {
  return { schema: 1, tint: 0, hat: '', head: '', body: '', hands: '', feet: '', accessory: '' };
}

/**
 * The look an agent has before anyone dresses it — the legacy seeded
 * accessory mapped into slot form, so pre-wardrobe agents look unchanged.
 */
export function defaultOutfitFor(agentId: string): AgentOutfit {
  const seeded = characterFor(agentId).accessory;
  const out = emptyOutfit();
  switch (seeded) {
    case 'cap':
      out.hat = 'cap';
      break;
    case 'glasses':
      out.head = 'glasses';
      break;
    // antenna / bow / scarf / flower live in the accessory slot.
    default:
      out.accessory = seeded;
  }
  return out;
}

/** Parse an untrusted wire value into a well-formed outfit (unknown item ids
 *  are kept — older clients must not destroy newer items — but shape is fixed).
 *  Returns null for null/undefined/garbage, meaning "no saved outfit". */
export function parseOutfit(raw: unknown): AgentOutfit | null {
  if (!raw || typeof raw !== 'object') return null;
  const o = raw as Record<string, unknown>;
  const slot = (k: OutfitSlot): string => (typeof o[k] === 'string' ? (o[k] as string) : '');
  const tint = typeof o.tint === 'number' && o.tint >= 1 && o.tint <= 10 ? Math.floor(o.tint) : 0;
  return {
    schema: 1,
    tint,
    hat: slot('hat'),
    head: slot('head'),
    body: slot('body'),
    hands: slot('hands'),
    feet: slot('feet'),
    accessory: slot('accessory'),
  };
}

/** Uniform-random outfit (for the dice button). Bare slots stay possible. */
export function randomOutfit(rng: () => number = Math.random): AgentOutfit {
  const pick = (items: readonly string[]) => items[Math.floor(rng() * items.length)] ?? '';
  return {
    schema: 1,
    tint: 1 + Math.floor(rng() * 10),
    hat: pick(OUTFIT_CATALOG.hat),
    head: pick(OUTFIT_CATALOG.head),
    body: pick(OUTFIT_CATALOG.body),
    hands: pick(OUTFIT_CATALOG.hands),
    feet: pick(OUTFIT_CATALOG.feet),
    accessory: pick(OUTFIT_CATALOG.accessory),
  };
}

/** Effective tint index for rendering: outfit override or the seeded tint. */
export function effectiveTint(agentId: string, outfit: AgentOutfit | null | undefined): number {
  if (outfit && outfit.tint >= 1 && outfit.tint <= 10) return outfit.tint;
  return characterFor(agentId).tintIndex;
}
