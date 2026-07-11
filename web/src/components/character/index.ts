/**
 * Character system barrel (§3.2). The one visual identity for every AI staff
 * member. Import from '@/components/character'.
 */
export { CharacterAvatar, type CharacterAvatarProps } from './CharacterAvatar';
export { StatusEmote, type StatusEmoteKind } from './StatusEmote';
export {
  agentPose,
  agentEmote,
  type CharacterPose,
  type AgentLifecycle,
} from './poses';
export {
  characterFor,
  type CharacterTraits,
  type CharacterAccessory,
} from '@/lib/character-gen';
