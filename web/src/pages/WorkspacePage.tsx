import { useEffect, useRef } from 'react';
import { useIntl } from 'react-intl';
import { RotateCcw } from 'lucide-react';
import { useChatStore } from '@/stores/chat-store';
import { Button } from '@/components/ui';
import { MessageBubble, TypingIndicator } from '@/components/chat';
import { PromptBar } from '@/components/workspace/PromptBar';
import { LauncherGrid } from '@/components/workspace/LauncherGrid';
import { ClawHero } from '@/components/workspace/ClawHero';

/**
 * The workspace landing — DuDuClaw's Genspark-style consumer shell
 * (TODO-genspark-workspace-shell §P1). Two states on one page:
 *  - `idle`       → hero + prompt bar + Claw value props + launcher grid.
 *  - `conversing` → the reply stream + prompt bar pinned at the bottom.
 * Both reuse the existing `/ws/chat` pipeline via `useChatStore`.
 */
export function WorkspacePage() {
  const intl = useIntl();
  const { messages, isStreaming, connectionState, connect, reset } = useChatStore();
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const conversing = messages.length > 0;

  // Connect on mount; keep the session alive on unmount (same as WebChat).
  useEffect(() => {
    if (connectionState === 'disconnected') connect();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (conversing) messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, isStreaming, conversing]);

  const focusPrompt = () => {
    document.getElementById('workspace-prompt')?.focus();
  };

  if (conversing) {
    return (
      <div className="mx-auto flex h-full max-w-3xl flex-col">
        <div className="flex items-center justify-end py-2">
          <Button
            variant="ghost"
            size="sm"
            icon={RotateCcw}
            onClick={reset}
            title={intl.formatMessage({ id: 'webchat.reset' })}
          >
            {intl.formatMessage({ id: 'workspace.newConversation', defaultMessage: '新對話' })}
          </Button>
        </div>
        <div className="flex-1 space-y-3 overflow-y-auto py-2">
          {messages.map((msg) => (
            <MessageBubble key={msg.id} message={msg} />
          ))}
          {isStreaming && <TypingIndicator />}
          <div ref={messagesEndRef} />
        </div>
        <div className="py-3">
          <PromptBar />
        </div>
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-[860px] space-y-10 py-6">
      <header className="space-y-2 text-center">
        <div className="flex items-center justify-center gap-2">
          <span
            className="grid h-9 w-9 place-items-center rounded-xl bg-gradient-to-b from-amber-400 to-amber-500 text-lg shadow-[0_4px_16px_-4px_rgba(245,158,11,0.6)]"
            role="img"
            aria-label="paw"
          >
            🐾
          </span>
          <h1 className="text-2xl font-semibold tracking-tight text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'workspace.title', defaultMessage: 'DuDuClaw 工作空間' })}
          </h1>
        </div>
        <p className="text-sm text-stone-500 dark:text-stone-400">
          {intl.formatMessage({
            id: 'workspace.subtitle',
            defaultMessage: '一句話,交辦給您的 AI 員工。',
          })}
        </p>
      </header>

      <PromptBar onSent={focusPrompt} />

      <ClawHero onStart={focusPrompt} />

      <LauncherGrid />
    </div>
  );
}
