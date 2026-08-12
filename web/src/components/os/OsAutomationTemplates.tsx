import { useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import {
  api,
  type OsAgentStatus,
  type AutopilotCondition,
  type AutopilotAction,
} from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  CardDescription,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Spinner,
} from '@/components/mds';
import { FolderInput, BellRing, Wand2 } from 'lucide-react';

/**
 * OsAutomationTemplates — one-click OS automation rule templates (OS page).
 *
 * The OS perception stack (os_file / os_frontmost events) has been wired to
 * the autopilot engine since P2-4, but authoring a rule required knowing the
 * event names + condition JSON by hand — so in practice nobody used it (the
 * "整個 OS 員工功能不知道在幹麻" field report). Each template fills a small
 * form and creates a real autopilot rule via the existing `autopilot.create`
 * RPC (validated server-side, circuit-breaker protected like any other rule);
 * the file template also appends the watch path via `os.settings.update`.
 */

type TemplateKind = 'file' | 'app';

/** Engine-shaped condition/action payloads (the legacy TS types in api.ts
 *  predate the `all/any + field/op/value` engine schema — server validates). */
const asCondition = (v: unknown) => v as AutopilotCondition;
const asAction = (v: unknown) => v as AutopilotAction;

export function OsAutomationTemplates({
  agents,
  displayNameOf,
  onApplied,
}: {
  agents: ReadonlyArray<OsAgentStatus>;
  displayNameOf: (agentId: string) => string;
  onApplied?: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id }, values);
  const navigate = useNavigate();

  const osAgents = useMemo(() => agents.filter((a) => a.os_native), [agents]);

  const [open, setOpen] = useState<TemplateKind | null>(null);
  const [applying, setApplying] = useState(false);
  const [agentId, setAgentId] = useState('');
  const [folder, setFolder] = useState('~/Downloads');
  const [appKeyword, setAppKeyword] = useState('');
  const [channel, setChannel] = useState('telegram');
  const [chatId, setChatId] = useState('');
  const [reminderText, setReminderText] = useState('');

  const openDialog = (kind: TemplateKind) => {
    setAgentId(osAgents[0]?.agent_id ?? '');
    setOpen(kind);
  };

  const selected = osAgents.find((a) => a.agent_id === agentId);

  const applyFileTemplate = async () => {
    if (!selected || !folder.trim()) return;
    // Append the watch path (idempotent) so the os_file events actually flow.
    const paths = selected.watch.paths.includes(folder.trim())
      ? selected.watch.paths
      : [...selected.watch.paths, folder.trim()];
    await api.os.settingsUpdate({
      agent_id: selected.agent_id,
      os_watch: { paths },
    });
    await api.autopilot.create({
      name: t('os.templates.file.ruleName', { agent: displayNameOf(selected.agent_id) }),
      trigger_event: 'os_file',
      conditions: asCondition({
        all: [{ field: 'agent_id', op: 'eq', value: selected.agent_id }],
      }),
      action: asAction({
        type: 'delegate',
        target_agent: selected.agent_id,
        prompt:
          '收到新檔案事件：{path}（變更類型 {kind}）。請判斷檔案性質（發票／收據／合約／一般文件），' +
          '把它歸檔到你工作目錄中對應的子資料夾，並以一句話回報你做了什麼。',
      }),
    });
  };

  const applyAppTemplate = async () => {
    if (!selected || !appKeyword.trim() || !chatId.trim()) return;
    await api.autopilot.create({
      name: t('os.templates.app.ruleName', {
        agent: displayNameOf(selected.agent_id),
        app: appKeyword.trim(),
      }),
      trigger_event: 'os_frontmost',
      conditions: asCondition({
        all: [
          { field: 'agent_id', op: 'eq', value: selected.agent_id },
          { field: 'app', op: 'contains', value: appKeyword.trim() },
        ],
      }),
      action: asAction({
        type: 'notify',
        channel,
        chat_id: chatId.trim(),
        text: reminderText.trim() || t('os.templates.app.defaultText'),
      }),
    });
  };

  const handleApply = async () => {
    if (!open) return;
    setApplying(true);
    try {
      if (open === 'file') await applyFileTemplate();
      else await applyAppTemplate();
      // UX audit §2-7 — the rule this just created is invisible from this
      // page (OSPage has no view/edit UI for its own automations); point the
      // operator straight at SettingsPage's autopilot tab, the only place it
      // can be reviewed, edited, or deleted.
      toast.success(t('os.templates.applied'), {
        action: { label: t('os.templates.manageLink'), onClick: () => navigate('/manage/system?tab=autopilot') },
      });
      setOpen(null);
      onApplied?.();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'os.templates.applyFailed' }, { message: formatError(e) }));
    } finally {
      setApplying(false);
    }
  };

  const canApply =
    !!selected &&
    (open === 'file' ? folder.trim().length > 0 : appKeyword.trim().length > 0 && chatId.trim().length > 0);

  const templates: { kind: TemplateKind; icon: typeof FolderInput; title: string; desc: string }[] = [
    { kind: 'file', icon: FolderInput, title: t('os.templates.file.title'), desc: t('os.templates.file.desc') },
    { kind: 'app', icon: BellRing, title: t('os.templates.app.title'), desc: t('os.templates.app.desc') },
  ];

  return (
    <Card>
      <CardHeader className="space-y-1">
        <CardTitle className="flex items-center gap-2 text-sm">
          <Wand2 className="size-4 text-muted-foreground" />
          {t('os.templates.title')}
        </CardTitle>
        <CardDescription>{t('os.templates.desc')}</CardDescription>
      </CardHeader>
      <CardContent>
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
          {templates.map(({ kind, icon: Icon, title, desc }) => (
            <div
              key={kind}
              className="flex items-start justify-between gap-3 rounded-xl border border-surface-border p-3"
            >
              <div className="flex min-w-0 items-start gap-2.5">
                <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                <div className="min-w-0">
                  <p className="text-sm font-medium">{title}</p>
                  <p className="mt-0.5 text-xs text-muted-foreground">{desc}</p>
                </div>
              </div>
              {osAgents.length === 0 ? (
                <Badge variant="ghost">{t('os.templates.needOsNative')}</Badge>
              ) : (
                <Button variant="outline" size="sm" onClick={() => openDialog(kind)}>
                  {t('os.templates.apply')}
                </Button>
              )}
            </div>
          ))}
        </div>
      </CardContent>

      <Dialog open={open !== null} onOpenChange={(o) => { if (!o && !applying) setOpen(null); }}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {open === 'file' ? t('os.templates.file.title') : t('os.templates.app.title')}
            </DialogTitle>
            <DialogDescription>
              {open === 'file' ? t('os.templates.file.desc') : t('os.templates.app.desc')}
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-3">
            <div className="space-y-1.5">
              <label className="text-xs font-medium text-muted-foreground">
                {t('os.templates.agent')}
              </label>
              <Select value={agentId} onValueChange={(v) => setAgentId(String(v))}>
                <SelectTrigger size="sm">
                  <SelectValue>
                    {selected ? displayNameOf(selected.agent_id) : t('os.templates.agent')}
                  </SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {osAgents.map((a) => (
                    <SelectItem key={a.agent_id} value={a.agent_id}>
                      {displayNameOf(a.agent_id)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            {open === 'file' && (
              <div className="space-y-1.5">
                <label className="text-xs font-medium text-muted-foreground">
                  {t('os.templates.folder')}
                </label>
                <Input value={folder} onChange={(e) => setFolder(e.target.value)} placeholder="~/Downloads" />
              </div>
            )}

            {open === 'app' && (
              <>
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">
                    {t('os.templates.appKeyword')}
                  </label>
                  <Input
                    value={appKeyword}
                    onChange={(e) => setAppKeyword(e.target.value)}
                    placeholder="YouTube / 遊戲名 / Figma…"
                  />
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <div className="space-y-1.5">
                    <label className="text-xs font-medium text-muted-foreground">
                      {t('os.templates.channel')}
                    </label>
                    <Select value={channel} onValueChange={(v) => setChannel(String(v))}>
                      <SelectTrigger size="sm">
                        <SelectValue>{channel}</SelectValue>
                      </SelectTrigger>
                      <SelectContent>
                        {['telegram', 'discord', 'line', 'slack'].map((c) => (
                          <SelectItem key={c} value={c}>{c}</SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-1.5">
                    <label className="text-xs font-medium text-muted-foreground">
                      {t('os.templates.chatId')}
                    </label>
                    <Input value={chatId} onChange={(e) => setChatId(e.target.value)} />
                  </div>
                </div>
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-muted-foreground">
                    {t('os.templates.reminderText')}
                  </label>
                  <Input
                    value={reminderText}
                    onChange={(e) => setReminderText(e.target.value)}
                    placeholder={t('os.templates.app.defaultText')}
                  />
                </div>
              </>
            )}
          </div>

          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setOpen(null)} disabled={applying}>
              {t('common.cancel')}
            </Button>
            <Button variant="brand" size="sm" onClick={handleApply} disabled={applying || !canApply}>
              {applying && <Spinner className="size-3.5" />}
              {t('os.templates.apply')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Card>
  );
}
