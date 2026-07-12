/**
 * Shared chat presentation components, reused by the full WebChat page and the
 * workspace conversation view (TODO-genspark-workspace-shell §P1.3).
 */
export { AttachmentChip } from './AttachmentChip';
export { MessageBubble } from './MessageBubble';
export { TypingIndicator } from './TypingIndicator';
export { TaskInsights } from './TaskInsights';
export { CenterStage, CornerDuDu } from './ChatStage';
export { EmployeeRow, type EmployeeRowAgent } from './EmployeeRow';
export { SessionHistoryMenu } from './SessionHistoryMenu';
export { useChatFace } from './useChatFace';
export { sampleViseme, REST_VISEME } from './viseme-sampler';
export { MicButton } from './MicButton';
export { VoicePlayToggle } from './VoicePlayToggle';
export { ttsSynthesizeUrl, VoiceNotConfiguredError } from './voice-api';
export { TalkModeButton, TalkModeStatusPill } from './TalkModeButton';
export { useTalkMode, type TalkModeHandle } from './useTalkMode';
