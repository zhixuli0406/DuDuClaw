import type { AgentOutfit } from '@/lib/outfit';

/**
 * OutfitLayers — SVG renderings of the wardrobe (衣帽間) slot items, layered
 * onto `CharacterAvatar`. Geometry is parametric on the head circle so the
 * same item fits both the small round `avatar` face and the `bust`.
 *
 * `feet` items are deliberately NOT drawn here — the avatar/bust crops above
 * the waist; shoes appear on the full body in the PixiJS world scene.
 */

interface Head {
  cx: number;
  cy: number;
  r: number;
}

const INK = 'var(--character-ink)';
const SOFT = 'var(--character-ink-soft)';
const PAPER = 'var(--character-bubble)';

function Hat({ h, item, gradId }: { h: Head; item: string; gradId: string }) {
  const topY = h.cy - h.r;
  const grad = `url(#${gradId})`;
  switch (item) {
    case 'cap':
      return (
        <g>
          <path d={`M ${h.cx - h.r * 0.9} ${topY + h.r * 0.28} A ${h.r * 0.9} ${h.r * 0.9} 0 0 1 ${h.cx + h.r * 0.9} ${topY + h.r * 0.28} Z`} fill={grad} />
          <rect x={h.cx - h.r * 0.95} y={topY + h.r * 0.22} width={h.r * 1.9} height={h.r * 0.14} rx={h.r * 0.07} fill={grad} />
        </g>
      );
    case 'tophat':
      return (
        <g fill={INK}>
          <rect x={h.cx - h.r * 0.95} y={topY + h.r * 0.02} width={h.r * 1.9} height={h.r * 0.16} rx={h.r * 0.08} />
          <rect x={h.cx - h.r * 0.55} y={topY - h.r * 0.78} width={h.r * 1.1} height={h.r * 0.84} rx={h.r * 0.08} />
          <rect x={h.cx - h.r * 0.55} y={topY - h.r * 0.18} width={h.r * 1.1} height={h.r * 0.14} fill="var(--coin)" />
        </g>
      );
    case 'beret':
      return (
        <g>
          <ellipse cx={h.cx - h.r * 0.12} cy={topY + h.r * 0.1} rx={h.r * 0.8} ry={h.r * 0.34} fill="var(--agent-6a)" />
          <circle cx={h.cx - h.r * 0.12} cy={topY - h.r * 0.22} r={h.r * 0.09} fill="var(--agent-6a)" />
        </g>
      );
    case 'crown': {
      const b = topY + h.r * 0.18;
      const t = topY - h.r * 0.4;
      const w = h.r * 0.62;
      return (
        <g>
          <path
            d={`M ${h.cx - w} ${b} L ${h.cx - w} ${t} L ${h.cx - w * 0.45} ${b - h.r * 0.28} L ${h.cx} ${t - h.r * 0.1} L ${h.cx + w * 0.45} ${b - h.r * 0.28} L ${h.cx + w} ${t} L ${h.cx + w} ${b} Z`}
            fill="var(--coin)"
          />
          <circle cx={h.cx} cy={b - h.r * 0.12} r={h.r * 0.08} fill="var(--xp)" />
        </g>
      );
    }
    case 'beanie':
      return (
        <g>
          <path d={`M ${h.cx - h.r * 0.88} ${topY + h.r * 0.3} A ${h.r * 0.88} ${h.r * 0.88} 0 0 1 ${h.cx + h.r * 0.88} ${topY + h.r * 0.3} Z`} fill="var(--agent-4a)" />
          <rect x={h.cx - h.r * 0.9} y={topY + h.r * 0.22} width={h.r * 1.8} height={h.r * 0.18} rx={h.r * 0.09} fill="var(--agent-4b)" />
          <circle cx={h.cx} cy={topY - h.r * 0.52} r={h.r * 0.14} fill="var(--agent-4b)" />
        </g>
      );
    case 'helmet':
      return (
        <g>
          <path d={`M ${h.cx - h.r * 0.9} ${topY + h.r * 0.32} A ${h.r * 0.9} ${h.r * 0.9} 0 0 1 ${h.cx + h.r * 0.9} ${topY + h.r * 0.32} Z`} fill="var(--coin)" />
          <rect x={h.cx - h.r} y={topY + h.r * 0.26} width={h.r * 2} height={h.r * 0.12} rx={h.r * 0.06} fill="var(--coin)" />
          <rect x={h.cx - h.r * 0.14} y={topY - h.r * 0.28} width={h.r * 0.28} height={h.r * 0.5} rx={h.r * 0.1} fill="var(--coin)" />
        </g>
      );
    default:
      return null;
  }
}

function eyeGeom(h: Head) {
  return { dx: h.r * 0.42, y: h.cy - h.r * 0.02, rx: h.r * 0.14 };
}

