/**
 * PixiJS 2D isometric scene controller for the world stage.
 *
 * RUNTIME-ONLY (not unit-tested in jsdom — no GPU/WebGL). The pure numbers it
 * consumes (iso projection + camera maths in `geometry.ts`) and the state it
 * renders (`WorldAgentState`, the scene registry, the traffic model) are tested
 * separately. PixiJS is loaded via dynamic `import()` so it lands in a lazy
 * chunk and never bloats the main bundle.
 *
 * CSP note (load-bearing — do not remove): the dashboard runs under
 * `script-src 'self'` with NO `unsafe-eval`. PixiJS's WebGLRenderer builds its
 * uniform-upload functions with `new Function(...)`, which that CSP blocks and
 * throws on first draw. The fix is to import the official `pixi.js/unsafe-eval`
 * polyfill *eagerly alongside* pixi so it installs before any renderer is built:
 *   await Promise.all([import('pixi.js'), import('pixi.js/unsafe-eval')]);
 * Additionally, WebGPU adapter acquisition can hang forever inside embedding
 * shells, so we force `preference:'webgl'` and race init against a 10s timeout.
 *
 * Camera: a single pannable/zoomable world container (no rotation — this is 2D).
 * Wheel / pinch zoom (cursor-anchored, 0.5×–2.5× clamp) and drag-to-pan
 * (bounds-clamped); a ⟲ recenter snaps back to the contain-fit framing. Drags
 * and taps are told apart by a movement threshold so a tap still triggers the
 * clicked prop's route.
 */
import type * as PIXI from 'pixi.js';
import {
  tileToScreen,
  tileCenterToScreen,
  depthAt,
  isoGridBounds,
  containFit,
  clampZoom,
  clampPan,
  zoomAtPoint,
  lerp,
  smoothAlpha,
  HALF_TILE_WIDTH,
  HALF_TILE_HEIGHT,
  ELEVATION_HEIGHT,
  LAYER_FURNITURE,
  LAYER_AGENT,
  MIN_ZOOM,
  MAX_ZOOM,
  type Bounds,
} from './geometry';
import { resolveAgentTint } from './rooms';
import { characterFor, type CharacterAccessory } from '@/lib/character-gen';
import { advanceCar, findHorizontalLanes, seedCars, type CarState } from './traffic';
import {
  TileCode,
  type FurnitureKind,
  type RoomDefinition,
  type RoomPalette,
  type SceneDefinition,
  type WorldAgentState,
  type WorldObjectId,
} from './types';

const INIT_TIMEOUT_MS = 10_000;
const RENDERER_PREFERENCE = 'webgl' as const;
/** Movement (px) beyond which a pointer gesture is a drag, not a tap. */
const TAP_THRESHOLD = 6;

/** Thrown when renderer init rejects or exceeds {@link INIT_TIMEOUT_MS}. */
export class RendererInitError extends Error {
  public constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = 'RendererInitError';
  }
}

export interface WorldSceneOptions {
  readonly palette: RoomPalette;
  readonly room: RoomDefinition;
  /** The active scene (drives ambient traffic + ground caching). */
  readonly scene: SceneDefinition;
  /** Invoked when a clickable prop / agent is tapped (not dragged). */
  readonly onObject: (object: WorldObjectId, agentId?: string) => void;
}

/** Roof colours for town buildings (≥6, warm/varied). Indexed by `variant`. */
const ROOF_TINTS = [
  0xd98b5a, 0x6aa4d9, 0x7ab87a, 0xd9a34a, 0xc57ab0, 0xe0685f, 0x8a93b5, 0xcf7f4f,
] as const;

const BUILDING_WALL = 0xe4d8c4;
const WALL_HEIGHT = ELEVATION_HEIGHT * 2.4;

const EMOTE_GLYPH: Record<NonNullable<WorldAgentState['emote']>, string> = {
  working: '💻',
  blocked: '⚠️',
  awaiting: '✋',
  sleeping: '💤',
  error: '😵',
  celebrating: '🎉',
};

/** Kinds drawn into the (optionally cached) static ground layer. */
const GROUND_DECOR: ReadonlySet<FurnitureKind> = new Set<FurnitureKind>([
  'building', 'tree', 'streetlamp', 'fountain', 'window', 'cabinet', 'plant', 'sofa', 'boardgame',
]);

interface ClickTarget {
  readonly node: PIXI.Container;
  readonly object: WorldObjectId;
  readonly agentId?: string;
}

