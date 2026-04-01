import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useChatStore, type ChatMessage } from '@/stores/chat-store';
import { cn } from '@/lib/utils';
import { Send, RotateCcw, Loader2 } from 'lucide-react';

function MessageBubble({ message }: { message: ChatMessage }) {
  const isUser = message.role === 'user';
  const isSystem = message.role === 'system';

  return (
    <div
      className={cn(
        'flex w-full',
        isUser ? 'justify-end' : 'justify-start'
      )}
    >
      <div
        className={cn(
          'max-w-[80%] rounded-2xl px-4 py-2.5 text-sm leading-relaxed',
          isUser
            ? 'bg-amber-500 text-white'
            : isSystem
              ? 'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400'
              : 'bg-stone-100 text-stone-800 dark:bg-stone-800 dark:text-stone-200'
        )}
      >
        <div className="whitespace-pre-wrap break-words">{message.content}</div>
        {message.tokens != null && message.tokens > 0 && (
          <div className="mt-1 text-xs opacity-50">{message.tokens} tokens</div>
        )}
      </div>
    </div>
  );
}

function TypingIndicator() {
  return (
    <div className="flex justify-start">
      <div className="flex items-center gap-1 rounded-2xl bg-stone-100 px-4 py-3 dark:bg-stone-800">
        <span className="h-2 w-2 animate-bounce rounded-full bg-stone-400 [animation-delay:0ms]" />
        <span className="h-2 w-2 animate-bounce rounded-full bg-stone-400 [animation-delay:150ms]" />
        <span className="h-2 w-2 animate-bounce rounded-full bg-stone-400 [animation-delay:300ms]" />
      </div>
    </div>
  );
}

export function WebChatPage() {
  const intl = useIntl();
  const {
    messages,
    isStreaming,
    agentName,
    agentIcon,
    connectionState,
    connect,
    send,
    reset,
  } = useChatStore();

  const [input, setInput] = useState('');
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  // Connect on mount
  useEffect(() => {
    if (connectionState === 'disconnected') {
      connect();
    }
    return () => {
      // Don't disconnect on unmount — keep session alive
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // Auto-scroll to bottom
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, isStreaming]);

  const handleSend = () => {
    const text = input.trim();
    if (!text || isStreaming) return;
    send(text);
    setInput('');
    inputRef.current?.focus();
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-stone-200 px-6 py-4 dark:border-stone-800">
        <div className="flex items-center gap-3">
          <span className="text-2xl">{agentIcon}</span>
          <div>
            <h2 className="text-lg font-semibold text-stone-900 dark:text-stone-50">
              {agentName}
            </h2>
            <div className="flex items-center gap-1.5">
              <span
                className={cn(
                  'h-2 w-2 rounded-full',
                  connectionState === 'connected'
                    ? 'bg-emerald-500'
                    : connectionState === 'connecting'
                      ? 'bg-amber-500'
                      : 'bg-stone-400'
                )}
              />
              <span className="text-xs text-stone-500 dark:text-stone-400">
                {connectionState === 'connected'
                  ? intl.formatMessage({ id: 'webchat.connected', defaultMessage: 'Connected' })
                  : connectionState === 'connecting'
                    ? intl.formatMessage({ id: 'webchat.connecting', defaultMessage: 'Connecting...' })
                    : intl.formatMessage({ id: 'webchat.disconnected', defaultMessage: 'Disconnected' })}
              </span>
            </div>
          </div>
        </div>

        <button
          onClick={reset}
          className="rounded-lg p-2 text-stone-500 transition-colors hover:bg-stone-100 hover:text-stone-700 dark:hover:bg-stone-800 dark:hover:text-stone-300"
          title={intl.formatMessage({ id: 'webchat.reset', defaultMessage: 'New conversation' })}
        >
          <RotateCcw className="h-4 w-4" />
        </button>
      </div>

      {/* Messages */}
      <div className="flex-1 overflow-y-auto px-6 py-4">
        <div className="mx-auto max-w-2xl space-y-3">
          {messages.length === 0 && (
            <div className="flex flex-col items-center justify-center py-20 text-center">
              <span className="text-5xl">{agentIcon}</span>
              <h3 className="mt-4 text-lg font-medium text-stone-700 dark:text-stone-300">
                {intl.formatMessage({ id: 'webchat.welcome', defaultMessage: 'Hello! How can I help you?' })}
              </h3>
              <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
                {intl.formatMessage({ id: 'webchat.hint', defaultMessage: 'Type a message or use /help for commands' })}
              </p>
            </div>
          )}

          {messages.map((msg) => (
            <MessageBubble key={msg.id} message={msg} />
          ))}

          {isStreaming && <TypingIndicator />}

          <div ref={messagesEndRef} />
        </div>
      </div>

      {/* Input */}
      <div className="border-t border-stone-200 px-6 py-4 dark:border-stone-800">
        <div className="mx-auto flex max-w-2xl items-end gap-3">
          <textarea
            ref={inputRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder={intl.formatMessage({
              id: 'webchat.placeholder',
              defaultMessage: 'Type a message...',
            })}
            rows={1}
            className={cn(
              'flex-1 resize-none rounded-xl border border-stone-200 bg-white px-4 py-3 text-sm',
              'placeholder:text-stone-400 focus:border-amber-400 focus:outline-none focus:ring-2 focus:ring-amber-400/20',
              'dark:border-stone-700 dark:bg-stone-800 dark:text-stone-200 dark:placeholder:text-stone-500 dark:focus:border-amber-500'
            )}
            disabled={connectionState !== 'connected'}
          />
          <button
            onClick={handleSend}
            disabled={!input.trim() || isStreaming || connectionState !== 'connected'}
            className={cn(
              'flex h-11 w-11 items-center justify-center rounded-xl transition-colors',
              input.trim() && !isStreaming
                ? 'bg-amber-500 text-white hover:bg-amber-600'
                : 'bg-stone-100 text-stone-400 dark:bg-stone-800'
            )}
          >
            {isStreaming ? (
              <Loader2 className="h-5 w-5 animate-spin" />
            ) : (
              <Send className="h-5 w-5" />
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
