import { useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { useChatStore } from '@/stores/chat-store';
import { cn } from '@/lib/utils';
import { isImeComposing } from '@/lib/keyboard';
import { LayoutGrid } from 'lucide-react';
import { Button, SubmitButton } from '@/components/mds';
import { MessageBubble, TypingIndicator, TaskInsights, useChatFace } from '@/components/chat';
import { DuDu } from '@/components/mascot';
import { ChatArtifactCard } from '@/components/console';

/**
 * Operator console (O-2 scaffold, `commercial/docs/DESIGN-agent-os-native-
 * apps-2026-08.md` §6.2 UI/UX + §6.3 O-2). "對話先行": on the appliance image
 * this becomes the default landing surface (wired in `App.tsx`'s
 * `HomeLanding`) instead of the app grid — the user opens the box and talks
 * to it, rather than hunting through settings pages.
 *
 * This pass is deliberately a SCAFFOLD, not the finished NL-OS experience:
 *  - The conversation partner is whatever the shared `useChatStore` session
 *    already points at (the default/main agent for a fresh session — see
 *    `chat-store.ts`'s `selectedAgentId: null` initial state), NOT a
 *    dedicated "system operator" persona. O-4 introduces that persona plus
 *    its capability tools; until then this page intentionally does not force
 *    a `selectAgent()` switch on mount, because `/chat` and `/console` share
 *    ONE websocket session (there is no per-page session dimension yet) —
 *    forcing a switch here would silently yank whatever staff member the
 *    user was mid-conversation with on `/chat` back to the default agent.
 *    That coupling is a known scaffold limitation, not a design decision.
 *  - There is no capability tool bridge yet (O-0) and no intent router
 *    (O-1), so the agent answers with whatever it can already do today —
 *    it cannot yet actually check device status, apply updates, or manage
 *    channels through this chat. The hero suggestions below are chosen to
 *    stay within what a default agent can attempt today (status/task talk),
 *    not device/system operations that would just fail right now.
 *  - O-3 wired the RENDER side of inline structured result/confirmation
 *    cards: any `ChatMessage` carrying an `artifact` (device status, update
 *    checks, destructive confirmations, ApprovalBroker cards — see
 *    `@/components/console/artifact-types.ts`) renders as a `ChatArtifactCard`
 *    below its bubble. O-4 now populates `artifact` over the wire for one
 *    case — a pending restart/shutdown/factory-reset confirmation (see
 *    `stores/chat-store.ts`'s `parseChatArtifact` and the gateway's
 *    `os_operator::marker_to_artifact`); the other card types still have no
 *    production producer and remain proven only against
 *    `@/components/console/fixtures.ts` in tests.
 *
 * The "進階" button is the §6.5 O-5 affordance: every native app/settings
 * surface stays exactly where it was (LauncherPage's app grid, the full
 * sidebar, `/manage/*`) — this page only changes which door most people walk
 * through first, never what's behind the other doors.
 */
export function OperatorConsolePage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const {
    messages,
    steps,
    stepTree,
    isStreaming,
    phase,
    connectionState,
    connect,
    send,
  } = useChatStore();

  const [input, setInput] = useState('');
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  // Connect on mount, same as WebChatPage — the appliance may boot straight
  // into this page without ever visiting /chat first.
  useEffect(() => {
    if (connectionState === 'disconnected') connect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, isStreaming, stepTree.length]);

  // Auto-grow the composer textarea up to a cap (mirrors WebChatPage).
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, [input]);

  const face = useChatFace(phase, input.trim().length > 0, messages.length > 0);
  const empty = messages.length === 0;
  const insightsVisible = stepTree.length > 0 || steps.length > 0;

  const last = messages[messages.length - 1];
  const streamingAssistant = isStreaming && last?.role === 'assistant' && last.tokens == null ? last : null;
  const priorMessages = streamingAssistant ? messages.slice(0, -1) : messages;

  const leadingAvatar = <DuDu face="idle" size={26} animated={false} label="DuDu" />;

  const handleSend = () => {
    const text = input.trim();
    if (!text || isStreaming) return;
    send(text, []);
    setInput('');
    inputRef.current?.focus();
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey && !isImeComposing(e)) {
      e.preventDefault();
      handleSend();
    }
  };

  const suggestions = useMemo(
    () => [
      intl.formatMessage({ id: 'console.hero.suggest.1' }),
      intl.formatMessage({ id: 'console.hero.suggest.2' }),
      intl.formatMessage({ id: 'console.hero.suggest.3' }),
    ],
    [intl],
  );

  const canSend = input.trim().length > 0 && !isStreaming && connectionState === 'connected';

  const statusTone =
    connectionState === 'connected' ? 'bg-success' : connectionState === 'connecting' ? 'bg-warning' : 'bg-muted-foreground';
  const statusText =
    connectionState === 'connected'
      ? intl.formatMessage({ id: 'webchat.connected' })
      : connectionState === 'connecting'
        ? intl.formatMessage({ id: 'webchat.connecting' })
        : intl.formatMessage({ id: 'webchat.disconnected' });

  return (
    <div className="-mx-4 -mt-4 flex min-h-0 flex-1 flex-col md:-mx-6 md:-mt-6 md:-mb-6">
      {/* Header — identity + status + the one 「進階」 escape hatch to the app
          grid (§6.5 O-5). No partner-switcher row here (unlike WebChatPage):
          the console speaks with one voice, not a staff roster. */}
      <div className="flex h-12 shrink-0 items-center gap-2 border-b border-surface-border px-4">
        <DuDu face="idle" size={24} animated={false} label="DuDu" />
        <span className="truncate text-sm font-medium text-foreground">
          {intl.formatMessage({ id: 'nav.console' })}
        </span>
        <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <span className={cn('size-1.5 rounded-full', statusTone)} aria-hidden="true" />
          <span className="hidden sm:inline">{statusText}</span>
        </span>
        <div className="ml-auto">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => navigate('/launcher')}
            aria-label={intl.formatMessage({ id: 'console.advancedAria' })}
            title={intl.formatMessage({ id: 'console.advancedAria' })}
          >
            <LayoutGrid className="size-4" />
            {intl.formatMessage({ id: 'navGroup.advanced' })}
          </Button>
        </div>
      </div>

      {/* Conversation — full-bleed, hero-led when empty (對話先行). */}
      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-6">
        <div className="mx-auto max-w-3xl space-y-3">
          {empty ? (
            <div className="flex flex-col items-center justify-center gap-4 py-16 text-center">
              <DuDu face={face} size={96} label="DuDu" />
              <div>
                <h1 className="text-xl font-semibold text-foreground">
                  {intl.formatMessage({ id: 'console.hero.title' })}
                </h1>
                <p className="mt-1.5 text-sm text-muted-foreground">
                  {intl.formatMessage({ id: 'console.hero.subtitle' })}
                </p>
              </div>
              <div className="flex flex-wrap justify-center gap-2">
                {suggestions.map((s) => (
                  <button
                    key={s}
                    type="button"
                    onClick={() => {
                      setInput(s);
                      inputRef.current?.focus();
                    }}
                    className="rounded-4xl border border-border px-3 py-1 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                  >
                    {s}
                  </button>
                ))}
              </div>
            </div>
          ) : (
            <>
              {priorMessages.map((msg) => (
                <div key={msg.id} className="space-y-2">
                  <MessageBubble message={msg} leading={msg.role === 'assistant' ? leadingAvatar : undefined} />
                  {msg.artifact && <ChatArtifactCard artifact={msg.artifact} />}
                </div>
              ))}

              {insightsVisible && <TaskInsights tree={stepTree} todos={steps} streaming={isStreaming} />}

              {streamingAssistant && <MessageBubble message={streamingAssistant} leading={leadingAvatar} />}

              {isStreaming && !streamingAssistant && <TypingIndicator />}
            </>
          )}

          <div ref={messagesEndRef} />
        </div>
      </div>

      {/* Composer — the primary input on this page, so it stays large and
          unambiguous rather than sharing space with attachments/voice chrome
          (those stay on /chat; this scaffold is text-only per O-2 scope). */}
      <div className="shrink-0 border-t border-surface-border px-4 py-4">
        <div className="mx-auto flex max-w-3xl items-end gap-1.5">
          <textarea
            ref={inputRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder={intl.formatMessage({ id: 'console.placeholder' })}
            rows={1}
            className={cn(
              'min-h-10 flex-1 resize-none rounded-lg border border-input bg-transparent px-3 py-2 text-sm',
              'placeholder:text-muted-foreground focus-visible:border-ring focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/50',
              'dark:bg-input/30',
            )}
            disabled={connectionState !== 'connected'}
          />
          <SubmitButton
            state={isStreaming ? 'submitting' : 'idle'}
            onClick={handleSend}
            disabled={!canSend}
            title={intl.formatMessage({ id: 'webchat.hint' })}
          />
        </div>
      </div>
    </div>
  );
}