interface CarNode {
  readonly state: CarState;
  readonly node: PIXI.Container;
}

interface AgentNode {
  container: PIXI.Container;
  bob: PIXI.Container;
  emote: PIXI.Text;
  bubble: PIXI.Container;
  bubbleText: PIXI.Text;
  bubbleBg: PIXI.Graphics;
  cur: { x: number; y: number };
  target: { x: number; y: number };
  action: WorldAgentState['action'];
  dimmed: boolean;
  bobPhase: number;
  bubbleAlpha: number;
  bubbleWant: number;
  lastEmote: string | null;
  lastSay: string | null;
}

type Pixi = typeof PIXI;

export class WorldScene {
  private PX: Pixi | null = null;
  private app: PIXI.Application | null = null;
  /** The pannable/zoomable camera container (holds ground + actors). */
  private world: PIXI.Container | null = null;
  private ground: PIXI.Container | null = null;
  private actors: PIXI.Container | null = null;
  private resizeObserver: ResizeObserver | null = null;

  private readonly agents = new Map<string, AgentNode>();
  private readonly cars: CarNode[] = [];
  private readonly clickTargets: ClickTarget[] = [];
  private carBounds: readonly [number, number] = [0, 0];

  private cols = 0;
  private rows = 0;
  private maxLevel = 0;
  private contentBounds: Bounds = { minX: 0, minY: 0, width: 1, height: 1 };
  private baseFit = { scale: 1, x: 0, y: 0 };
  private zoom = 1;
  private userAdjusted = false;

  // Gesture state.
  private readonly pointers = new Map<number, { x: number; y: number }>();
  private dragMoved = 0;
  private pinchDist = 0;

  private readonly opts: WorldSceneOptions;

  public constructor(opts: WorldSceneOptions) {
    this.opts = opts;
  }

  // ---- Init ---------------------------------------------------------------

  public async init(parent: HTMLElement): Promise<void> {
    if (this.app) return;
    // CSP: the unsafe-eval polyfill MUST install before any renderer is built.
    const [PX] = await Promise.all([import('pixi.js'), import('pixi.js/unsafe-eval')]);
    this.PX = PX;

    const matrix = this.opts.room.matrix;
    this.rows = matrix.length;
    this.cols = matrix[0]?.length ?? 0;
    this.maxLevel = this.opts.room.furniture.reduce((m, f) => Math.max(m, f.level ?? 0), 0);

    const width = parent.clientWidth || 600;
    const height = parent.clientHeight || 400;

    const app = new PX.Application();
    await this.withTimeout(
      app.init({
        preference: RENDERER_PREFERENCE,
        width,
        height,
        antialias: true,
        backgroundAlpha: 0,
        resolution: Math.min(globalThis.devicePixelRatio || 1, 2),
        autoDensity: true,
      }),
    );
    this.app = app;

    const world = new PX.Container();
    const ground = new PX.Container();
    const actors = new PX.Container();
    ground.sortableChildren = true;
    actors.sortableChildren = true;
    world.addChild(ground, actors);
    app.stage.addChild(world);
    this.world = world;
    this.ground = ground;
    this.actors = actors;

    this.buildGround();
    this.buildFurniture();
    if (this.opts.scene.cacheGround) ground.cacheAsTexture(true);
    this.spawnTraffic();

    // Frame the whole scene.
    this.contentBounds = isoGridBounds(this.cols, this.rows, this.maxLevel);
    this.recomputeFit(width, height);
    this.applyFit();

    // Canvas mount + input wiring.
    const canvas = app.canvas;
    canvas.style.width = '100%';
    canvas.style.height = '100%';
    canvas.style.display = 'block';
    canvas.style.touchAction = 'none';
    parent.appendChild(canvas);

    app.stage.eventMode = 'static';
    app.stage.hitArea = app.screen;
    app.stage.on('pointerdown', this.onPointerDown);
    app.stage.on('pointermove', this.onPointerMove);
    app.stage.on('pointerup', this.onPointerUp);
    app.stage.on('pointerupoutside', this.onPointerUp);
    app.stage.on('pointercancel', this.onPointerUp);
    canvas.addEventListener('wheel', this.onWheel, { passive: false });

    this.observeResize(parent);
    app.ticker.add(this.tick);
  }

