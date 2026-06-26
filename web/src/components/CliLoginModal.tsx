import { useEffect, useRef, useState } from 'react';
import { AlertTriangle, CheckCircle2, XCircle, Loader2, SendHorizonal } from 'lucide-react';
import { api } from '@/lib/api';
import { client } from '@/lib/ws-client';
import { Dialog, inputClass, buttonPrimary, buttonSecondary } from '@/components/shared/Dialog';

export type LoginRuntime = 'claude' | 'codex' | 'gemini' | 'antigravity';

const RUNTIME_LABELS: Record<LoginRuntime, string> = {
  claude: 'Claude',
  codex: 'Codex',
  gemini: 'Gemini',
  antigravity: 'Antigravity (agy)',
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
  const [output, setOutput] = useState('');
  const [status, setStatus] = useState<UiStatus>('idle');
  const [remoteSafe, setRemoteSafe] = useState(true);
  const [hint, setHint] = useState('');
  const [program, setProgram] = useState('');
  const [input, setInput] = useState('');
  const [errMsg, setErrMsg] = useState<string | null>(null);
  const outRef = useRef<HTMLPreElement>(null);
  const sidRef = useRef<string | null>(null);

  // Start the login session when the modal opens.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setOutput('');
    setStatus('running');
    setErrMsg(null);
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
      if (pl.status === 'succeeded') onSuccess?.();
    });
    return () => {
      offOut();
      offStatus();
    };
  }, [open, onSuccess]);

  // Auto-scroll the terminal.
  useEffect(() => {
    if (outRef.current) outRef.current.scrollTop = outRef.current.scrollHeight;
  }, [output]);

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
        <span className="inline-flex items-center gap-1 text-sm text-emerald-600 dark:text-emerald-400">
          <CheckCircle2 className="h-4 w-4" /> 登入成功
        </span>
      );
    if (status === 'failed' || status === 'error')
      return (
        <span className="inline-flex items-center gap-1 text-sm text-rose-600 dark:text-rose-400">
          <XCircle className="h-4 w-4" /> {status === 'error' ? errMsg ?? '啟動失敗' : '登入失敗'}
        </span>
      );
    if (status === 'exited')
      return <span className="text-sm text-stone-500">流程已結束（未偵測到成功訊號）</span>;
    return (
      <span className="inline-flex items-center gap-1 text-sm text-stone-500">
        <Loader2 className="h-4 w-4 animate-spin" /> 進行中…
      </span>
    );
  };

  return (
    <Dialog open={open} onClose={handleClose} title={`${RUNTIME_LABELS[runtime]} 一鍵登入`}>
      <div className="space-y-3">
        {!remoteSafe && (
          <div className="flex items-start gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-800 dark:text-amber-200">
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
            <span>
              此 CLI 使用 localhost 回呼登入：僅在「Dashboard 與瀏覽器在同一台機器」（自架）可完成。
              遠端 Cloud 請改用 API key。
            </span>
          </div>
        )}
        {hint && <p className="text-xs text-stone-500 dark:text-stone-400">{hint}</p>}
        {program && (
          <p className="font-mono text-[11px] text-stone-400">$ {program} …</p>
        )}

        <pre
          ref={outRef}
          className="h-64 overflow-auto rounded-lg border border-stone-300/50 bg-stone-950/90 p-3 font-mono text-[12px] leading-relaxed text-stone-100 dark:border-white/10"
        >
          {output || '啟動登入程序中…'}
        </pre>

        <div className="flex items-center gap-2">
          <input
            className={inputClass}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                void sendInput();
              }
            }}
            placeholder="貼上驗證碼 / 輸入回應後按 Enter"
            disabled={status !== 'running'}
            autoComplete="off"
            spellCheck={false}
          />
          <button
            className={buttonSecondary}
            onClick={() => void sendInput()}
            disabled={status !== 'running'}
            title="送出"
          >
            <SendHorizonal className="h-4 w-4" />
          </button>
        </div>

        <div className="flex items-center justify-between pt-1">
          <StatusBadge />
          <div className="flex gap-2">
            <button className={buttonSecondary} onClick={handleClose}>
              {status === 'running' ? '取消' : '關閉'}
            </button>
            {status === 'succeeded' && (
              <button className={buttonPrimary} onClick={onClose}>
                完成
              </button>
            )}
          </div>
        </div>
      </div>
    </Dialog>
  );
}
