import type { ComponentType } from 'react';
import {
  Briefcase, Bot, Brain, Headset, Phone, Mail, Calculator, Code, Megaphone,
  Palette, Wrench, Book, BookOpen, Users, User, Building, Building2, Store,
  ShoppingCart, Truck, Scale, Stethoscope, Pill, Heart, HeartPulse, Shield,
  ShieldCheck, Globe, GitBranch, Utensils, Factory, Hammer, Sparkles, Star,
  Zap, Rocket, GraduationCap, Scissors, Wallet, Receipt, ChartLine, Clipboard,
  FileText, Camera, House, Car, Package, Gavel, FlaskConical, PenTool,
  Handshake, Landmark,
  type LucideProps,
} from 'lucide-react';

type IconType = ComponentType<LucideProps>;

/**
 * Agent icon glyph resolution.
 *
 * `agent.toml`'s `icon` field is a free string: some agents (and every industry
 * template) set a **lucide token** like `"briefcase"` / `"shield-check"`, while
 * users may set an **emoji** like `"🤖"`. Rendering the raw string as text
 * leaks internal tokens into the UI (bug #3 — a chat header showed the literal
 * word `briefcase`). This module is the single boundary that decides how to
 * render an icon string so a token never surfaces as text.
 *
 * Discipline: unknown tokens fall back to a default glyph rather than leaking —
 * fail-safe, never fail-leak.
 */

/** Curated lucide token → component map (kebab-case keys, matching lucide's own
 *  token names). Covers the tokens the shipped templates use plus the common
 *  business-role icons; unknown tokens resolve to `null` and fall back. */
const GLYPH_ICONS: Record<string, IconType> = {
  briefcase: Briefcase,
  bot: Bot,
  brain: Brain,
  headset: Headset,
  phone: Phone,
  mail: Mail,
  calculator: Calculator,
  code: Code,
  megaphone: Megaphone,
  palette: Palette,
  wrench: Wrench,
  book: Book,
  'book-open': BookOpen,
  users: Users,
  user: User,
  building: Building,
  'building-2': Building2,
  store: Store,
  'shopping-cart': ShoppingCart,
  truck: Truck,
  scale: Scale,
  stethoscope: Stethoscope,
  pill: Pill,
  heart: Heart,
  'heart-pulse': HeartPulse,
  shield: Shield,
  'shield-check': ShieldCheck,
  globe: Globe,
  'git-branch': GitBranch,
  utensils: Utensils,
  'fork-knife': Utensils, // lucide has no ForkKnife; Utensils is the alias
  factory: Factory,
  hammer: Hammer,
  sparkles: Sparkles,
  star: Star,
  zap: Zap,
  rocket: Rocket,
  'graduation-cap': GraduationCap,
  scissors: Scissors,
  wallet: Wallet,
  receipt: Receipt,
  'chart-line': ChartLine,
  'line-chart': ChartLine,
  clipboard: Clipboard,
  'file-text': FileText,
  camera: Camera,
  house: House,
  home: House,
  car: Car,
  package: Package,
  gavel: Gavel,
  'flask-conical': FlaskConical,
  'pen-tool': PenTool,
  handshake: Handshake,
  landmark: Landmark,
};

/**
 * Is this string safe to render directly as text? True only when it is a
 * non-empty glyph containing a non-ASCII codepoint (emoji / pictograph / CJK).
 * ASCII-only strings like `"briefcase"` are icon tokens or labels, never emoji.
 */
export function isDisplayableGlyph(icon: string | null | undefined): boolean {
  if (!icon) return false;
  const trimmed = icon.trim();
  if (!trimmed) return false;
  return /[^\x00-\x7f]/.test(trimmed);
}

/** Resolve a lucide component for an agent icon token, or `null` if the string
 *  is an emoji, empty, or an unrecognized token. */
export function glyphIconFor(icon: string | null | undefined): IconType | null {
  if (!icon) return null;
  return GLYPH_ICONS[icon.trim().toLowerCase()] ?? null;
}

/**
 * Text-only glyph for contexts that cannot render a component (e.g. a native
 * `<option>` or a d3/SVG text node). Emoji → itself; an icon token or empty
 * string → the fallback emoji (never the raw token).
 */
export function glyphText(icon: string | null | undefined, fallback = '🤖'): string {
  return isDisplayableGlyph(icon) ? icon!.trim() : fallback;
}

/**
 * Render an agent icon string. Emoji renders as text; a known lucide token
 * renders the icon; anything else falls back to `fallback` (a brand emoji).
 */
export function AgentGlyph({
  icon,
  className,
  iconClassName = 'size-5',
  fallback = '🐾',
}: {
  icon: string | null | undefined;
  /** Applied to the emoji/fallback `<span>`. */
  className?: string;
  /** Applied to the lucide icon when a token resolves. */
  iconClassName?: string;
  /** Shown when `icon` is empty or an unknown token. */
  fallback?: string;
}) {
  if (isDisplayableGlyph(icon)) {
    return <span className={className}>{icon!.trim()}</span>;
  }
  const Icon = glyphIconFor(icon);
  if (Icon) {
    return <Icon className={iconClassName} aria-hidden="true" />;
  }
  return <span className={className}>{fallback}</span>;
}