  private async withTimeout<T>(promise: Promise<T>): Promise<T> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timeout = new Promise<never>((_r, reject) => {
      timer = setTimeout(
        () => reject(new RendererInitError(`renderer init timed out after ${INIT_TIMEOUT_MS}ms`)),
        INIT_TIMEOUT_MS,
      );
    });
    try {
      return await Promise.race([
        promise.catch((error: unknown) => {
          const message = error instanceof Error ? error.message : String(error);
          throw new RendererInitError(`renderer init failed: ${message}`, { cause: error });
        }),
        timeout,
      ]);
    } finally {
      if (timer !== undefined) clearTimeout(timer);
    }
  }

  // ---- Ground (floor + walls, each batched into one Graphics) -------------

  private buildGround(): void {
    const PX = this.PX!;
    const ground = this.ground!;
    const matrix = this.opts.room.matrix;
    const pal = this.opts.palette;

    const floor = new PX.Graphics();
    const walls = new PX.Graphics();

    for (let y = 0; y < matrix.length; y++) {
      const row = matrix[y];
      for (let x = 0; x < row.length; x++) {
        const code = row[x];
        if (code === TileCode.Void) continue;
        const top = tileToScreen(x, y);
        if (code === TileCode.Wall) {
          // A floor cell beneath so no void shows through, plus an extruded block.
          this.diamond(floor, top.x, top.y, pal.floorAlt);
          this.wallBlock(walls, top.x, top.y, pal.wall);
        } else if (code === TileCode.Water) {
          this.diamond(floor, top.x, top.y, pal.water ?? 0x5aa9d6, 0.85);
        } else {
          this.diamond(floor, top.x, top.y, this.tileColor(code, x, y, pal));
        }
      }
    }
    floor.zIndex = -1000;
    walls.zIndex = -500;
    ground.addChild(floor, walls);
  }

  /** Draw one iso floor diamond with its top corner at `(sx, sy)`. */
  private diamond(g: PIXI.Graphics, sx: number, sy: number, color: number, alpha = 1): void {
    g.poly([
      sx, sy,
      sx + HALF_TILE_WIDTH, sy + HALF_TILE_HEIGHT,
      sx, sy + HALF_TILE_HEIGHT * 2,
      sx - HALF_TILE_WIDTH, sy + HALF_TILE_HEIGHT,
    ]).fill({ color, alpha });
    // Subtle edge so tiles read as a grid.
    g.stroke({ color: shade(color, 0.9), width: 1, alpha: 0.35 * alpha });
  }

  /** An extruded back-wall block above a tile's top corner `(sx, sy)`. */
  private wallBlock(g: PIXI.Graphics, sx: number, sy: number, color: number): void {
    const h = WALL_HEIGHT;
    const top = shade(color, 1.08);
    const left = shade(color, 0.78);
    const right = shade(color, 0.92);
    // Top face (diamond) lifted by h.
    g.poly([
      sx, sy - h,
      sx + HALF_TILE_WIDTH, sy + HALF_TILE_HEIGHT - h,
      sx, sy + HALF_TILE_HEIGHT * 2 - h,
      sx - HALF_TILE_WIDTH, sy + HALF_TILE_HEIGHT - h,
    ]).fill({ color: top });
    // Left face.
    g.poly([
      sx - HALF_TILE_WIDTH, sy + HALF_TILE_HEIGHT - h,
      sx, sy + HALF_TILE_HEIGHT * 2 - h,
      sx, sy + HALF_TILE_HEIGHT * 2,
      sx - HALF_TILE_WIDTH, sy + HALF_TILE_HEIGHT,
    ]).fill({ color: left });
    // Right face.
    g.poly([
      sx + HALF_TILE_WIDTH, sy + HALF_TILE_HEIGHT - h,
      sx, sy + HALF_TILE_HEIGHT * 2 - h,
      sx, sy + HALF_TILE_HEIGHT * 2,
      sx + HALF_TILE_WIDTH, sy + HALF_TILE_HEIGHT,
    ]).fill({ color: right });
  }

  private tileColor(code: TileCode, x: number, y: number, pal: RoomPalette): number {
    const checker = (x + y) % 2 === 0;
    switch (code) {
      case TileCode.Rug:
        return pal.rug;
      case TileCode.Road:
        return pal.road ?? 0x565b66;
      case TileCode.Pavement:
        return pal.pavement ?? 0xc2c7cf;
      case TileCode.Grass:
        return checker ? (pal.grass ?? 0x7cb473) : shade(pal.grass ?? 0x7cb473, 1.07);
      case TileCode.Carpet:
        return checker ? (pal.carpet ?? 0xcf9f6f) : shade(pal.carpet ?? 0xcf9f6f, 1.08);
      default:
        return checker ? pal.floor : pal.floorAlt;
    }
  }

  // ---- Furniture ----------------------------------------------------------

  private buildFurniture(): void {
    const PX = this.PX!;
    for (const piece of this.opts.room.furniture) {
      const node = new PX.Container();
      this.drawFurniture(node, piece.kind, piece.variant ?? 0, piece.level ?? 1);
      const centre = tileCenterToScreen(piece.x, piece.y);
      node.position.set(centre.x, centre.y);

      const interactive = !!piece.object;
      const inGround = GROUND_DECOR.has(piece.kind) && !interactive;
      node.zIndex = depthAt(piece.x, piece.y, piece.level ?? 0, LAYER_FURNITURE);
      if (inGround) {
        this.ground!.addChild(node);
      } else {
        this.actors!.addChild(node);
        if (piece.object && piece.object !== 'agent') {
          node.eventMode = 'static';
          node.cursor = 'pointer';
          this.clickTargets.push({ node, object: piece.object });
        }
      }
    }
  }

  private drawFurniture(node: PIXI.Container, kind: FurnitureKind, variant: number, level: number): void {
    const PX = this.PX!;
    const g = new PX.Graphics();
    node.addChild(g);
    const pal = this.opts.palette;
    switch (kind) {
      case 'building':
        this.drawBuilding(g, variant, level);
        break;
      case 'tree':
        this.prism(g, 5, 3, 20, 0x8a5a34);
        g.circle(0, -30, 18).fill({ color: [0x4f9a52, 0x5cab5c, 0x63b863][variant % 3] });
        g.circle(-10, -22, 12).fill({ color: shade([0x4f9a52, 0x5cab5c, 0x63b863][variant % 3], 1.1) });
        break;
      case 'streetlamp':
        this.prism(g, 3, 2, 46, 0x4a4f57);
        g.circle(0, -50, 6).fill({ color: 0xf6e6a8 });
        break;
      case 'fountain':
        this.prism(g, 26, 15, 8, 0x9aa7b0);
        g.ellipse(0, -8, 18, 9).fill({ color: pal.water ?? 0x5aa9d6, alpha: 0.9 });
        g.circle(0, -20, 5).fill({ color: pal.water ?? 0x5aa9d6 });
        break;
      case 'sofa':
        this.prism(g, 26, 15, 12, 0x6f8bbf);
        this.prism(g, 26, 8, 20, 0x7f9bcf, -8);
        break;
      case 'boardgame':
        this.prism(g, 20, 12, 12, 0x8a6d4b);
        g.ellipse(0, -12, 12, 6).fill({ color: 0xf0e6d2 });
        break;
      case 'coffeeMachine':
      case 'coffee':
        this.prism(g, 12, 8, 26, 0x5b616b);
        g.circle(0, -20, 4).fill({ color: 0xe86a5a });
        break;
      case 'window':
        this.prism(g, 20, 6, WALL_HEIGHT * 0.5, 0x8fc7e8, -WALL_HEIGHT * 0.3);
        break;
      case 'cabinet':
        this.prism(g, 15, 10, 30, 0xa5825b);
        break;
      case 'desk':
        this.prism(g, 24, 14, 20, 0xa5825b);
        break;
      case 'bulletin':
        this.prism(g, 20, 5, WALL_HEIGHT * 0.55, 0xcf9b6a, -WALL_HEIGHT * 0.2);
        break;
      case 'whiteboard':
        this.prism(g, 22, 5, WALL_HEIGHT * 0.55, 0xf6f6f2, -WALL_HEIGHT * 0.2);
        break;
      case 'door':
        this.prism(g, 14, 6, 40, 0x7a5638);
        g.circle(8, -20, 2.5).fill({ color: 0xf2c14e });
        break;
      case 'vault':
        this.prism(g, 18, 11, 34, 0x5b616b);
        g.circle(0, -18, 6).stroke({ color: 0xd7b45a, width: 3 });
        break;
      case 'plant':
        this.prism(g, 8, 5, 10, 0xb07a4c);
        g.circle(0, -16, 10).fill({ color: 0x6aa86a });
        break;
      default:
        break;
    }
  }

  /** A stylised low building: body prism + tinted roof + emissive windows. */
  private drawBuilding(g: PIXI.Graphics, variant: number, level: number): void {
    const storeys = Math.max(1, Math.min(3, level));
    const h = 26 + storeys * 26;
    const hw = HALF_TILE_WIDTH * 0.62;
    const hh = HALF_TILE_HEIGHT * 0.62;
    this.prism(g, hw, hh, h, BUILDING_WALL);
    // Roof cap.
    const roof = ROOF_TINTS[variant % ROOF_TINTS.length];
    g.poly([
      0, -h - 8,
      hw, -h + hh - 8,
      0, -h + hh * 2 - 8,
      -hw, -h + hh - 8,
    ]).fill({ color: roof });
    // Windows on the two visible faces.
    for (let s = 0; s < storeys; s++) {
      const wy = -(h / (storeys + 1)) * (s + 1);
      const lit = (s + variant) % 2 === 0;
      const c = lit ? 0xf6e6a8 : 0x6f7c8c;
      g.rect(-hw * 0.55, wy - 5, 7, 10).fill({ color: c });
      g.rect(hw * 0.3, wy - 5, 7, 10).fill({ color: c });
    }
  }

  /**
   * Draw an iso prism: a `2·hw × 2·hh` diamond footprint extruded `height` px up.
   * `yOff` lifts the whole prism (for wall-mounted props). Light comes from the
   * upper-right: top brightest, right mid, left darkest.
   */
  private prism(g: PIXI.Graphics, hw: number, hh: number, height: number, color: number, yOff = 0): void {
    const topY = -height + yOff;
    const top = shade(color, 1.12);
    const left = shade(color, 0.72);
    const right = shade(color, 0.92);
    // Left face.
    g.poly([-hw, topY + hh, 0, topY + hh * 2, 0, yOff + hh * 2, -hw, yOff + hh]).fill({ color: left });
    // Right face.
    g.poly([hw, topY + hh, 0, topY + hh * 2, 0, yOff + hh * 2, hw, yOff + hh]).fill({ color: right });
    // Top face.
    g.poly([0, topY, hw, topY + hh, 0, topY + hh * 2, -hw, topY + hh]).fill({ color: top });
  }

  // ---- Traffic ------------------------------------------------------------

  private spawnTraffic(): void {
    if (this.opts.scene.ambient !== 'cars') return;
    const PX = this.PX!;
    const matrix = this.opts.room.matrix;
    const cols = matrix[0]?.length ?? 0;
    const lanes = findHorizontalLanes(matrix);
    this.carBounds = [-2, cols + 2];
    for (const state of seedCars(lanes, cols)) {
      const node = new PX.Container();
      const g = new PX.Graphics();
      this.prism(g, 18, 9, 12, state.tint);
      this.prism(g, 11, 6, 20, shade(state.tint, 1.18), -8);
      node.addChild(g);
      this.positionCar(state, node);
      this.actors!.addChild(node);
      this.cars.push({ state, node });
    }
  }

  private positionCar(state: CarState, node: PIXI.Container): void {
    const c = tileCenterToScreen(state.x, state.row);
    node.position.set(c.x, c.y);
    node.zIndex = depthAt(state.x, state.row, 0, LAYER_FURNITURE);
  }

  // ---- Agents -------------------------------------------------------------

  public updateAgents(states: ReadonlyArray<WorldAgentState>): void {
    if (!this.PX || !this.actors) return;
    const seen = new Set<string>();
    for (const s of states) {
      seen.add(s.id);
      let node = this.agents.get(s.id);
      if (!node) {
        node = this.createAgentNode(s);
        this.agents.set(s.id, node);
        this.actors.addChild(node.container);
        this.clickTargets.push({ node: node.container, object: 'agent', agentId: s.id });
      }
      node.target = { x: s.x, y: s.y };
      node.action = s.action;
      if (node.dimmed !== s.dimmed) {
        node.bob.alpha = s.dimmed ? 0.42 : 1;
        node.dimmed = s.dimmed;
      }
      if (s.emote !== node.lastEmote) {
        node.emote.text = s.emote ? EMOTE_GLYPH[s.emote] : '';
        node.emote.visible = !!s.emote;
        node.lastEmote = s.emote ?? null;
      }
      if (s.say && s.say !== node.lastSay) {
        node.bubbleText.text = s.say;
        this.paintBubbleBg(node);
        node.lastSay = s.say;
      }
      node.bubbleWant = s.say ? 1 : 0;
    }
    for (const [id, node] of this.agents) {
      if (!seen.has(id)) {
        node.container.destroy({ children: true });
        this.agents.delete(id);
        const i = this.clickTargets.findIndex((t) => t.object === 'agent' && t.agentId === id);
        if (i >= 0) this.clickTargets.splice(i, 1);
      }
    }
  }

  private createAgentNode(s: WorldAgentState): AgentNode {
    const PX = this.PX!;
    const container = new PX.Container();
    const bob = new PX.Container();
    container.addChild(bob);

    const tint = resolveAgentTint(s.tintIndex);
    const traits = characterFor(s.id);

    // Contact shadow.
    const shadow = new PX.Graphics();
    shadow.ellipse(0, 0, 16, 8).fill({ color: 0x000000, alpha: 0.16 });
    bob.addChild(shadow);

    // Body + head + eyes.
    const body = new PX.Graphics();
    body.roundRect(-13, -34, 26, 30, 12).fill({ color: tint });
    body.circle(0, -40, 12).fill({ color: shade(tint, 1.14) });
    body.circle(-4, -42, 2).fill({ color: 0x2a2320 });
    body.circle(4, -42, 2).fill({ color: 0x2a2320 });
    this.drawAccessory(body, resolveAccessory(traits.accessory), tint);
    bob.addChild(body);

    // Nameplate (always on).
    const nameBg = new PX.Graphics();
    const name = new PX.Text({
      text: s.label,
      style: { fontFamily: 'system-ui, sans-serif', fontSize: 12, fontWeight: '600', fill: 0xffffff },
    });
    name.anchor.set(0.5, 0.5);
    name.position.set(0, -64);
    const pad = 6;
    nameBg.roundRect(-name.width / 2 - pad, -64 - name.height / 2 - 3, name.width + pad * 2, name.height + 6, 6)
      .fill({ color: 0x1c1917, alpha: 0.66 });
    container.addChild(nameBg, name);

    // Emote glyph.
    const emote = new PX.Text({
      text: s.emote ? EMOTE_GLYPH[s.emote] : '',
      style: { fontFamily: 'system-ui, sans-serif', fontSize: 18, fill: 0xffffff },
    });
    emote.anchor.set(0.5, 1);
    emote.position.set(14, -50);
    emote.visible = !!s.emote;
    container.addChild(emote);

    // Speech bubble (fades).
    const bubble = new PX.Container();
    const bubbleBg = new PX.Graphics();
    const bubbleText = new PX.Text({
      text: s.say ?? '',
      style: { fontFamily: 'system-ui, sans-serif', fontSize: 12, fill: 0x1c1917 },
    });
    bubbleText.anchor.set(0.5, 0.5);
    bubble.addChild(bubbleBg, bubbleText);
    bubble.position.set(0, -84);
    bubble.visible = false;
    container.addChild(bubble);

    const centre = tileCenterToScreen(s.x, s.y);
    container.position.set(centre.x, centre.y);
    container.zIndex = depthAt(s.x, s.y, 0, LAYER_AGENT);

    const node: AgentNode = {
      container, bob, emote, bubble, bubbleText, bubbleBg,
      cur: { x: s.x, y: s.y },
      target: { x: s.x, y: s.y },
      action: s.action,
      dimmed: false,
      bobPhase: (traits.blinkSeedMs / 3600) * Math.PI * 2,
      bubbleAlpha: 0,
      bubbleWant: s.say ? 1 : 0,
      lastEmote: s.emote ?? null,
      lastSay: s.say ?? null,
    };
    if (s.say) this.paintBubbleBg(node);
    if (s.dimmed) bob.alpha = 0.42;
    node.dimmed = s.dimmed;
    return node;
  }

  private paintBubbleBg(node: AgentNode): void {
    const t = node.bubbleText;
    const pad = 8;
    node.bubbleBg.clear();
    node.bubbleBg
      .roundRect(-t.width / 2 - pad, -t.height / 2 - 5, t.width + pad * 2, t.height + 10, 8)
      .fill({ color: 0xffffff, alpha: 0.96 });
  }

  private drawAccessory(g: PIXI.Graphics, model: AccessoryModel, tint: number): void {
    switch (model) {
      case 'glasses':
        g.circle(-4, -42, 4).stroke({ color: 0x2a2320, width: 1.5 });
        g.circle(4, -42, 4).stroke({ color: 0x2a2320, width: 1.5 });
        break;
      case 'cap':
        g.ellipse(0, -50, 13, 6).fill({ color: shade(tint, 0.7) });
        g.rect(-4, -52, 16, 4).fill({ color: shade(tint, 0.7) });
        break;
      case 'bow':
        g.poly([-2, -52, -10, -56, -10, -48]).fill({ color: 0xef7fa2 });
        g.poly([2, -52, 10, -56, 10, -48]).fill({ color: 0xef7fa2 });
        g.circle(0, -52, 2).fill({ color: 0xef7fa2 });
        break;
      default: // antenna
        g.rect(-0.6, -58, 1.2, 8).fill({ color: 0x4a4f57 });
        g.circle(0, -59, 2.5).fill({ color: shade(tint, 1.3) });
    }
  }

  // ---- Camera -------------------------------------------------------------

  private recomputeFit(w: number, h: number): void {
    this.baseFit = containFit(this.contentBounds, w, h, 28, { min: MIN_ZOOM, max: MAX_ZOOM });
  }

  private applyFit(): void {
    const world = this.world;
    if (!world) return;
    this.zoom = this.baseFit.scale;
    world.scale.set(this.zoom);
    world.position.set(this.baseFit.x, this.baseFit.y);
  }

  /** Snap the camera back to the contain-fit framing. */
  public resetCamera(): void {
    this.userAdjusted = false;
    this.applyFit();
  }

  private applyZoom(nextScale: number, anchorX: number, anchorY: number): void {
    const world = this.world;
    const app = this.app;
    if (!world || !app) return;
    const clamped = clampZoom(nextScale, MIN_ZOOM, MAX_ZOOM);
    if (clamped === this.zoom) return;
    const pos = zoomAtPoint({ x: world.position.x, y: world.position.y }, this.zoom, clamped, anchorX, anchorY);
    this.zoom = clamped;
    world.scale.set(clamped);
    const bounded = clampPan(pos, this.contentBounds, clamped, app.screen.width, app.screen.height);
    world.position.set(bounded.x, bounded.y);
    this.userAdjusted = true;
  }

  private panBy(dx: number, dy: number): void {
    const world = this.world;
    const app = this.app;
    if (!world || !app) return;
    const bounded = clampPan(
      { x: world.position.x + dx, y: world.position.y + dy },
      this.contentBounds, this.zoom, app.screen.width, app.screen.height,
    );
    world.position.set(bounded.x, bounded.y);
    this.userAdjusted = true;
  }

  private readonly onWheel = (e: WheelEvent): void => {
    e.preventDefault();
    const canvas = this.app?.canvas;
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    const factor = Math.exp(-e.deltaY * 0.0015);
    this.applyZoom(this.zoom * factor, e.clientX - rect.left, e.clientY - rect.top);
  };

  private readonly onPointerDown = (e: PIXI.FederatedPointerEvent): void => {
    this.pointers.set(e.pointerId, { x: e.global.x, y: e.global.y });
    this.dragMoved = 0;
    if (this.pointers.size === 2) {
      const [a, b] = [...this.pointers.values()];
      this.pinchDist = Math.hypot(a.x - b.x, a.y - b.y);
    }
  };

  private readonly onPointerMove = (e: PIXI.FederatedPointerEvent): void => {
    const prev = this.pointers.get(e.pointerId);
    if (!prev) return;
    const nx = e.global.x;
    const ny = e.global.y;

    if (this.pointers.size >= 2) {
      // Pinch zoom anchored at the midpoint.
      this.pointers.set(e.pointerId, { x: nx, y: ny });
      const [a, b] = [...this.pointers.values()];
      const dist = Math.hypot(a.x - b.x, a.y - b.y);
      if (this.pinchDist > 0 && dist > 0) {
        this.applyZoom(this.zoom * (dist / this.pinchDist), (a.x + b.x) / 2, (a.y + b.y) / 2);
      }
      this.pinchDist = dist;
      this.dragMoved += TAP_THRESHOLD + 1; // a pinch is never a tap
      return;
    }

    const dx = nx - prev.x;
    const dy = ny - prev.y;
    this.dragMoved += Math.abs(dx) + Math.abs(dy);
    this.pointers.set(e.pointerId, { x: nx, y: ny });
    this.panBy(dx, dy);
  };

  private readonly onPointerUp = (e: PIXI.FederatedPointerEvent): void => {
    const wasSingle = this.pointers.size === 1;
    this.pointers.delete(e.pointerId);
    if (this.pointers.size < 2) this.pinchDist = 0;
    if (wasSingle && this.dragMoved <= TAP_THRESHOLD) this.handleTap(e.global.x, e.global.y);
  };

  /** Hit-test the interactive targets (topmost first) and fire its route. */
  private handleTap(gx: number, gy: number): void {
    for (let i = this.clickTargets.length - 1; i >= 0; i--) {
      const t = this.clickTargets[i];
      if (t.node.destroyed) continue;
      const b = t.node.getBounds();
      if (gx >= b.minX && gx <= b.maxX && gy >= b.minY && gy <= b.maxY) {
        this.opts.onObject(t.object, t.agentId);
        return;
      }
    }
  }

  // ---- Render loop --------------------------------------------------------

  private readonly tick = (ticker: PIXI.Ticker): void => {
    const dt = Math.min(ticker.deltaMS / 1000, 0.05);
    const elapsed = ticker.lastTime / 1000;
    const a = smoothAlpha(0.18, dt);

    for (const node of this.agents.values()) {
      node.cur.x = lerp(node.cur.x, node.target.x, a);
      node.cur.y = lerp(node.cur.y, node.target.y, a);
      const c = tileCenterToScreen(node.cur.x, node.cur.y);
      const amp = node.action === 'sitting' ? 0.8 : node.action === 'walking' ? 2.4 : 1.4;
      node.bob.y = Math.sin(elapsed * 3 + node.bobPhase) * amp;
      node.container.position.set(c.x, c.y);
      node.container.zIndex = depthAt(node.cur.x, node.cur.y, 0, LAYER_AGENT);

      node.bubbleAlpha = lerp(node.bubbleAlpha, node.bubbleWant, smoothAlpha(0.3, dt));
      if (node.bubbleAlpha < 0.02 && node.bubbleWant === 0) {
        node.bubble.visible = false;
      } else {
        node.bubble.visible = true;
        node.bubble.alpha = node.bubbleAlpha;
      }
    }

    if (this.cars.length > 0) {
      const [min, max] = this.carBounds;
      for (const car of this.cars) {
        car.state.x = advanceCar(car.state.x, car.state.direction, car.state.speed, dt, min, max);
        this.positionCar(car.state, car.node);
      }
    }
  };

  // ---- Resize / teardown --------------------------------------------------

  private observeResize(parent: HTMLElement): void {
    this.resizeObserver = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      this.applySize(rect?.width ?? 600, rect?.height ?? 400);
    });
    this.resizeObserver.observe(parent);
  }

  private applySize(width: number, height: number): void {
    const app = this.app;
    if (!app || width <= 0 || height <= 0) return;
    app.renderer.resize(width, height);
    app.stage.hitArea = app.screen;
    this.recomputeFit(width, height);
    if (!this.userAdjusted) this.applyFit();
  }

  public destroy(): void {
    this.resizeObserver?.disconnect();
    this.resizeObserver = null;
    const app = this.app;
    if (app) {
      app.canvas.removeEventListener('wheel', this.onWheel);
      app.ticker.remove(this.tick);
      try {
        app.destroy(true, { children: true, texture: true });
      } catch {
        /* ignore */
      }
    }
    this.agents.clear();
    this.cars.length = 0;
    this.clickTargets.length = 0;
    this.pointers.clear();
    this.app = null;
    this.world = null;
    this.ground = null;
    this.actors = null;
    this.PX = null;
  }
}

// ---- Accessory mapping (inlined; the 3D character-3d module was removed) ----

type AccessoryModel = 'antenna' | 'glasses' | 'cap' | 'bow';
const MODELLED: ReadonlySet<CharacterAccessory> = new Set<CharacterAccessory>(['antenna', 'glasses', 'cap', 'bow']);
/** Map the full accessory vocabulary onto the drawn set (rest → antenna). */
function resolveAccessory(accessory: CharacterAccessory): AccessoryModel {
  return MODELLED.has(accessory) ? (accessory as AccessoryModel) : 'antenna';
}

/** Scale a packed 0xRRGGBB brightness (`factor` < 1 darkens, > 1 lightens). */
function shade(color: number, factor: number): number {
  const r = Math.min(255, Math.round(((color >> 16) & 0xff) * factor));
  const g = Math.min(255, Math.round(((color >> 8) & 0xff) * factor));
  const b = Math.min(255, Math.round((color & 0xff) * factor));
  return (r << 16) | (g << 8) | b;
}
