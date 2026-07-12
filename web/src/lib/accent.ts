/**
 * Accent-color white-label injection (design-distributor-white-label §10.4).
 *
 * A distributor picks a single `#rrggbb` brand color; we derive a small light/
 * dark ramp from it (OKLCH lightness shift, hue + chroma preserved) and inject a
 * `<style id="brand-accent">` that overrides the amber `--color-primary-400/500/
 * 600` and `--color-accent-400/500` tokens defined in `index.css`. No accent →
 * the style tag is removed and the UI keeps its default amber.
 *
 * The derivation is a pure function (`deriveAccentVars`) so it is unit-testable
 * without a DOM; `applyAccent` is the thin DOM side-effect that uses it.
 */

const HEX_RE = /^#[0-9a-fA-F]{6}$/;
const STYLE_ID = 'brand-accent';

/** Lightness offsets (OKLab L, 0–1) mirroring the amber ramp step feel in
 *  index.css: 400 is a step lighter than 500, 600 a step darker. */
const L_LIGHTER = 0.05;
const L_DARKER = 0.06;

// ── sRGB ↔ OKLab (Björn Ottosson) ────────────────────────────────────────────

function srgbToLinear(c: number): number {
  return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
}

function linearToSrgb(c: number): number {
  return c <= 0.0031308 ? 12.92 * c : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
}

function clamp01(x: number): number {
  return x < 0 ? 0 : x > 1 ? 1 : x;
}

interface OkLab {
  L: number;
  a: number;
  b: number;
}

function hexToOklab(hex: string): OkLab {
  const r = srgbToLinear(parseInt(hex.slice(1, 3), 16) / 255);
  const g = srgbToLinear(parseInt(hex.slice(3, 5), 16) / 255);
  const b = srgbToLinear(parseInt(hex.slice(5, 7), 16) / 255);

  const l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
  const m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
  const s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;

  const l_ = Math.cbrt(l);
  const m_ = Math.cbrt(m);
  const s_ = Math.cbrt(s);

  return {
    L: 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
    a: 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
    b: 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
  };
}

function oklabToHex({ L, a, b }: OkLab): string {
  const l_ = L + 0.3963377774 * a + 0.2158037573 * b;
  const m_ = L - 0.1055613458 * a - 0.0638541728 * b;
  const s_ = L - 0.0894841775 * a - 1.2914855480 * b;

  const l = l_ * l_ * l_;
  const m = m_ * m_ * m_;
  const s = s_ * s_ * s_;

  const r = linearToSrgb(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s);
  const g = linearToSrgb(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s);
  const bl = linearToSrgb(-0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s);

  const to255 = (c: number) =>
    Math.round(clamp01(c) * 255)
      .toString(16)
      .padStart(2, '0');
  return `#${to255(r)}${to255(g)}${to255(bl)}`;
}

/** Shift a color's OKLab lightness by `dL` (chroma + hue preserved). */
function shiftLightness(base: OkLab, dL: number): string {
  return oklabToHex({ L: clamp01(base.L + dL), a: base.a, b: base.b });
}

/**
 * Derive the CSS custom-property overrides for a brand accent hex, or `null`
 * when the input is not a valid `#rrggbb` string (invalid → no injection).
 */
export function deriveAccentVars(hex: string): Record<string, string> | null {
  if (!HEX_RE.test(hex)) return null;
  const base = hex.toLowerCase();
  const lab = hexToOklab(base);
  const lighter = shiftLightness(lab, L_LIGHTER);
  const darker = shiftLightness(lab, -L_DARKER);
  return {
    '--color-primary-400': lighter,
    '--color-primary-500': base,
    '--color-primary-600': darker,
    '--color-accent-400': lighter,
    '--color-accent-500': base,
  };
}

/**
 * Inject (or update / remove) the `<style id="brand-accent">` override for a
 * brand accent. `null`/invalid input removes any existing override so the UI
 * reverts to the default amber. Safe to call in a non-DOM environment.
 */
export function applyAccent(hex: string | null | undefined): void {
  if (typeof document === 'undefined') return;
  const existing = document.getElementById(STYLE_ID);
  const vars = hex ? deriveAccentVars(hex) : null;

  if (!vars) {
    existing?.remove();
    return;
  }

  const body = Object.entries(vars)
    .map(([k, v]) => `${k}:${v};`)
    .join('');
  const css = `:root{${body}}`;

  let el = existing as HTMLStyleElement | null;
  if (!el) {
    el = document.createElement('style');
    el.id = STYLE_ID;
    document.head.appendChild(el);
  }
  el.textContent = css;
}
