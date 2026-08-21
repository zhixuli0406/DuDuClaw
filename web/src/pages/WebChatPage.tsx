import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useLocation, useNavigate, useSearchParams } from 'react-router';
import { useChatStore, type PendingAttachment } from '@/stores/chat-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useConversationsStore } from '@/stores/conversations-store';
import { cn } from '@/lib/utils';
import { isImeComposing } from '@/lib/keyboard';
import { useUrlStateNullable } from '@/lib/use-url-state';
import { Plus, Paperclip, Eye, EyeOff, MessagesSquare, AlertTriangle } from 'lucide-react';
import { toast } from '@/lib/toast';
import {
  Button,
  SubmitButton,
} from '@/components/mds';
import {
  AttachmentChip,
  MessageBubble,
  TypingIndicator,
  TaskInsights,
  EmployeeRow,
  useChatFace,
  MicButton,
  VoicePlayToggle,
  TalkModeButton,
  TalkModeStatusPill,
  useTalkMode,
  ttsSynthesizeUrl,
  VoiceNotConfiguredError,
} from '@/components/chat';
import { CharacterAvatar } from '@/components/character';
import { AgentGlyph } from '@/lib/agent-glyph';
import { DuDu } from '@/components/mascot';
import { isImageMime, readAttachment } from '@/lib/attachments';

