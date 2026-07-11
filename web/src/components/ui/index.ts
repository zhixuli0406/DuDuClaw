/**
 * Calm Glass UI library — the shared primitives every dashboard page composes
 * from. See web/DESIGN.md for usage rules. Import from '@/components/ui'.
 */
export { Page } from './Page';
export { PageHeader } from './PageHeader';
export { Card } from './Card';
export { Section } from './Section';
export { StatCard } from './StatCard';
export { Tabs, type TabItem } from './Tabs';
export { Button } from './Button';
export { Badge } from './Badge';
export { EmptyState, type EmptyStateDudu } from './EmptyState';
export { Skeleton, SkeletonList } from './Skeleton';
export { Toolbar } from './Toolbar';
export { Field, controlClass } from './Field';
export { Mono } from './Mono';
export { EntityCard } from './EntityCard';
export { EntityDetailShell, type EntityTab } from './EntityDetailShell';

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
