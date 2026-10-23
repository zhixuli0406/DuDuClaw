import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { Clock, FileText, History, Pencil } from 'lucide-react';
import { api } from '@/lib/api';
import type {
  AgentFileDiffResponse,
  AgentFileGetResponse,
  AgentFileKey,
  AgentFileListResponse,
  AgentFileSaveResponse,
  AgentFileVersion,
} from '@/lib/agent-files-types';
import { useAuthStore } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';
import { toast } from '@/lib/toast';
import { cn } from '@/lib/utils';
import { Button, Empty, ErrorState, Skeleton, Spinner, Textarea, useErrorMessage } from '@/components/mds';
import { ChangeReviewDialog } from '@/components/agent-files/ChangeReviewDialog';
import { ConflictView } from '@/components/agent-files/ConflictView';
import { DiffView } from '@/components/agent-files/DiffView';
import { DriftBanner } from '@/components/agent-files/DriftBanner';
import { HistoryDrawer } from '@/components/agent-files/HistoryDrawer';
import { UnsavedChangesDialog } from '@/components/agent-files/UnsavedChangesDialog';
import { RuntimeFilesSection } from '@/components/agent-files/RuntimeFilesSection';
import { agentFileHeading, agentFileLabel, agentFileName, isSkillKey } from '@/components/agent-files/review-text';
import { AgentFileList } from './AgentFileList';
import { useAgentFileSave, type SentChange } from './useAgentFileSave';

const encoder = new TextEncoder();
const byteLength = (s: string) => encoder.encode(s).length;
const DEFAULT_LIMIT = 64 * 1024;

export interface AgentFilesTabProps {
  agentId: string;
  /** Told whenever the editor holds an unsaved change (the page asks before
   *  switching tabs). */
  onDirtyChange?: (dirty: boolean) => void;
}

type Mode = 'view' | 'edit';

/**
 * AgentFilesTab (`/agents/:id/files`) — the AI employee's profile files in
 * plain language: read them, and (as the employee's owner or an admin) edit
 * one, compare, save through the automatic check, browse and restore earlier
 * versions. Who may do what comes from the gateway (`editable_by_me`), which
 * re-checks it on every call.
 */
