import { REST_SMILE_PATH } from './visemes';

/**
 * The discrete face presets DuDu can wear (§7.1). The vocabulary mirrors the
 * agent + reply lifecycle so the renderer stays presentation-only:
 *
 * - `sleep`      — eyes closed, at rest (idle for long / desktop-pet default).
 * - `idle`       — calm, open-eyed rest.
 * - `listening`  — attentive, wide eyes (user is typing / dictating).
 * - `thinking`   — first inference in flight; paw to chin.
 * - `speaking`   — streaming a reply; the mouth is driven by `viseme`, not the
 *                  face preset.
 * - `happy`      — short post-turn acknowledgement before falling back to idle.
 * - `concerned`  — error / disconnect / failed path.
 * - `curious`    — interested; something needs a look (pending approval).
 * - `proud`      — a meaningful task finished; both paws up.
 * - `celebrating`— success burst (inbox zero, achievement).
 * - `writing`    — agent is editing / creating (paws typing).
 * - `reading`    — agent is browsing / reading (paws holding a book).
 * - `waving`     — greeting; one paw raised.
 *
 * Kept free of React so the mapping from mood → face stays a pure import for
 * `lib/mascot-mood.ts` and the store.
 */
export type DuduFace =
  | 'sleep'
  | 'idle'
  | 'listening'
  | 'thinking'
  | 'speaking'
  | 'happy'
  | 'concerned'
  | 'curious'
  | 'proud'
  | 'celebrating'
  | 'writing'
  | 'reading'
  | 'waving';

/** The 13 canonical presets, exported so tests / callers can iterate them. */
export const DUDU_FACES: readonly DuduFace[] = [
  'sleep',
  'idle',
  'listening',
  'thinking',
  'speaking',
  'happy',
  'concerned',
  'curious',
  'proud',
  'celebrating',
  'writing',
  'reading',
  'waving',
];

/** Whole-body arm attitude a preset asks for. */
export type ArmPose = 'rest' | 'wave' | 'cheer' | 'think' | 'work' | 'read';

export interface FacePreset {
  /** Vertical squash of the eyes (1 = round, < 1 = squinted). */
  eyeScaleY: number;
  /** Horizontal scale of the eyes. */
  eyeScaleX: number;
  /** Eyebrow tilt in degrees — positive points the inner brow up (worried). */
  browTilt: number;
  /** Vertical brow offset — negative is higher (raised). */
  browDy: number;
  /** Whether to render eyebrows at all. */
  showBrows: boolean;
  /** Blush intensity multiplier. */
  blushOpacity: number;
  /** Arm attitude for the preset. */
  arm: ArmPose;
}

export const FACE_PRESETS: Record<DuduFace, FacePreset> = {
  sleep: { eyeScaleY: 0.1, eyeScaleX: 1, browTilt: 0, browDy: 2, showBrows: false, blushOpacity: 0.5, arm: 'rest' },
  idle: { eyeScaleY: 1, eyeScaleX: 1, browTilt: 0, browDy: 0, showBrows: false, blushOpacity: 0.85, arm: 'rest' },
  listening: { eyeScaleY: 1.08, eyeScaleX: 1.05, browTilt: -8, browDy: -3, showBrows: true, blushOpacity: 0.9, arm: 'rest' },
  thinking: { eyeScaleY: 0.7, eyeScaleX: 1, browTilt: -4, browDy: -1, showBrows: true, blushOpacity: 0.6, arm: 'think' },
  speaking: { eyeScaleY: 1, eyeScaleX: 1, browTilt: 0, browDy: 0, showBrows: false, blushOpacity: 0.95, arm: 'rest' },
  happy: { eyeScaleY: 0.45, eyeScaleX: 1.1, browTilt: -6, browDy: -2, showBrows: false, blushOpacity: 1, arm: 'rest' },
  concerned: { eyeScaleY: 0.95, eyeScaleX: 0.95, browTilt: 22, browDy: -1, showBrows: true, blushOpacity: 0.5, arm: 'rest' },
  curious: { eyeScaleY: 1.12, eyeScaleX: 1.05, browTilt: -10, browDy: -3, showBrows: true, blushOpacity: 0.8, arm: 'think' },
  proud: { eyeScaleY: 0.55, eyeScaleX: 1.15, browTilt: -4, browDy: -2, showBrows: false, blushOpacity: 1, arm: 'cheer' },
  celebrating: { eyeScaleY: 0.4, eyeScaleX: 1.15, browTilt: -8, browDy: -3, showBrows: false, blushOpacity: 1, arm: 'cheer' },
  writing: { eyeScaleY: 0.75, eyeScaleX: 1, browTilt: -2, browDy: 0, showBrows: false, blushOpacity: 0.7, arm: 'work' },
  reading: { eyeScaleY: 0.85, eyeScaleX: 1.05, browTilt: -6, browDy: -1, showBrows: true, blushOpacity: 0.75, arm: 'read' },
  waving: { eyeScaleY: 0.5, eyeScaleX: 1.1, browTilt: -6, browDy: -2, showBrows: false, blushOpacity: 1, arm: 'wave' },
};

export function presetFor(face: DuduFace): FacePreset {
  return FACE_PRESETS[face];
}

/**
 * Closed-mouth shape for non-speaking states (mouth centred near y≈59 in the
 * 100×100 viewBox). `speaking` is handled separately via `visemePath`.
 */
export function restMouthPath(face: DuduFace): string {
  switch (face) {
    case 'sleep':
      return 'M47,60 Q50,61.5 53,60 Q50,61 47,60 Z';
    case 'happy':
    case 'celebrating':
    case 'waving':
      return 'M43,56 Q50,64 57,56 Q50,60.5 43,56 Z';
    case 'proud':
      return 'M44,56.5 Q50,63 56,56.5 Q50,60 44,56.5 Z';
    case 'concerned':
      return 'M45,61 Q50,55.5 55,61 Q50,59 45,61 Z';
    case 'thinking':
    case 'writing':
      return 'M46,59 Q50,60.5 54,59 Q50,60 46,59 Z';
    case 'listening':
    case 'curious':
      return 'M47,58.5 Q50,61.5 53,58.5 Q50,62.5 47,58.5 Z';
    case 'reading':
      return 'M46,59 Q50,60.5 54,59 Q50,60 46,59 Z';
    default:
      return REST_SMILE_PATH;
  }
}