function HeadItem({ h, item }: { h: Head; item: string }) {
  const e = eyeGeom(h);
  switch (item) {
    case 'glasses':
      return (
        <g fill="none" stroke={INK} strokeWidth={h.r * 0.06}>
          <circle cx={h.cx - e.dx} cy={e.y} r={e.rx * 1.9} />
          <circle cx={h.cx + e.dx} cy={e.y} r={e.rx * 1.9} />
          <line x1={h.cx - e.dx + e.rx * 1.9} y1={e.y} x2={h.cx + e.dx - e.rx * 1.9} y2={e.y} />
        </g>
      );
    case 'sunglasses':
      return (
        <g>
          <circle cx={h.cx - e.dx} cy={e.y} r={e.rx * 1.9} fill={INK} />
          <circle cx={h.cx + e.dx} cy={e.y} r={e.rx * 1.9} fill={INK} />
          <line x1={h.cx - e.dx} y1={e.y - e.rx} x2={h.cx + e.dx} y2={e.y - e.rx} stroke={INK} strokeWidth={h.r * 0.07} />
        </g>
      );
    case 'monocle':
      return (
        <g fill="none" stroke={INK} strokeWidth={h.r * 0.06}>
          <circle cx={h.cx + e.dx} cy={e.y} r={e.rx * 1.9} />
          <line x1={h.cx + e.dx + e.rx * 1.4} y1={e.y + e.rx * 1.4} x2={h.cx + e.dx + e.rx * 2.1} y2={h.cy + h.r * 0.8} />
        </g>
      );
    case 'mask':
      return (
        <g>
          <rect x={h.cx - h.r * 0.55} y={h.cy + h.r * 0.18} width={h.r * 1.1} height={h.r * 0.52} rx={h.r * 0.14} fill={PAPER} stroke={SOFT} strokeWidth={h.r * 0.03} />
          <line x1={h.cx - h.r * 0.55} y1={h.cy + h.r * 0.3} x2={h.cx - h.r * 0.95} y2={h.cy + h.r * 0.1} stroke={SOFT} strokeWidth={h.r * 0.04} />
          <line x1={h.cx + h.r * 0.55} y1={h.cy + h.r * 0.3} x2={h.cx + h.r * 0.95} y2={h.cy + h.r * 0.1} stroke={SOFT} strokeWidth={h.r * 0.04} />
        </g>
      );
    default:
      return null;
  }
}

function AccessoryItem({ h, item }: { h: Head; item: string }) {
  const topY = h.cy - h.r;
  switch (item) {
    case 'antenna':
      return (
        <g>
          <line x1={h.cx} y1={topY + h.r * 0.1} x2={h.cx} y2={topY - h.r * 0.5} stroke={INK} strokeWidth={h.r * 0.07} strokeLinecap="round" />
          <circle cx={h.cx} cy={topY - h.r * 0.55} r={h.r * 0.16} fill="var(--xp)" />
        </g>
      );
    case 'bow':
      return (
        <g transform={`translate(${h.cx + h.r * 0.5} ${topY + h.r * 0.12})`}>
          <path d={`M 0 0 L ${-h.r * 0.32} ${-h.r * 0.2} L ${-h.r * 0.32} ${h.r * 0.2} Z`} fill="var(--xp)" />
          <path d={`M 0 0 L ${h.r * 0.32} ${-h.r * 0.2} L ${h.r * 0.32} ${h.r * 0.2} Z`} fill="var(--xp)" />
          <circle cx={0} cy={0} r={h.r * 0.1} fill="var(--coin)" />
        </g>
      );
    case 'flower':
      return (
        <g transform={`translate(${h.cx + h.r * 0.55} ${topY + h.r * 0.15})`}>
          {[0, 72, 144, 216, 288].map((deg) => {
            const rad = (deg * Math.PI) / 180;
            return <circle key={deg} cx={Math.cos(rad) * h.r * 0.16} cy={Math.sin(rad) * h.r * 0.16} r={h.r * 0.11} fill="var(--agent-6a)" />;
          })}
          <circle cx={0} cy={0} r={h.r * 0.1} fill="var(--xp)" />
        </g>
      );
    case 'scarf': {
      const sy = h.cy + h.r * 0.82;
      return <rect x={h.cx - h.r * 0.72} y={sy} width={h.r * 1.44} height={h.r * 0.34} rx={h.r * 0.17} fill="var(--agent-2b)" />;
    }
    case 'halo':
      return <ellipse cx={h.cx} cy={topY - h.r * 0.42} rx={h.r * 0.6} ry={h.r * 0.18} fill="none" stroke="var(--coin)" strokeWidth={h.r * 0.09} />;
    case 'badge':
      // Chest pin in the bust; on the small round face it sits low-left.
      return (
        <g>
          <circle cx={h.cx - h.r * 0.72} cy={h.cy + h.r * 0.72} r={h.r * 0.16} fill="var(--coin)" />
          <circle cx={h.cx - h.r * 0.72} cy={h.cy + h.r * 0.72} r={h.r * 0.06} fill={PAPER} />
        </g>
      );
    default:
      return null;
  }
}