export function AgentFilesTab({ agentId, onDirtyChange }: AgentFilesTabProps) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const navigate = useNavigate();
  const errorText = useErrorMessage();
  const role = useAuthStore((s) => s.user?.role);
  const bindings = useAuthStore((s) => s.bindings);
  const isAdmin = hasMinRole(role, 'admin');
  const level = (bindings ?? []).find((b) => b.agent_name === agentId)?.access_level;
  const canSeeHistory = isAdmin || level === 'owner' || level === 'operator';

  const [list, setList] = useState<AgentFileListResponse | null>(null);
  const [listError, setListError] = useState<unknown>(null);
  const [selected, setSelected] = useState<AgentFileKey>('soul');
  const [doc, setDoc] = useState<AgentFileGetResponse | null>(null);
  const [docError, setDocError] = useState<unknown>(null);
  const [mode, setMode] = useState<Mode>('view');
  const [draft, setDraft] = useState('');
  const [preview, setPreview] = useState<AgentFileDiffResponse | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  const [conflict, setConflict] = useState<{ mine: string; current: string } | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [leaveTo, setLeaveTo] = useState<AgentFileKey | 'discard' | null>(null);

  const entry = list?.files.find((f) => f.file === selected) ?? null;
  const editable = !!entry?.editable_by_me && selected !== 'agent_toml';
  const limit = (isSkillKey(selected) ? list?.limits.skill : list?.limits[selected]) ?? DEFAULT_LIMIT;
  const dirty = mode === 'edit' && doc !== null && draft !== doc.content;
  const draftBytes = useMemo(() => byteLength(draft), [draft]);
  const overLimit = draftBytes > limit;

  useEffect(() => onDirtyChange?.(dirty), [dirty, onDirtyChange]);
  useEffect(() => () => onDirtyChange?.(false), [onDirtyChange]);
  useEffect(() => {
    if (!dirty) return;
    const warn = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [dirty]);

  const loadList = useCallback(async () => {
    try {
      const r = await api.agentFiles.list(agentId);
      // An older gateway (or an odd answer) must not crash the page.
      setList({ agent_id: agentId, files: Array.isArray(r?.files) ? r.files : [], limits: r?.limits ?? {} });
      setListError(null);
    } catch (e) {
      setListError(e);
    }
  }, [agentId]);

  const docRequest = useRef(0);
  const loadDoc = useCallback(
    async (file: AgentFileKey) => {
      const n = ++docRequest.current;
      setDocError(null);
      try {
        const r = await api.agentFiles.get(agentId, file);
        const d: AgentFileGetResponse = {
          ...r,
          content: typeof r?.content === 'string' ? r.content : '',
          drift: r?.drift ?? { state: 'none', accepted_version_id: null },
        };
        if (n === docRequest.current) setDoc(d);
        return d;
      } catch (e) {
        if (n === docRequest.current) setDocError(e);
        return null;
      }
    },
    [agentId],
  );

  useEffect(() => {
    void loadList();
  }, [loadList]);
  useEffect(() => {
    setDoc(null);
    setMode('view');
    setPreview(null);
    setConflict(null);
    void loadDoc(selected);
  }, [selected, loadDoc]);

  const backToView = useCallback(() => {
    setMode('view');
    setPreview(null);
    setConflict(null);
  }, []);

  const save = useAgentFileSave(
    agentId,
    {
      onApplied: (res: AgentFileSaveResponse, sent: SentChange) => {
        if (res.status === 'unchanged') toast.info(t('agentFiles.editor.unchanged'));
        backToView();
        void loadDoc(sent.file);
        void loadList();
      },
      onConflict: (sent: SentChange) => {
        const mine = sent.kind === 'save' ? sent.content : '';
        void loadDoc(sent.file).then((d) => setConflict({ mine, current: d?.content ?? '' }));
      },
      onPending: () => {
        backToView();
        void loadList();
      },
      onError: (e) => toast.error(t('agentFiles.editor.saveFailed', { message: errorText(e) })),
    },
    errorText,
  );

  const select = (file: AgentFileKey) => {
    if (file === selected) return;
    if (dirty) setLeaveTo(file);
    else setSelected(file);
  };

  const startEdit = () => {
    if (!doc) return;
    setDraft(doc.content);
    setPreview(null);
    setMode('edit');
  };

  const showDiff = async () => {
    if (preview) {
      setPreview(null);
      return;
    }
    setPreviewBusy(true);
    try {
      setPreview(await api.agentFiles.diff(agentId, selected, { content: draft }));
    } catch (e) {
      toast.error(t('agentFiles.editor.diffFailed', { message: errorText(e) }));
    } finally {
      setPreviewBusy(false);
    }
  };

  const checkAndSave = () => {
    if (!doc) return;
    void save.submit({ kind: 'save', file: selected, content: draft, baseHash: doc.hash });
  };

  const restore = (versionId: string) => {
    if (!doc) return;
    setHistoryOpen(false);
    void save.submit({ kind: 'rollback', file: selected, versionId, baseHash: doc.hash });
  };

  const acceptCurrent = () => {
    if (!doc) return;
    void save.submit({ kind: 'save', file: selected, content: doc.content, baseHash: doc.hash });
  };

  const withdraw = async (approvalId: string) => {
    try {
      await api.agentFiles.withdraw(approvalId);
      toast.success(t('agentFiles.pending.withdrawn'));
      void loadList();
    } catch (e) {
      toast.error(t('agentFiles.pending.withdrawFailed', { message: errorText(e) }));
    }
  };

  // INSTRUCTIONS.md does not exist on a fresh employee: creating it is an
  // ordinary save of a first text (same check, history and approval path).
  const startCreateInstructions = () => {
    setDraft(t('agentFiles.instructions.template'));
    setPreview(null);
    setMode('edit');
  };

  const confirmLeave = () => {
    const to = leaveTo;
    setLeaveTo(null);
    backToView();
    if (to && to !== 'discard') setSelected(to);
  };

  if (listError) {
    return <ErrorState error={listError} onRetry={() => void loadList()} />;
  }
  if (!list) {
    return (
      <div className="grid gap-4 md:grid-cols-[16rem_1fr]" role="status" aria-label={t('agentFiles.loading')}>
        <Skeleton className="h-64" />
        <Skeleton className="h-64" />
      </div>
    );
  }

  const label = agentFileLabel(intl, selected);
  const heading = agentFileHeading(intl, selected);
  const missingInstructions = selected === 'instructions' && doc !== null && !doc.exists && mode === 'view' && !conflict;

  return (
    <div className="grid gap-4 md:grid-cols-[16rem_1fr]" data-testid="agent-files-tab">
      <AgentFileList files={list.files} selected={selected} onSelect={select} />

      <section aria-label={label} className="min-w-0 space-y-3">
        <header className="flex flex-wrap items-start justify-between gap-2">
          <div className="min-w-0">
            <h2 className="text-base font-semibold text-foreground">{heading}</h2>
            <p className="truncate font-mono text-[11px] text-muted-foreground">{agentFileName(selected)}</p>
          </div>
          {mode === 'view' && doc && (
            <div className="flex flex-wrap gap-2">
              {canSeeHistory && (
                <Button variant="outline" size="sm" onClick={() => setHistoryOpen(true)}>
                  <History aria-hidden="true" />
                  {t('agentFiles.editor.history')}
                </Button>
              )}
              {editable && !missingInstructions && (
                <Button variant="brand" size="sm" onClick={startEdit}>
                  <Pencil aria-hidden="true" />
                  {t('agentFiles.editor.edit')}
                </Button>
              )}
            </div>
          )}
        </header>

        {selected === 'instructions' && (
          <p className="text-sm text-muted-foreground" data-testid="instructions-help">
            {t('agentFiles.help.instructions')}
          </p>
        )}

        {selected === 'agent_toml' ? (
          <p className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
            {t('agentFiles.editor.basicReadOnly')}
            <Button variant="outline" size="sm" onClick={() => navigate(`/agents/${encodeURIComponent(agentId)}/edit`)}>
              {t('agentFiles.editor.openEditPage')}
            </Button>
          </p>
        ) : (
          !editable && <p className="text-sm text-muted-foreground" data-testid="view-only-hint">{t('agentFiles.editor.viewOnly')}</p>
        )}

        {entry?.pending_approval_id && (
          <p className="flex flex-wrap items-center gap-2 rounded-lg border border-surface-border bg-muted px-3 py-2 text-sm text-foreground" role="status">
            <Clock className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
            {t('agentFiles.pending.notice')}
            {editable && (
              <Button variant="ghost" size="xs" onClick={() => void withdraw(entry.pending_approval_id ?? '')}>
                {t('agentFiles.pending.withdraw')}
              </Button>
            )}
          </p>
        )}

        {doc?.drift.state === 'drifted' && mode === 'view' && (
          <DriftBanner
            acceptedVersionId={doc.drift.accepted_version_id}
            canEdit={editable}
            busy={save.review?.kind === 'checking'}
            onRestore={() => doc.drift.accepted_version_id && restore(doc.drift.accepted_version_id)}
            onAccept={acceptCurrent}
          />
        )}

        {docError ? (
          <ErrorState variant="inline" error={docError} onRetry={() => void loadDoc(selected)} />
        ) : !doc ? (
          <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground">
            <Spinner label={t('agentFiles.loading')} />
            {t('agentFiles.loading')}
          </p>
        ) : conflict ? (
          <ConflictView mine={conflict.mine} current={conflict.current} onReload={backToView} />
        ) : missingInstructions ? (
          <div className="space-y-2" data-testid="instructions-missing">
            <Empty
              icon={FileText}
              title={t(editable ? 'agentFiles.instructions.createHint' : 'agentFiles.instructions.createViewOnly')}
              variant="dashed"
              className="py-8"
            />
            {editable && (
              <div className="flex justify-center">
                <Button variant="brand" onClick={startCreateInstructions}>
                  <Pencil aria-hidden="true" />
                  {t('agentFiles.instructions.create')}
                </Button>
              </div>
            )}
          </div>
        ) : mode === 'view' ? (
          doc.content === '' ? (
            <Empty icon={FileText} title={t('agentFiles.editor.empty')} variant="dashed" className="py-8" />
          ) : (
            <pre
              className="max-h-[32rem] overflow-auto whitespace-pre-wrap break-words rounded-lg border border-surface-border bg-muted p-3 font-mono text-xs leading-relaxed"
              data-testid="agent-file-content"
            >
              {doc.content}
            </pre>
          )
        ) : (
          <div className="space-y-2">
            <Textarea
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              rows={20}
              spellCheck={false}
              aria-label={t('agentFiles.editor.textareaLabel', { file: label })}
              aria-describedby="agent-file-size"
              className={cn('font-mono text-xs leading-relaxed', overLimit && 'border-destructive')}
            />
            <p id="agent-file-size" className={cn('text-xs', overLimit ? 'text-destructive' : 'text-muted-foreground')}>
              {t(overLimit ? 'agentFiles.editor.overLimit' : 'agentFiles.editor.size', {
                used: Math.ceil(draftBytes / 1024),
                limit: Math.floor(limit / 1024),
              })}
            </p>
            <div className="flex flex-wrap justify-end gap-2">
              <Button variant="ghost" onClick={() => (dirty ? setLeaveTo('discard') : backToView())}>
                {t('agentFiles.editor.discard')}
              </Button>
              <Button variant="outline" onClick={() => void showDiff()} disabled={previewBusy || (!dirty && !preview)} aria-pressed={!!preview}>
                {t('agentFiles.editor.showDiff')}
              </Button>
              <Button variant="brand" onClick={checkAndSave} disabled={!dirty || overLimit || save.review?.kind === 'checking'}>
                {t('agentFiles.editor.saveAndCheck')}
              </Button>
            </div>
            {preview && (
              <div className="space-y-1">
                <p className="text-xs text-muted-foreground">
                  {t('agentFiles.approval.diffSummary', { added: preview.added, removed: preview.removed })}
                </p>
                <DiffView hunks={preview.hunks} />
              </div>
            )}
          </div>
        )}
      </section>

      <div className="md:col-span-2">
        <RuntimeFilesSection agentId={agentId} />
      </div>

      <ChangeReviewDialog
        state={save.review}
        isAdmin={isAdmin}
        busy={save.busy}
        error={save.error}
        onConfirm={(reason, ack) => void save.followUp('confirm', reason, ack)}
        onRequestApproval={(reason) => void save.followUp('request_approval', reason, false)}
        onClose={save.close}
        onReload={() => {
          save.close();
          backToView();
          void loadDoc(selected);
          void loadList();
        }}
        onApplied={() => toast.success(t('agentFiles.review.applied'))}
      />
      <HistoryDrawer
        open={historyOpen}
        onOpenChange={setHistoryOpen}
        agentId={agentId}
        file={selected}
        fileLabel={label}
        currentHash={doc?.hash ?? null}
        canRestore={editable}
        onRestore={(v: AgentFileVersion) => restore(v.version_id)}
      />
      <UnsavedChangesDialog open={leaveTo !== null} onStay={() => setLeaveTo(null)} onDiscard={confirmLeave} />
    </div>
  );
}
