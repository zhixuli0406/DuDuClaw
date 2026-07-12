import { useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useSearchParams } from 'react-router';
import { useChatStore, historyToMessages, type PendingAttachment } from '@/stores/chat-store';
import { api, type ChatSessionSummary } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { cn } from '@/lib/utils';
import { Send, RotateCcw, Loader2, Paperclip, Eye, EyeOff } from 'lucide-react';
import { toast } from '@/lib/toast';
import { Button, Badge } from '@/components/ui';
import {
  AttachmentChip,
  MessageBubble,
  TypingIndicator,
  TaskInsights,
  CenterStage,
  CornerDuDu,
  EmployeeRow,
  SessionHistoryMenu,
  useChatFace,
  MicButton,
  VoicePlayToggle,
  TalkModeButton,
  TalkModeStatusPill,
  useTalkMode,
  ttsSynthesizeUrl,
  VoiceNotConfiguredError,
} from '@/components/chat';
import { CharacterAvatar, type AgentLifecycle } from '@/components/character';
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
    sessionId,
    connect,
    send,
    reset,
    selectAgent,
    resumeSession,
  } = useChatStore();

  const agents = useAgentsStore((s) => s.agents);
  const agentsLoaded = useAgentsStore((s) => s.loaded);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);

  const [searchParams] = useSearchParams();
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

  // Auto-scroll to bottom on new content.
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, isStreaming, stepTree.length]);

  // The chosen partner (an AI staff member) or DuDu (the office assistant).
  const partner = useMemo(
    () => (selectedAgentId ? agents.find((a) => a.name === selectedAgentId) ?? null : null),
    [agents, selectedAgentId],
  );
  // The agent whose history to browse: the chosen employee, or (for DuDu) the
  // main agent that backs the default assistant. Non-admin callers must pass a
  // visible agent_id, so resolve one rather than relying on an admin default.
  const mainAgentId = useMemo(
    () => agents.find((a) => a.role === 'main')?.name ?? null,
    [agents],
  );
  const historyAgentId = selectedAgentId ?? mainAgentId;

  const handleResume = async (session: ChatSessionSummary) => {
    try {
      const hist = await api.chatSessions.history(session.session_id);
      resumeSession(session.session_id, historyToMessages(hist.messages ?? []));
    } catch {
      toast.error(
        intl.formatMessage({
          id: 'webchat.history.loadFailed',
          defaultMessage: '無法載入這個對話',
        }),
      );
    }
  };

  const partnerName = partner?.display_name ?? agentName;
  // Header icon follows the chosen partner (W6a fix: it previously stayed the
  // main agent's icon even after selecting a different employee).
  const partnerIcon = partner?.icon ?? agentIcon;
  const partnerStatus: AgentLifecycle = partner?.status ?? 'active';

  // Recording drives the `listening` face just like typing does.
  const face = useChatFace(phase, input.trim().length > 0 || isRecording, messages.length > 0);

  // Leading avatar for assistant bubbles — the partner's identity.
  const leadingAvatar = selectedAgentId ? (
    <CharacterAvatar agentId={selectedAgentId} name={partnerName} size={26} variant="avatar" animated={false} />
  ) : (
    <DuDu face="idle" size={24} animated={false} label="DuDu" />
  );

  // Split off a still-streaming assistant bubble so the step tree can sit above
  // it while the reply builds.
  const last = messages[messages.length - 1];
  const streamingAssistant =
    isStreaming && last?.role === 'assistant' && last.tokens == null ? last : null;
  const priorMessages = streamingAssistant ? messages.slice(0, -1) : messages;
  const insightsVisible = stepTree.length > 0 || steps.length > 0;

  const empty = messages.length === 0;
  const duduCentered = empty && !selectedAgentId;

  const handleSend = () => {
    const text = input.trim();
    if ((!text && attachments.length === 0) || isStreaming) return;
    send(text, attachments);
    setInput('');
    setAttachments([]);
    inputRef.current?.focus();
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  // Fill the composer from a voice transcript — never auto-send; the human
  // reviews and hits enter. Appends to any text already typed.
  const handleTranscript = (text: string) => {
    setInput((prev) => (prev ? `${prev.trimEnd()} ${text}` : text));
    inputRef.current?.focus();
  };

  // Talk Mode (G13): continuous voice loop — listen → STT → send → TTS →
  // listen. Transcripts go through the same `send()` path as typed messages.
  const talk = useTalkMode({
    onNotConfigured: (msg) => {
      toast.error(
        msg ||
          intl.formatMessage({
            id: 'voice.stt.notConfigured',
            defaultMessage: '尚未設定語音轉文字，請至設定 → 語音',
          }),
      );
    },
    onError: (msg) => {
      toast.error(
        intl.formatMessage(
          { id: 'voice.stt.failed', defaultMessage: '語音辨識失敗：{message}' },
          { message: msg },
        ),
      );
    },
    onTtsFailed: () => {
      toast.error(
        intl.formatMessage({
          id: 'voice.talk.ttsFailed',
          defaultMessage: '語音朗讀失敗，已繼續聆聽',
        }),
      );
    },
    onEngageFailed: (msg) => {
      if (msg === 'unsupported') return; // button is already disabled
      toast.error(
        intl.formatMessage(
          { id: 'voice.talk.engageFailed', defaultMessage: '無法啟動對話模式：{message}' },
          { message: msg },
        ),
      );
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

  // Reply playback (openhuman-parity B-P2): when the toggle is on, speak each
  // freshly-completed assistant reply via /api/tts. Guarded by message id so a
  // reply is spoken exactly once; a 501 quietly closes the toggle.
  const lastSpokenRef = useRef<string | null>(null);
  useEffect(() => {
    if (!ttsEnabled || phase !== 'done') return;
    // Talk Mode owns reply playback while engaged — avoid speaking twice.
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
          toast.error(
            err.message ||
              intl.formatMessage({
                id: 'voice.tts.notConfigured',
                defaultMessage: '尚未啟用語音朗讀，請至設定 → 語音',
              }),
          );
          setTtsEnabled(false);
        }
        // Other synthesis errors stay silent to avoid spamming per reply.
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
        toast.error(
          intl.formatMessage(
            { id: 'webchat.attachTooLarge', defaultMessage: '{name} 超過 20MB 上限' },
            { name: result.name },
          ),
        );
      } else {
        toast.error(
          intl.formatMessage(
            { id: 'webchat.attachReadFailed', defaultMessage: '無法讀取 {name}' },
            { name: result.name },
          ),
        );
      }
    }
    if (next.length > 0) setAttachments((prev) => [...prev, ...next]);
    // Reset so selecting the same file again re-triggers onChange.
    if (fileInputRef.current) fileInputRef.current.value = '';
  };

  const removeAttachment = (index: number) => {
    setAttachments((prev) => prev.filter((_, i) => i !== index));
  };

  const canSend =
    (input.trim().length > 0 || attachments.length > 0) &&
    !isStreaming &&
    connectionState === 'connected';

  return (
    <div className="flex h-full min-w-0 flex-col">
      {/* Header — partner identity + connection + reset. */}
      <div className="flex items-center justify-between border-b border-[var(--panel-border)] px-6 py-3">
        <div className="flex items-center gap-3">
          <span className="text-2xl">{partnerIcon}</span>
          <div>
            <h2 className="text-lg font-semibold tracking-tight text-stone-900 dark:text-stone-50">
              {partnerName}
            </h2>
            <div className="mt-0.5">
              <Badge
                dot
                tone={
                  connectionState === 'connected'
                    ? 'success'
                    : connectionState === 'connecting'
                      ? 'warning'
                      : 'neutral'
                }
              >
                {connectionState === 'connected'
                  ? intl.formatMessage({ id: 'webchat.connected', defaultMessage: 'Connected' })
                  : connectionState === 'connecting'
                    ? intl.formatMessage({ id: 'webchat.connecting', defaultMessage: 'Connecting...' })
                    : intl.formatMessage({ id: 'webchat.disconnected', defaultMessage: 'Disconnected' })}
              </Badge>
            </div>
          </div>
        </div>

        <div className="flex items-center gap-1">
          <VoicePlayToggle />
          <SessionHistoryMenu
            key={historyAgentId ?? 'dudu'}
            agentId={historyAgentId}
            activeSessionId={sessionId}
            onResume={handleResume}
          />
          <Button
            variant="ghost"
            icon={RotateCcw}
            onClick={reset}
            title={intl.formatMessage({ id: 'webchat.reset', defaultMessage: 'New conversation' })}
          />
        </div>
      </div>

      {/* Employee strip — pick the conversation partner. */}
      {agents.length > 0 && (
        <div className="border-b border-[var(--panel-border)]">
          <EmployeeRow agents={agents} selectedId={selectedAgentId} onSelect={selectAgent} />
        </div>
      )}

      {/* Conversation — DuDu holds the corner once there's history. */}
      <div className="relative min-h-0 flex-1 overflow-y-auto px-6 py-4">
        {!duduCentered && <CornerDuDu face={face} viseme={viseme} />}

        <div className="mx-auto max-w-3xl space-y-3">
          {empty ? (
            <CenterStage
              face={face}
              viseme={viseme}
              agentId={selectedAgentId}
              agentName={partnerName}
              agentStatus={partnerStatus}
              phase={phase}
            />
          ) : (
            <>
              {priorMessages.map((msg) => (
                <MessageBubble
                  key={msg.id}
                  message={msg}
                  leading={msg.role === 'assistant' ? leadingAvatar : undefined}
                />
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
      <div className="border-t border-[var(--panel-border)] px-6 py-4">
        <div className="mx-auto max-w-3xl space-y-2">
          {/* Capability badge */}
          <div className="flex items-center gap-2 text-xs text-stone-400 dark:text-stone-500">
            {supportsVision ? (
              <span
                title={intl.formatMessage({
                  id: 'webchat.visionOnTip',
                  defaultMessage: '此模型可理解上傳的圖片內容',
                })}
              >
                <Badge tone="success">
                  <Eye className="h-3.5 w-3.5" />
                  {intl.formatMessage({ id: 'webchat.visionOn', defaultMessage: '支援圖片' })}
                </Badge>
              </span>
            ) : (
              <span
                title={intl.formatMessage({
                  id: 'webchat.visionOffTip',
                  defaultMessage: '此模型不支援圖片理解,僅會讀取文件的文字內容',
                })}
              >
                <Badge tone="neutral">
                  <EyeOff className="h-3.5 w-3.5" />
                  {intl.formatMessage({ id: 'webchat.visionOff', defaultMessage: '僅文件' })}
                </Badge>
              </span>
            )}
            {model && <span className="opacity-70 tabular-nums">· {model}</span>}
            {/* Talk Mode live state (G13): listening / transcribing / speaking. */}
            <TalkModeStatusPill status={talk.status} className="ml-auto" />
          </div>

          {/* Vision warning when an image is queued for a text-only model */}
          {showVisionWarning && (
            <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-700 dark:text-amber-400">
              {intl.formatMessage({
                id: 'webchat.visionWarning',
                defaultMessage: '⚠️ 目前模型不支援圖片理解 — 圖片仍會送出,但模型可能無法看見內容。',
              })}
            </div>
          )}

          {/* Pending attachment chips */}
          {attachments.length > 0 && (
            <div className="flex flex-wrap gap-1.5">
              {attachments.map((a, i) => (
                <AttachmentChip key={i} name={a.name} mime={a.mime} onRemove={() => removeAttachment(i)} />
              ))}
            </div>
          )}

          <div className="flex items-end gap-3">
            <input
              ref={fileInputRef}
              type="file"
              multiple
              className="hidden"
              onChange={(e) => handleFilesSelected(e.target.files)}
            />
            <Button
              variant="secondary"
              icon={Paperclip}
              onClick={() => fileInputRef.current?.click()}
              disabled={connectionState !== 'connected'}
              title={intl.formatMessage({ id: 'webchat.attach', defaultMessage: '上傳檔案' })}
              className="h-11 w-11 shrink-0 rounded-control"
            />
            {/* Push-to-talk mic (openhuman-parity B): hold to record, release to
                transcribe into the composer. Disabled with a tooltip when the
                browser can't capture audio or the socket is down. */}
            <MicButton
              onTranscript={handleTranscript}
              disabled={connectionState !== 'connected' || talk.active}
              onCapturingChange={setPttCapturing}
            />
            {/* Talk Mode toggle (G13): continuous voice conversation loop.
                Exits via the same button or Esc. Mutually exclusive with the
                push-to-talk hold — disabled while PTT is capturing so a
                two-finger touch can't drive both recorders at once. */}
            <TalkModeButton
              handle={talk}
              disabled={connectionState !== 'connected' || pttCapturing}
            />
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
                'flex-1 resize-none rounded-control border border-[var(--panel-border)] bg-[var(--panel-fill)] px-4 py-3 text-sm',
                'text-stone-800 placeholder:text-stone-400 focus-visible:border-amber-500/50 focus-visible:outline-none',
                'focus-visible:ring-2 focus-visible:ring-amber-500/30 dark:text-stone-100 dark:placeholder:text-stone-500',
              )}
              disabled={connectionState !== 'connected'}
            />
            <Button
              variant="primary"
              onClick={handleSend}
              disabled={!canSend}
              className="h-11 w-11 shrink-0 rounded-control px-0"
            >
              {isStreaming ? (
                <Loader2 className="h-5 w-5 animate-spin" />
              ) : (
                <Send className="h-5 w-5" />
              )}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
