/**
 * Shared types for the 3D world stage.
 *
 * The world is *state-driven* (openhuman O2): the React controller
 * (`useWorldState`) pushes an authoritative {@link WorldAgentState} per agent;
 * the three.js renderer only interpolates toward it (per-frame lerp) and holds
 * no business logic. Keeping these types renderer-free lets the mapping layer be
 * unit tested in jsdom without a GPU.
 */

/** Tile codes in a room layout matrix (`matrix[y][x]`). */
export enum TileCode {
  Void = 0,
  Floor = 1,
  Wall = 2,
  Rug = 3,
  /** Town: asphalt road lane (cars cruise these rows). */
  Road = 4,
  /** Town: pavement / sidewalk flanking a road. */
  Pavement = 5,
  /** Town: grass / park ground fill. */
  Grass = 6,
  /** Town: water (central plaza pool). */
  Water = 7,
  /** Lounge: warm carpet zone. */
  Carpet = 8,
}

export type Facing = 'left' | 'right';

/** What an agent is visibly doing on the stage. */
export type WorldAction = 'idle' | 'walking' | 'sitting';

export interface TileCoord {
  readonly x: number;
  readonly y: number;
}

/**
 * Authoritative per-agent state the controller pushes to the renderer. Position
 * is in tile space; the renderer projects + smooths it.
 */
export interface WorldAgentState {
  /** Stable agent id — seeds the character tint (same `characterFor` source). */
  readonly id: string;
  /** Human display name for the nameplate. */
  readonly label: string;
  /** Target tile column. */
  readonly x: number;
  /** Target tile row. */
  readonly y: number;
  readonly action: WorldAction;
  readonly facing: Facing;
  /** 1..10 tint index (from `characterFor(id).tintIndex`), keeps world ⇄ UI in sync. */
  readonly tintIndex: number;
  /** Wardrobe composition; null = seeded default look (legacy accessory). */
  readonly outfit: import('@/lib/outfit').AgentOutfit | null;
  /** Head-top emote glyph, or null for a calm character. */
  readonly emote: WorldEmote | null;
  /** Whether the desk/nameplate should read as inactive (terminated). */
  readonly dimmed: boolean;
  /** Optional speech bubble text (already CJK-safe truncated). */
  readonly say?: string;
}

/** Head-top emote vocabulary, aligned with the UI `StatusEmote` kinds. */
export type WorldEmote = 'working' | 'blocked' | 'awaiting' | 'sleeping' | 'error' | 'celebrating';

/** Builder key the renderer switches on. */
export type FurnitureKind =
  | 'desk'
  | 'bulletin'
  | 'whiteboard'
  | 'coffee'
  | 'door'
  | 'vault'
  | 'plant'
  // Office upgrade
  | 'window'
  | 'cabinet'
  // Town
  | 'building'
  | 'tree'
  | 'streetlamp'
  | 'fountain'
  // Lounge
  | 'sofa'
  | 'boardgame'
  | 'coffeeMachine';

/** A furniture piece placed in a room. */
export interface FurniturePiece {
  readonly kind: FurnitureKind;
  /** Anchor tile (its floor cell). */
  readonly x: number;
  readonly y: number;
  /** Which agent index (0-based) owns this desk, if it's a desk. */
  readonly deskIndex?: number;
  /** The interactive object id this piece triggers on click, if any. */
  readonly object?: WorldObjectId;
  /**
   * Renderer variant selector — buildings use it to pick a roof colour, other
   * kinds may use it for shape variation. Stable per placement (no per-frame RNG).
   */
  readonly variant?: number;
  /** Extra elevation levels (buildings: storeys). */
  readonly level?: number;
}

/** Clickable world objects → routes (T8.4). */
export type WorldObjectId = 'bulletin' | 'whiteboard' | 'door' | 'vault' | 'coffee' | 'agent';

/**
 * Palette resolved from CSS tokens (numbers are 0xRRGGBB). The first five shades
 * are theme-tied (re-read on theme change); the trailing scene-tile colours are
 * scene-authored constants (roads/grass/water stay legible in both themes).
 */
export interface RoomPalette {
  readonly floor: number;
  readonly floorAlt: number;
  readonly wall: number;
  readonly wallSide: number;
  readonly rug: number;
  /** Scene-tile colours (town/lounge). Optional so office keeps its 5-shade shape. */
  readonly road?: number;
  readonly pavement?: number;
  readonly grass?: number;
  readonly water?: number;
  readonly carpet?: number;
}

/** Complete declarative room definition. */
export interface RoomDefinition {
  readonly key: string;
  /** Layout matrix `matrix[y][x]` of {@link TileCode}. */
  readonly matrix: ReadonlyArray<ReadonlyArray<TileCode>>;
  readonly furniture: ReadonlyArray<FurniturePiece>;
  /** Where agents without a desk (overflow / walking) spawn. */
  readonly spawn: TileCoord;
}

// ---- Multi-scene registry --------------------------------------------------

/** The built-in scene keys the world can render. */
export type SceneKey = 'office' | 'town' | 'lounge';

/** Where + how one agent is placed in a scene (pure geometry, no tint/label). */
export interface ScenePlacement {
  readonly x: number;
  readonly y: number;
  readonly action: WorldAction;
  readonly facing: Facing;
  readonly dimmed: boolean;
}

/** Minimal per-agent facts a scene needs to place a sprite. */
export interface PlacementInput {
  /** Stable roster index (fixes the desk / building / seat). */
  readonly index: number;
  readonly status: 'active' | 'paused' | 'terminated';
  /** True when the agent has a live, non-idle signal. */
  readonly busy: boolean;
}

/**
 * A declarative scene: metadata + a layout builder + a placement rule. The
 * builder and placer are pure so both are unit-tested without a GPU.
 */
export interface SceneDefinition {
  readonly key: SceneKey;
  /** i18n message id for the scene name. */
  readonly nameId: string;
  /** i18n message id for the one-line scene description. */
  readonly descId: string;
  /** Ambient animation the renderer runs for this scene. */
  readonly ambient: 'none' | 'cars';
  /**
   * Bake the static ground (floor + walls + furniture) into a single cached
   * texture. Enabled for town (large, no interactive furniture); off for
   * office/lounge which keep click-through interactive props.
   */
  readonly cacheGround: boolean;
  /** Build the tile + furniture layout for `agentCount` employees. */
  build(agentCount: number): RoomDefinition;
  /** Place one agent; `null` ⇒ the agent is not shown in this scene. */
  place(input: PlacementInput): ScenePlacement | null;
}
