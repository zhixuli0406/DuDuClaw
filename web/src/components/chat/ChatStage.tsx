import { useIntl } from 'react-intl';
import type { VisemeShape } from '@/components/mascot';
import { DuDu, type DuduFace } from '@/components/mascot';
import { CharacterAvatar, agentPose, type AgentLifecycle } from '@/components/character';
import type { ChatPhase } from '@/stores/chat-store';

/** True while the assistant is actively working this turn (drives the bust pose). */
function isBusy(phase: ChatPhase): boolean {
  return phase === 'thinking' || phase === 'speaking';
}

/**
 * CenterStage — the character-forward centre of gravity shown while the
 * conversation is empty (V7 / T7.1). Renders DuDu large by default; once an AI
 * staff member is picked (T7.2) the centre becomes that employee's bust and DuDu
 * retreats to the corner. A short caption reflects the live phase.
 */
export function CenterStage({
  face,
  viseme,
  agentId,
  agentName,
  agentStatus,
  phase,
}: {
  face: DuduFace;
  viseme: VisemeShape;
  agentId: string | null;
  agentName: string;
  agentStatus: AgentLifecycle;
  phase: ChatPhase;
}) {
  const intl = useIntl();

  const caption =
    phase === 'speaking'
      ? intl.formatMessage({ id: 'chat.stage.speaking', defaultMessage: 'Replying…' })
      : phase === 'thinking'
        ? intl.formatMessage({ id: 'chat.stage.thinking', defaultMessage: 'Thinking…' })
        : intl.formatMessage({ id: 'chat.stage.ready', defaultMessage: 'Ready' });

  return (
    <div className="flex flex-col items-center justify-center gap-4 py-16 text-center">
      {agentId ? (
        <CharacterAvatar
          agentId={agentId}
          name={agentName}
          size={148}
          variant="bust"
          pose={agentPose(agentStatus, isBusy(phase))}
          live={isBusy(phase)}
        />
      ) : (
        <DuDu face={face} viseme={viseme} size="lg" label={`DuDu, ${face}`} />
      )}

      <div>
        <h3 className="text-lg font-semibold tracking-tight text-stone-800 dark:text-stone-100">
          {agentName}
        </h3>
        <p className="mt-0.5 text-sm text-stone-500 dark:text-stone-400">{caption}</p>
      </div>
    </div>
  );
}

/**
 * CornerDuDu — the persistent small companion pinned to the conversation's top
 * corner once there's history (or when an employee holds the centre). Same face
 * logic as the centre stage, just `sm`.
 */
export function CornerDuDu({ face, viseme }: { face: DuduFace; viseme: VisemeShape }) {
  return (
    <div className="pointer-events-none absolute right-3 top-3 z-10">
      <DuDu face={face} viseme={viseme} size="sm" label={`DuDu, ${face}`} />
    </div>
  );
}