/** Head-anchored layers (hat / head item / accessory) — both variants. */
export function HeadOutfit({ h, fit, gradId }: { h: Head; fit: AgentOutfit; gradId: string }) {
  return (
    <g>
      <AccessoryItem h={h} item={fit.accessory} />
      <HeadItem h={h} item={fit.head} />
      <Hat h={h} item={fit.hat} gradId={gradId} />
    </g>
  );
}

/**
 * Bust-only layers: clothing on the shoulder mound + a held item near the
 * resting right hand. Coordinates match `BustBody` (mound `M6 48 Q6 31 24 31
 * Q42 31 42 48 Z`, idle right hand ≈ (38, 45)).
 */
export function BustOutfit({ fit }: { fit: AgentOutfit }) {
  return (
    <g>
      <BodyItem item={fit.body} />
      <HandsItem item={fit.hands} />
    </g>
  );
}

function BodyItem({ item }: { item: string }) {
  switch (item) {
    case 'tie':
      return (
        <g fill="var(--agent-2b)">
          <path d="M22.5 32 L25.5 32 L24 34.5 Z" />
          <path d="M24 34.5 L21.5 40 L24 45.5 L26.5 40 Z" />
        </g>
      );
    case 'bowtie':
      return (
        <g fill="var(--agent-6a)">
          <path d="M24 33.5 L19.5 31 L19.5 36 Z" />
          <path d="M24 33.5 L28.5 31 L28.5 36 Z" />
          <circle cx={24} cy={33.5} r={1.3} fill="var(--coin)" />
        </g>
      );
    case 'suit':
      return (
        <g>
          <path d="M17 33 L24 40 L31 33" fill="none" stroke={SOFT} strokeWidth={1.6} strokeLinecap="round" />
          <circle cx={24} cy={43} r={0.9} fill={SOFT} />
          <circle cx={24} cy={46} r={0.9} fill={SOFT} />
        </g>
      );
    case 'apron':
      return (
        <g>
          <rect x={17.5} y={36} width={13} height={12} rx={2} fill={PAPER} stroke={SOFT} strokeWidth={0.5} />
          <path d="M19 36 L22 31.5 M29 36 L26 31.5" stroke={SOFT} strokeWidth={1} fill="none" />
          <rect x={20.5} y={40} width={7} height={4} rx={1} fill="none" stroke={SOFT} strokeWidth={0.7} />
        </g>
      );
    case 'sash':
      return <path d="M12 34 L36 48 L32 48 L10 37 Z" fill="var(--xp)" opacity={0.9} />;
    default:
      return null;
  }
}

function HandsItem({ item }: { item: string }) {
  // Anchored near the idle right hand (38, 45).
  switch (item) {
    case 'coffee':
      return (
        <g>
          <rect x={35.5} y={40.5} width={5} height={5.5} rx={1} fill={PAPER} stroke={SOFT} strokeWidth={0.6} />
          <path d="M40.5 41.8 Q43 42.8 40.5 44.5" fill="none" stroke={SOFT} strokeWidth={0.8} />
          <path d="M37 39.5 Q37.6 38.4 37 37.4 M39 39.5 Q39.6 38.4 39 37.4" fill="none" stroke={SOFT} strokeWidth={0.6} opacity={0.7} />
        </g>
      );
    case 'briefcase':
      return (
        <g>
          <rect x={33.5} y={41} width={9} height={6.5} rx={1.2} fill="var(--coin)" />
          <path d="M36.5 41 v-1.4 a1.5 1.5 0 0 1 3 0 V41" fill="none" stroke={INK} strokeWidth={0.8} />
          <line x1={33.5} y1={44} x2={42.5} y2={44} stroke={INK} strokeWidth={0.5} opacity={0.5} />
        </g>
      );
    case 'wrench':
      return (
        <g stroke={SOFT} strokeWidth={1.8} strokeLinecap="round">
          <line x1={35} y1={47} x2={41} y2={40} />
          <circle cx={41.5} cy={39.5} r={2} fill="none" strokeWidth={1.4} />
        </g>
      );
    case 'clipboard':
      return (
        <g>
          <rect x={34} y={38.5} width={7.5} height={9.5} rx={1} fill={PAPER} stroke={SOFT} strokeWidth={0.6} />
          <rect x={36.2} y={37.6} width={3} height={1.8} rx={0.6} fill={SOFT} />
          <line x1={35.5} y1={41.5} x2={40} y2={41.5} stroke={SOFT} strokeWidth={0.6} />
          <line x1={35.5} y1={43.5} x2={40} y2={43.5} stroke={SOFT} strokeWidth={0.6} />
          <line x1={35.5} y1={45.5} x2={38.5} y2={45.5} stroke={SOFT} strokeWidth={0.6} />
        </g>
      );
    case 'phone':
      return (
        <g>
          <rect x={36} y={39.5} width={4.6} height={8} rx={1} fill={INK} />
          <rect x={36.7} y={40.6} width={3.2} height={5.2} rx={0.4} fill="var(--agent-8a)" />
        </g>
      );
    default:
      return null;
  }
}
