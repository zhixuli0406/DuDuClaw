import { useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  AlertTriangle,
  CheckCircle2,
  XCircle,
  Loader2,
  SendHorizonal,
  ExternalLink,
  Info,
} from 'lucide-react';
import { api } from '@/lib/api';
import { isImeComposing } from '@/lib/keyboard';
import { client } from '@/lib/ws-client';
import { Dialog, DialogContent, DialogHeader, DialogTitle, Button, Input } from '@/components/mds';

/* eslint-disable no-control-regex */
/**
 * Strip ANSI / VT escape sequences from raw PTY output so the streamed CLI login
 * transcript is human-readable instead of a wall of escape codes. The login CLIs
 * render with a full-screen Ink TUI; this won't perfectly reconstruct the redraw,
 * but it removes the garbage so the prompt + result text are legible.
 */
function stripAnsi(s: string): string {
  return (
    s
      // CSI sequences: ESC [ … final byte
      .replace(/\[[0-9;?=>!]*[A-Za-z@]/g, '')
      // OSC sequences: ESC ] … (BEL or ESC \ terminator)
      .replace(/\][\s\S]*?(?:|\\)/g, '')
      // charset selection: ESC ( / ) / # / % X
      .replace(/[()#%][0-9A-Za-z]/g, '')
      // misc single-char escapes: ESC =, ESC >, ESC 7/8, ESC M …
      .replace(/[=>NODEHM78]/g, '')
      // bell, backspace, vertical tab, form feed
      .replace(/[]/g, '')
  );
}
/* eslint-enable no-control-regex */

/**
 * Pull the OAuth authorize URL out of the (ANSI-stripped) login output so the
 * dashboard can render it as a one-click link — many users don't realise the URL
 * is buried in the terminal output. The gateway widens the PTY so the URL stays
 * on a single line, making this a clean single-match extraction.
 */
function extractAuthUrl(clean: string): string | null {
  const urls = clean.match(/https?:\/\/[^\s"'<>)\]]+/g);
  if (!urls) return null;
  const oauth = urls.find((u) => /oauth|authorize|auth\.|\/cai\//i.test(u));
  const pick = oauth ?? urls.reduce((a, b) => (b.length > a.length ? b : a));
  return pick.replace(/[.,)\]]+$/, ''); // drop trailing punctuation the TUI may append
}

export type LoginRuntime = 'claude' | 'codex' | 'gemini' | 'antigravity' | 'grok';

const RUNTIME_LABELS: Record<LoginRuntime, string> = {
  claude: 'Claude',
  codex: 'Codex',
  gemini: 'Gemini',
  antigravity: 'Antigravity (agy)',
  grok: 'Grok（SuperGrok 訂閱）',
};

interface Props {
  open: boolean;
  runtime: LoginRuntime;
  onClose: () => void;
  onSuccess?: () => void;
}

type UiStatus = 'idle' | 'running' | 'succeeded' | 'failed' | 'exited' | 'error';

/**
 * "Dashboard 一鍵登入" — drives a CLI's native login command in a PTY on the
 * gateway, streams the output here, and relays the user's pasted code back.
 * Shows a warning when the flow relies on a localhost callback (not completable
 * from a remote dashboard).
 */
export function CliLoginModal({ open, runtime, onClose, onSuccess }: Props) {
  const intl = useIntl();
  const [output, setOutput] = useState('');
  const [status, setStatus] = useState<UiStatus>('idle');
  const [remoteSafe, setRemoteSafe] = useState(true);
  const [hint, setHint] = useState('');
  const [program, setProgram] = useState('');
  const [input, setInput] = useState('');
  const [errMsg, setErrMsg] = useState<string | null>(null);
  const [registerMsg, setRegisterMsg] = useState<string | null>(null);
  const outRef = useRef<HTMLPreElement>(null);
  const sidRef = useRef<string | null>(null);

  // Derive a readable transcript + the one-click auth URL from the raw stream.
  const clean = useMemo(() => stripAnsi(output), [output]);
  const authUrl = useMemo(() => extractAuthUrl(clean), [clean]);

  // Start the login session when the modal opens.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setOutput('');
    setStatus('running');
    setErrMsg(null);
    setRegisterMsg(null);
    setInput('');
    sidRef.current = null;
    api.auth
      .cliLoginStart(runtime)
      .then((r) => {
        if (cancelled) return;
        sidRef.current = r.session_id;
        setRemoteSafe(r.remote_safe);
        setHint(r.hint);
        setProgram(r.program);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setStatus('error');
        setErrMsg(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [open, runtime]);

  // Stream output + terminal status from the gateway event bus.
  useEffect(() => {
    if (!open) return;
    const offOut = client.subscribe('auth.cli_login.output', (p) => {
      const pl = p as { session_id: string; data: string };
      if (pl.session_id !== sidRef.current) return;
      setOutput((o) => (o + pl.data).slice(-20000));
    });
    const offStatus = client.subscribe('auth.cli_login.status', (p) => {
      const pl = p as { session_id: string; status: 'succeeded' | 'failed' | 'exited' };
      if (pl.session_id !== sidRef.current) return;
      setStatus(pl.status);
      if (pl.status === 'succeeded') {
        const sid = sidRef.current;
        // Register the account the login produced (the CLI only PRINTS its token),
        // then refresh the parent so it appears in the account list.
        if (sid) {
          api.auth
            .cliLoginFinalize(sid)
            .then((r) =>
              setRegisterMsg(
                r.registered
                  ? '帳號已加入'
                  : `登入成功，但未自動加入帳號${r.reason ? `（${r.reason}）` : ''}`,
              ),
            )
            .catch((e: unknown) =>
              setRegisterMsg(`帳號註冊失敗：${e instanceof Error ? e.message : String(e)}`),
            )
            .finally(() => onSuccess?.());
        } else {
          onSuccess?.();
        }
      }
    });
    return () => {
      offOut();
      offStatus();
    };
  }, [open, onSuccess]);

  // Auto-scroll the terminal.
  useEffect(() => {
    if (outRef.current) outRef.current.scrollTop = outRef.current.scrollHeight;
  }, [clean]);

  const sendInput = async () => {
    if (!sidRef.current || status !== 'running') return;
    try {
      await api.auth.cliLoginInput(sidRef.current, `${input}\r`);
      setInput('');
    } catch (e) {
      setErrMsg(e instanceof Error ? e.message : String(e));
    }
  };

  const handleClose = async () => {
    if (sidRef.current && status === 'running') {
      try {
        await api.auth.cliLoginCancel(sidRef.current);
      } catch {
        /* best-effort */
      }
    }
    onClose();
  };

  const StatusBadge = () => {
    if (status === 'succeeded')
      return (
        <span className="inline-flex items-center gap-1 text-sm text-success">
          <CheckCircle2 className="h-4 w-4" /> 登入成功
        </span>
      );
    if (status === 'failed' || status === 'error')
      return (
        <span className="inline-flex items-center gap-1 text-sm text-destructive">
          <XCircle className="h-4 w-4" /> {status === 'error' ? errMsg ?? '啟動失敗' : '登入失敗'}
        </span>
      );
    if (status === 'exited')
      return <span className="text-sm text-muted-foreground">流程已結束（未偵測到成功訊號）</span>;
    return (
      <span className="inline-flex items-center gap-1 text-sm text-muted-foreground">
        <Loader2 className="h-4 w-4 animate-spin" /> 進行中…
      </span>
    );
  };

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) void handleClose(); }}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{`${RUNTIME_LABELS[runtime]} 一鍵登入`}</DialogTitle>
        </DialogHeader>
        <div className="space-y-3">
          {!remoteSafe && (
            <div className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
              <span>
                此 CLI 使用 localhost 回呼登入：僅在「Dashboard 與瀏覽器在同一台機器」（自架）可完成。
                遠端 Cloud 請改用 API key。
              </span>
            </div>
          )}
          {hint && <p className="text-xs text-muted-foreground">{hint}</p>}

          {/* Docker deployment caveat — grok's device-code login writes into
              whichever ~/.grok the gateway process sees, which is the
              container's volume when the gateway runs in Docker. */}
          {runtime === 'grok' && (
            <div className="flex items-start gap-2 rounded-lg border border-surface-border bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
              <Info className="mt-0.5 h-4 w-4 shrink-0" />
              <span>{intl.formatMessage({ id: 'cliLogin.grok.dockerHint' })}</span>
            </div>
          )}

          {/* One-click auth link — surfaces the URL buried in the CLI output. */}
          {authUrl && status === 'running' && (
            <div className="space-y-1.5 rounded-lg border border-warning/30 bg-warning/5 p-3">
              <p className="text-xs font-medium text-muted-foreground">
                ① 點此開啟授權網址 → 完成授權後複製驗證碼 → ② 貼到下方按 Enter
              </p>
              <a
                href={authUrl}
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center gap-2 rounded-lg bg-brand px-3 py-2 text-sm font-medium text-brand-foreground transition hover:bg-brand/90"
              >
                <ExternalLink className="h-4 w-4" /> 開啟授權網址
              </a>
              <p className="select-all break-all font-mono text-[10px] text-muted-foreground">{authUrl}</p>
            </div>
          )}

          {program && <p className="font-mono text-[11px] text-muted-foreground">$ {program} …</p>}

          {/* intentional dark terminal surface */}
          <pre
            ref={outRef}
            className="h-48 overflow-auto whitespace-pre-wrap break-all rounded-lg border border-surface-border bg-stone-950/90 p-3 font-mono text-[12px] leading-relaxed text-stone-100"
          >
            {clean.trim() || '啟動登入程序中…'}
          </pre>

          <div className="flex items-center gap-2">
            <Input
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !isImeComposing(e)) {
                  e.preventDefault();
                  void sendInput();
                }
              }}
              placeholder="貼上驗證碼 / 輸入回應後按 Enter"
              disabled={status !== 'running'}
              autoComplete="off"
              spellCheck={false}
            />
            <Button
              variant="outline"
              onClick={() => void sendInput()}
              disabled={status !== 'running'}
              title="送出"
            >
              <SendHorizonal className="h-4 w-4" />
            </Button>
          </div>

          {registerMsg && status === 'succeeded' && (
            <p className="text-xs text-muted-foreground">{registerMsg}</p>
          )}

          <div className="flex items-center justify-between pt-1">
            <StatusBadge />
            <div className="flex gap-2">
              <Button variant="outline" onClick={() => void handleClose()}>
                {status === 'running' ? '取消' : '關閉'}
              </Button>
              {status === 'succeeded' && (
                <Button variant="default" onClick={onClose}>
                  完成
                </Button>
              )}
            </div>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
