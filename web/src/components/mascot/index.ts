/**
 * DuDu mascot barrel (§7). The DuDuClaw paw-creature companion — one SVG
 * character, 13 face presets, viseme-driven speech. Import from
 * '@/components/mascot'.
 */
export { DuDu, type DuDuProps, type DuduSize } from './DuDu';
export {
  type DuduFace,
  type ArmPose,
  type FacePreset,
  DUDU_FACES,
  FACE_PRESETS,
  presetFor,
  restMouthPath,
} from './faces';
export {
  VISEMES,
  REST_SMILE_PATH,
  visemePath,
  lerpViseme,
  type VisemeId,
  type VisemeShape,
} from './visemes';
export { useDuduClock } from './useDuduClock';
