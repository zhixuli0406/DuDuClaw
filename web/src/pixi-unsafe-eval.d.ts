/**
 * Ambient module declaration for `pixi.js/unsafe-eval`.
 *
 * The dashboard ships under a strict CSP: `script-src 'self'` with NO
 * `unsafe-eval`. PixiJS's `WebGLRenderer` generates its uniform-upload functions
 * with `new Function(...)` at runtime, which that CSP blocks — the renderer
 * throws on first draw. The official fix is the `pixi.js/unsafe-eval` polyfill,
 * which swaps the `Function`-generated sync paths for interpreted equivalents.
 * The world renderer imports it eagerly alongside `pixi.js`:
 *
 *     await Promise.all([import('pixi.js'), import('pixi.js/unsafe-eval')]);
 *
 * The published subpath has no `types` export condition, so TypeScript can't
 * resolve declarations for it under `moduleResolution: bundler`. This ambient
 * declaration is the shim that keeps the dynamic import type-checking cleanly —
 * the module is imported purely for its install side-effect (no named exports
 * we consume), so an empty module body is correct.
 */
declare module 'pixi.js/unsafe-eval';
