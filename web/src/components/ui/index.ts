/**
 * Functional UI primitives — the shared behavioural components dashboard pages
 * compose from (status/priority icons, inline editing, celebration, character
 * system, mascot). The former Calm Glass visual primitives (Button/Card/Section/
 * Badge/EmptyState/Tabs/Field) were retired in favour of '@/components/mds'.
 * Import from '@/components/ui'.
 */

// ── Soft Play v2 primitives (dashboard-redesign-v2 §3.3 / T0.3) ──
export {
  PropertiesPanel,
  PanelProvider,
  usePanel,
} from './PropertiesPanel';
export { PropertyRow, PropertySection } from './PropertyRow';
export { InlineEditor } from './InlineEditor';
export {
  StatusIcon,
  useStatusLabel,
  TASK_STATUS_ORDER,
  type TaskStatusKey,
} from './StatusIcon';
export { PriorityIcon, type TaskPriorityKey } from './PriorityIcon';
export { LiveBadge } from './LiveBadge';
export { SpeechBubble } from './SpeechBubble';
export { CoinChip } from './CoinChip';
export { XpBar, levelFromXp, xpForLevel, levelProgress } from './XpBar';
export { AchievementBadge } from './AchievementBadge';
export {
  CelebrationLayer,
  celebrate,
  type CelebrationKind,
  type CelebrationOptions,
} from './CelebrationLayer';
export { SwipeToArchive } from './SwipeToArchive';
export { GroupHeader } from './GroupHeader';

// ── Character system (dashboard-redesign-v2 §3.2 / V2) — re-exported here so
// pages can pull the AI-staff visual identity from the same '@/components/ui'
// barrel as the rest of the primitives. Source lives in '@/components/character'.
export {
  CharacterAvatar,
  type CharacterAvatarProps,
  StatusEmote,
  type StatusEmoteKind,
  agentPose,
  agentEmote,
  type CharacterPose,
} from '@/components/character';

// ── DuDu mascot (§7 / V9) — re-exported so pages can pull the single companion
// character from the same '@/components/ui' barrel. Source lives in
// '@/components/mascot'.
export { DuDu, type DuDuProps, type DuduSize } from '@/components/mascot';
export { type DuduFace } from '@/components/mascot/faces';