export function WebChatPage() {
  const intl = useIntl();
  const {
    messages,
    steps,
    stepTree,
    isStreaming,
    phase,
    viseme,
    agentName,
    agentIcon,
    selectedAgentId,
    supportsVision,
    model,
    connectionState,
    isRecording,
    ttsEnabled,
    setTtsEnabled,
    sessionsRevision,
    connect,
    send,
    selectAgent,
  } = useChatStore();

  const agents = useAgentsStore((s) => s.agents);
  const agentsLoaded = useAgentsStore((s) => s.loaded);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);

  const [searchParams] = useSearchParams();
  // P11 (state-as-URL): who you are talking to is page state, so it round-trips
  // through `?agent=<id>` instead of being read once at mount and then lost on
  // the next refresh.
  const [, setAgentParam] = useUrlStateNullable('agent');
  const navigate = useNavigate();
  const [input, setInput] = useState('');
  const [attachments, setAttachments] = useState<PendingAttachment[]>([]);
  const [pttCapturing, setPttCapturing] = useState(false);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const hasImageAttachment = attachments.some((a) => isImageMime(a.mime));
  const showVisionWarning = hasImageAttachment && !supportsVision;

  // Connect on mount; preselect the conversation partner from `?agent=<id>`.
  useEffect(() => {
    if (connectionState === 'disconnected') connect();
    if (!agentsLoaded) fetchAgents();
    const preselect = searchParams.get('agent');
    if (preselect) selectAgent(preselect);
    // Keep the session alive across unmounts — no disconnect here.
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // 問一問 handoff (UX plan I-1b): the AssignSheet passes its draft through
  // router state rather than the URL (a goal paragraph does not belong in a
  // query string). Seeded into the composer, never auto-sent — the user still
  // reads it and presses send — and consumed once so a back-navigation or a
  // refresh cannot resurrect an already-sent draft.
  const location = useLocation();
  useEffect(() => {
    const draft = (location.state as { draft?: unknown } | null)?.draft;
    if (typeof draft !== 'string' || draft.trim().length === 0) return;
    setInput((prev) => (prev ? prev : draft));
    navigate(location.pathname + location.search, { replace: true, state: null });
    inputRef.current?.focus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [location.key]);

  // Auto-scroll to bottom on new content.
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, isStreaming, stepTree.length]);

  // Auto-grow the composer textarea up to a cap.
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, [input]);

  // The chosen partner (an AI staff member) or DuDu (the office assistant).
  const partner = useMemo(
    () => (selectedAgentId ? agents.find((a) => a.name === selectedAgentId) ?? null : null),
    [agents, selectedAgentId],
  );
  const mainAgentId = useMemo(() => agents.find((a) => a.role === 'main')?.name ?? null, [agents]);
  const historyAgentId = selectedAgentId ?? mainAgentId;

  // The main agent is already represented by the leading DuDu chip (which routes
  // to it), so it gets no duplicate employee chip in the row.
  const staffAgents = useMemo(() => agents.filter((a) => a.role !== 'main'), [agents]);

  /**
   * Every user-driven partner switch goes through here so the store and the URL
   * never disagree (P11). Selecting DuDu drops the param rather than writing an
   * empty one.
   */
  const handleSelectAgent = useCallback(
    (id: string | null) => {
      selectAgent(id);
      setAgentParam(id);
    },
    [selectAgent, setAgentParam],
  );

  // A restored/preselected selection (e.g. `?agent=<main>`) may point at the main
  // agent, whose chip no longer exists — normalize it back to DuDu so the
  // highlighted entry is always present (and the URL follows).
  useEffect(() => {
    if (mainAgentId && selectedAgentId === mainAgentId) handleSelectAgent(null);
  }, [mainAgentId, selectedAgentId, handleSelectAgent]);

  // ── Conversation history ────────────────────────────────────────────────────
  //
  // The list itself moved out on 2026-08-04 (D17) — it duplicated the sidebar
  // 對話紀錄 zone, and everything older now lives on `/conversations`. What
  // stays here is the refresh: a completed turn must make the just-created
  // conversation appear in those two places without a manual reload.
  const loadSessions = useConversationsStore((s) => s.fetch);
  const startNewConversation = useConversationsStore((s) => s.startNew);

  useEffect(() => {
    if (connectionState === 'connected') void loadSessions();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [historyAgentId, connectionState]);

  // Refresh the list whenever a reply lands (`sessionsRevision` bumps once per
  // completed turn) so a just-created conversation appears — and stays
  // resumable — without a manual reload.
  useEffect(() => {
    if (sessionsRevision > 0 && connectionState === 'connected') void loadSessions();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionsRevision]);

  const partnerName = partner?.display_name ?? agentName;

  // Recording drives the `listening` face just like typing does.
  const face = useChatFace(phase, input.trim().length > 0 || isRecording, messages.length > 0);

  // Leading avatar for assistant bubbles — the partner's identity.
  const leadingAvatar = selectedAgentId ? (
    <CharacterAvatar agentId={selectedAgentId} name={partnerName} size={26} variant="avatar" animated={false} />
  ) : (
    <DuDu face="idle" size={24} animated={false} label="DuDu" />
  );

  // Split off a still-streaming assistant bubble so the step tree can sit above it.
  const last = messages[messages.length - 1];
  const streamingAssistant = isStreaming && last?.role === 'assistant' && last.tokens == null ? last : null;
  const priorMessages = streamingAssistant ? messages.slice(0, -1) : messages;
  const insightsVisible = stepTree.length > 0 || steps.length > 0;
  const empty = messages.length === 0;

  const newConversation = () => {
    startNewConversation();
  };

  const handleSend = () => {
    const text = input.trim();
    if ((!text && attachments.length === 0) || isStreaming) return;
    send(text, attachments);
    setInput('');
    setAttachments([]);
    inputRef.current?.focus();
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Ignore Enter while a CJK IME is composing (注音/拼音): the first Enter
    // confirms candidate selection, not send — otherwise a half-composed message
    // is dispatched. See `isImeComposing`.
    if (e.key === 'Enter' && !e.shiftKey && !isImeComposing(e)) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleTranscript = (text: string) => {
    setInput((prev) => (prev ? `${prev.trimEnd()} ${text}` : text));
    inputRef.current?.focus();
  };

  // Talk Mode (G13): continuous voice loop. Transcripts go through the same
  // `send()` path as typed messages.
  const talk = useTalkMode({
    onNotConfigured: (msg) => {
      toast.error(msg || intl.formatMessage({ id: 'voice.stt.notConfigured', defaultMessage: '尚未設定語音轉文字，請至設定 → 語音' }));
    },
    onError: (msg) => {
      toast.error(intl.formatMessage({ id: 'voice.stt.failed', defaultMessage: '語音辨識失敗：{message}' }, { message: msg }));
    },
    onTtsFailed: () => {
      toast.error(intl.formatMessage({ id: 'voice.talk.ttsFailed', defaultMessage: '語音朗讀失敗，已繼續聆聽' }));
    },
    onEngageFailed: (msg) => {
      if (msg === 'unsupported') return;
      toast.error(intl.formatMessage({ id: 'voice.talk.engageFailed', defaultMessage: '無法啟動對話模式：{message}' }, { message: msg }));
    },
  });

  // Esc exits Talk Mode from anywhere on the page.
  useEffect(() => {
    if (!talk.active) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        talk.stop();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [talk.active, talk.stop]);

  // Reply playback: when the toggle is on, speak each freshly-completed reply.
  const lastSpokenRef = useRef<string | null>(null);
  useEffect(() => {
    if (!ttsEnabled || phase !== 'done') return;
    if (talk.active) return;
    const latest = messages[messages.length - 1];
    if (!latest || latest.role !== 'assistant' || !latest.content.trim()) return;
    if (lastSpokenRef.current === latest.id) return;
    lastSpokenRef.current = latest.id;

    let cancelled = false;
    let objectUrl: string | null = null;
    (async () => {
      try {
        objectUrl = await ttsSynthesizeUrl(latest.content);
        if (cancelled) {
          URL.revokeObjectURL(objectUrl);
          return;
        }
        const audio = new Audio(objectUrl);
        audio.onended = () => objectUrl && URL.revokeObjectURL(objectUrl);
        await audio.play().catch(() => {
          /* autoplay may be blocked until first user gesture — ignore */
        });
      } catch (err) {
        if (objectUrl) URL.revokeObjectURL(objectUrl);
        if (err instanceof VoiceNotConfiguredError) {
          toast.error(err.message || intl.formatMessage({ id: 'voice.tts.notConfigured', defaultMessage: '尚未啟用語音朗讀，請至設定 → 語音' }));
          setTtsEnabled(false);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [phase, ttsEnabled, messages, setTtsEnabled, intl, talk.active]);

  const handleFilesSelected = async (fileList: FileList | null) => {
    if (!fileList || fileList.length === 0) return;
    const next: PendingAttachment[] = [];
    for (const file of Array.from(fileList)) {
      const result = await readAttachment(file);
      if (result.ok) {
        next.push(result.attachment);
      } else if (result.reason === 'too-large') {
        toast.error(intl.formatMessage({ id: 'webchat.attachTooLarge', defaultMessage: '{name} 超過 20MB 上限' }, { name: result.name }));
      } else {
        toast.error(intl.formatMessage({ id: 'webchat.attachReadFailed', defaultMessage: '無法讀取 {name}' }, { name: result.name }));
      }
    }
    if (next.length > 0) setAttachments((prev) => [...prev, ...next]);
    if (fileInputRef.current) fileInputRef.current.value = '';
  };

  const removeAttachment = (index: number) => {
    setAttachments((prev) => prev.filter((_, i) => i !== index));
  };

  const canSend =
    (input.trim().length > 0 || attachments.length > 0) && !isStreaming && connectionState === 'connected';

  const suggestions = useMemo(
    () => [
      intl.formatMessage({ id: 'webchat.suggest.1', defaultMessage: '今天有哪些待辦？' }),
      intl.formatMessage({ id: 'webchat.suggest.2', defaultMessage: '幫我整理這週的重點' }),
      intl.formatMessage({ id: 'webchat.suggest.3', defaultMessage: '交辦一個任務給員工' }),
    ],
    [intl],
  );

  // ── Conversation status dot ───────────────────────────────────────────────────
  const statusTone =
    connectionState === 'connected' ? 'bg-success' : connectionState === 'connecting' ? 'bg-warning' : 'bg-muted-foreground';
  const statusText =
    connectionState === 'connected'
      ? intl.formatMessage({ id: 'webchat.connected', defaultMessage: 'Connected' })
      : connectionState === 'connecting'
        ? intl.formatMessage({ id: 'webchat.connecting', defaultMessage: 'Connecting…' })
        : intl.formatMessage({ id: 'webchat.disconnected', defaultMessage: 'Disconnected' });

  // ── Right column: the conversation ────────────────────────────────────────────
  const chatColumn = (
    <div className="flex h-full min-h-0 flex-col">
      {/* Session header — partner identity + status + companion + actions. */}
      <div className="flex h-12 shrink-0 items-center gap-2 border-b border-surface-border px-3">
        {selectedAgentId ? (
          <CharacterAvatar agentId={selectedAgentId} name={partnerName} size={26} variant="avatar" animated={false} />
        ) : (
          <AgentGlyph
            icon={agentIcon}
            className="text-xl leading-none"
            iconClassName="size-5 text-muted-foreground"
          />
        )}
        <div className="min-w-0">
          <div className="flex items-center gap-1.5">
            <span className="truncate text-sm font-medium text-foreground">{partnerName}</span>
          </div>
        </div>
        <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <span className={cn('size-1.5 rounded-full', statusTone)} aria-hidden="true" />
          <span className="hidden sm:inline">{statusText}</span>
        </span>
        <div className="ml-auto flex items-center gap-0.5">
          <VoicePlayToggle />
          {/* 對話紀錄 lives in the sidebar (newest five) and on its own page —
              the page-local duplicate list was removed 2026-08-04 (D17), so
              this is the way back to older threads from here. */}
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={() => navigate('/conversations')}
            title={intl.formatMessage({ id: 'nav.conversations' })}
            aria-label={intl.formatMessage({ id: 'nav.conversations' })}
          >
            <MessagesSquare />
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={newConversation}
            title={intl.formatMessage({ id: 'webchat.reset', defaultMessage: 'New conversation' })}
            aria-label={intl.formatMessage({ id: 'webchat.reset', defaultMessage: 'New conversation' })}
          >
            <Plus />
          </Button>
        </div>
      </div>

      {/* Staff strip — pick who you are talking to. Previously the top of the
          removed left column; it is conversation chrome, not history chrome. */}
      {staffAgents.length > 0 && (
        <div className="shrink-0 border-b border-surface-border">
          <EmployeeRow agents={staffAgents} selectedId={selectedAgentId} onSelect={handleSelectAgent} />
        </div>
      )}

      {/* Conversation */}
      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
        <div className="mx-auto max-w-3xl space-y-3">
          {empty ? (
            <div className="flex flex-col items-center justify-center gap-4 py-12 text-center">
              {selectedAgentId ? (
                <CharacterAvatar agentId={selectedAgentId} name={partnerName} size={88} variant="bust" />
              ) : (
                <DuDu face={face} viseme={viseme} size={80} label={`DuDu, ${face}`} />
              )}
              <div>
                <h3 className="text-base font-medium text-foreground">{partnerName}</h3>
                <p className="mt-0.5 text-sm text-muted-foreground">
                  {intl.formatMessage({ id: 'webchat.welcome', defaultMessage: 'Hi! How can I help you today?' })}
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
                <MessageBubble key={msg.id} message={msg} leading={msg.role === 'assistant' ? leadingAvatar : undefined} />
              ))}

              {insightsVisible && <TaskInsights tree={stepTree} todos={steps} streaming={isStreaming} />}

              {streamingAssistant && <MessageBubble message={streamingAssistant} leading={leadingAvatar} />}

              {isStreaming && !streamingAssistant && <TypingIndicator />}
            </>
          )}

          <div ref={messagesEndRef} />
        </div>
      </div>

      {/* Composer — pinned to the bottom. */}
      <div className="shrink-0 border-t border-surface-border px-4 py-3">
        <div className="mx-auto max-w-3xl space-y-2">
          {/* Capability + model + Talk Mode live state */}
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            {supportsVision ? (
              <span className="inline-flex items-center gap-1" title={intl.formatMessage({ id: 'webchat.visionOnTip' })}>
                <Eye className="size-3.5 text-success" />
                {intl.formatMessage({ id: 'webchat.visionOn', defaultMessage: '支援圖片' })}
              </span>
            ) : (
              <span className="inline-flex items-center gap-1" title={intl.formatMessage({ id: 'webchat.visionOffTip' })}>
                <EyeOff className="size-3.5" />
                {intl.formatMessage({ id: 'webchat.visionOff', defaultMessage: '僅文件' })}
              </span>
            )}
            {model && <span className="font-mono tabular-nums opacity-70">· {model}</span>}
            <TalkModeStatusPill status={talk.status} className="ml-auto" />
          </div>

          {showVisionWarning && (
            <div className="flex items-start gap-1.5 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
              <AlertTriangle className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
              <span>
                {intl.formatMessage({
                  id: 'webchat.visionWarning',
                  defaultMessage: '目前模型不支援圖片理解 — 圖片仍會送出,但模型可能無法看見內容。',
                })}
              </span>
            </div>
          )}

          {attachments.length > 0 && (
            <div className="flex flex-wrap gap-1.5">
              {attachments.map((a, i) => (
                <AttachmentChip key={i} name={a.name} mime={a.mime} onRemove={() => removeAttachment(i)} />
              ))}
            </div>
          )}

          <div className="flex items-end gap-1.5">
            <input ref={fileInputRef} type="file" multiple className="hidden" onChange={(e) => handleFilesSelected(e.target.files)} />
            <Button
              variant="ghost"
              size="icon"
              onClick={() => fileInputRef.current?.click()}
              disabled={connectionState !== 'connected'}
              title={intl.formatMessage({ id: 'webchat.attach', defaultMessage: '上傳檔案' })}
              aria-label={intl.formatMessage({ id: 'webchat.attach', defaultMessage: '上傳檔案' })}
            >
              <Paperclip />
            </Button>
            <MicButton
              onTranscript={handleTranscript}
              disabled={connectionState !== 'connected' || talk.active}
              onCapturingChange={setPttCapturing}
            />
            <TalkModeButton handle={talk} disabled={connectionState !== 'connected' || pttCapturing} />
            <textarea
              ref={inputRef}
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder={intl.formatMessage({ id: 'webchat.placeholder', defaultMessage: 'Type a message…' })}
              rows={1}
              className={cn(
                'min-h-8 flex-1 resize-none rounded-lg border border-input bg-transparent px-3 py-1.5 text-sm',
                'placeholder:text-muted-foreground focus-visible:border-ring focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/50',
                'dark:bg-input/30',
              )}
              disabled={connectionState !== 'connected'}
            />
            <SubmitButton
              state={isStreaming ? 'submitting' : 'idle'}
              onClick={handleSend}
              disabled={!canSend}
              title={intl.formatMessage({ id: 'webchat.hint', defaultMessage: 'Enter to send, Shift+Enter for newline' })}
            />
          </div>
        </div>
      </div>
    </div>
  );

  // Single column since 2026-08-04 (D17): the page-local history list duplicated
  // the sidebar 對話紀錄 zone, so the split view went with it. The conversation
  // now gets the full width, which is what a chat surface wanted anyway.
  return (
    <div className="-mx-4 -mt-4 flex min-h-0 flex-1 md:-mx-6 md:-mt-6 md:-mb-6">
      <div className="w-full">{chatColumn}</div>
    </div>
  );
}
