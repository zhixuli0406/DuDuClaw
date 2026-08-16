// DuDuClaw VS Code extension — a thin client for a self-hosted DuDuClaw
// gateway.
//
// Architecture note (Origin gate): the gateway rejects browser Origins it
// doesn't know, and VS Code webviews carry a RANDOM `vscode-webview://<uuid>`
// Origin — unallowlistable. So ALL sockets live in the extension host
// (Node `ws` sends no Origin header ⇒ allowed as a non-browser client), and
// the webview is pure UI talking to the host via postMessage. A side benefit:
// the JWT never enters the webview at all.
import * as vscode from 'vscode';
import WebSocket from 'ws';

const SECRET_ACCESS = 'duduclaw.accessToken';
const SECRET_REFRESH = 'duduclaw.refreshToken';

function gatewayUrl(): string {
  const raw =
    vscode.workspace.getConfiguration('duduclaw').get<string>('gatewayUrl') ??
    'http://127.0.0.1:18789';
  return raw.replace(/\/+$/, '');
}

async function apiPost(path: string, body: unknown): Promise<Response> {
  return fetch(`${gatewayUrl()}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
}

async function doLogin(context: vscode.ExtensionContext): Promise<boolean> {
  const email = await vscode.window.showInputBox({
    prompt: `DuDuClaw 登入 Email（${gatewayUrl()}）`,
    ignoreFocusOut: true,
  });
  if (!email) return false;
  const password = await vscode.window.showInputBox({
    prompt: 'DuDuClaw 密碼',
    password: true,
    ignoreFocusOut: true,
  });
  if (!password) return false;

  let res: Response;
  try {
    res = await apiPost('/api/login', { email, password });
  } catch (e) {
    vscode.window.showErrorMessage(`DuDuClaw：連不上 gateway（${gatewayUrl()}）— ${e}`);
    return false;
  }
  const data = (await res.json().catch(() => ({}))) as {
    access_token?: string;
    refresh_token?: string;
    error?: string;
  };
  if (!res.ok || !data.access_token) {
    vscode.window.showErrorMessage(`DuDuClaw：登入失敗 — ${data.error ?? res.status}`);
    return false;
  }
  await context.secrets.store(SECRET_ACCESS, data.access_token);
  if (data.refresh_token) await context.secrets.store(SECRET_REFRESH, data.refresh_token);
  vscode.window.showInformationMessage('DuDuClaw：登入成功 🐾');
  return true;
}

async function refreshAccessToken(
  context: vscode.ExtensionContext
): Promise<string | undefined> {
  const rt = await context.secrets.get(SECRET_REFRESH);
  if (!rt) return undefined;
  try {
    const res = await apiPost('/api/refresh', { refresh_token: rt });
    if (!res.ok) return undefined;
    const data = (await res.json()) as { access_token?: string; refresh_token?: string };
    if (!data.access_token) return undefined;
    await context.secrets.store(SECRET_ACCESS, data.access_token);
    if (data.refresh_token) await context.secrets.store(SECRET_REFRESH, data.refresh_token);
    return data.access_token;
  } catch {
    return undefined;
  }
}

type Post = (msg: unknown) => void;

/** Host-side gateway sockets: WebChat chat stream + dashboard JSON-RPC. */
class GatewayClient {
  private chat: WebSocket | undefined;
  private rpcSock: WebSocket | undefined;
  private rpcReady: Promise<void> | undefined;
  private rpcSeq = 0;
  private pending = new Map<string, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();

  constructor(
    private readonly ctx: vscode.ExtensionContext,
    private readonly post: Post
  ) {}

  dispose(): void {
    try { this.chat?.close(); } catch { /* noop */ }
    try { this.rpcSock?.close(); } catch { /* noop */ }
    this.chat = undefined;
    this.rpcSock = undefined;
    this.rpcReady = undefined;
  }

  private wsUrl(path: string): string {
    return gatewayUrl().replace(/^http/, 'ws') + path;
  }

  private async token(): Promise<string | undefined> {
    return this.ctx.secrets.get(SECRET_ACCESS);
  }

  // ── chat (WebChat protocol) ──
  private async ensureChat(retried = false): Promise<WebSocket> {
    if (this.chat && this.chat.readyState === WebSocket.OPEN) return this.chat;
    const token = await this.token();
    if (!token) throw new Error('not-authed');
    const sock = new WebSocket(this.wsUrl('/ws/chat'));
    this.chat = sock;
    sock.on('message', async (raw: Buffer) => {
      let m: { type?: string; message?: string };
      try { m = JSON.parse(raw.toString('utf8')); } catch { return; }
      if (
        m.type === 'error' &&
        /auth|token|jwt|unauthor/i.test(m.message ?? '') &&
        !retried
      ) {
        const t = await refreshAccessToken(this.ctx);
        sock.close();
        this.chat = undefined;
        if (t) {
          try { await this.ensureChat(true); } catch { this.post({ type: 'auth-required' }); }
        } else {
          this.post({ type: 'auth-required' });
        }
        return;
      }
      this.post({ type: 'chat', frame: m });
    });
    sock.on('close', () => {
      if (this.chat === sock) this.chat = undefined;
      this.post({ type: 'chat-closed' });
    });
    sock.on('error', () => { /* close follows */ });
    await new Promise<void>((resolve, reject) => {
      sock.once('open', () => resolve());
      sock.once('error', (e: Error) => reject(e));
    });
    sock.send(JSON.stringify({ type: 'auth', token }));
    return sock;
  }

  async sendChat(text: string): Promise<void> {
    const sock = await this.ensureChat();
    sock.send(JSON.stringify({ type: 'user_message', content: text }));
  }

  // ── dashboard JSON-RPC ──
  private async ensureRpc(retried = false): Promise<void> {
    if (this.rpcSock && this.rpcSock.readyState === WebSocket.OPEN && this.rpcReady) {
      return this.rpcReady;
    }
    const token = await this.token();
    if (!token) throw new Error('not-authed');
    const sock = new WebSocket(this.wsUrl('/ws'));
    this.rpcSock = sock;
    // The gateway speaks its own `WsFrame` protocol (see
    // `crates/duduclaw-gateway/src/protocol.rs`), NOT JSON-RPC 2.0: frames are
    // tagged `{"type":"req"|"res"|"event"}`, a reply carries `ok` + `payload`
    // (not `result`), and `error` is a bare JSON value — usually a string.
    // Server-pushed `event` frames have no `id` and must be ignored here.
    sock.on('message', (raw: Buffer) => {
      let f: {
        type?: string;
        id?: unknown;
        ok?: boolean;
        payload?: unknown;
        error?: unknown;
      };
      try { f = JSON.parse(raw.toString('utf8')); } catch { return; }
      if (f.type !== 'res' || f.id == null) return;
      const p = this.pending.get(String(f.id));
      if (!p) return;
      this.pending.delete(String(f.id));
      if (f.ok === false) {
        const msg = typeof f.error === 'string' ? f.error : JSON.stringify(f.error);
        p.reject(new Error(msg || 'request failed'));
      } else {
        p.resolve(f.payload);
      }
    });
    sock.on('close', () => {
      if (this.rpcSock === sock) { this.rpcSock = undefined; this.rpcReady = undefined; }
      for (const [, p] of this.pending) p.reject(new Error('connection closed'));
      this.pending.clear();
    });
    sock.on('error', () => { /* close follows */ });
    await new Promise<void>((resolve, reject) => {
      sock.once('open', () => resolve());
      sock.once('error', (e: Error) => reject(e));
    });
    this.rpcReady = this.rawRpc('connect', { jwt: token }).then(
      () => undefined,
      async (e: Error) => {
        if (retried) throw e;
        const t = await refreshAccessToken(this.ctx);
        sock.close();
        if (!t) { this.post({ type: 'auth-required' }); throw e; }
        return this.ensureRpc(true);
      }
    );
    return this.rpcReady;
  }

  private rawRpc(method: string, params: unknown): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const id = String(++this.rpcSeq);
      this.pending.set(id, { resolve, reject });
      // `type: 'req'` is required — an untagged frame fails the gateway's
      // `WsFrame` deserialization, which it treats as a failed handshake and
      // closes the socket ("WebSocket auth failed"), surfacing here as the
      // misleading 'connection closed'.
      this.rpcSock?.send(JSON.stringify({ type: 'req', id, method, params }));
      setTimeout(() => {
        if (this.pending.delete(id)) reject(new Error(`${method} timeout`));
      }, 15000);
    });
  }

  async rpc(method: string, params: unknown): Promise<unknown> {
    await this.ensureRpc();
    return this.rawRpc(method, params);
  }
}

class DuduPanelProvider implements vscode.WebviewViewProvider {
  private view: vscode.WebviewView | undefined;
  private client: GatewayClient | undefined;

  constructor(private readonly context: vscode.ExtensionContext) {}

  private post(msg: unknown): void {
    this.view?.webview.postMessage(msg);
  }

  resetClient(): void {
    this.client?.dispose();
    this.client = new GatewayClient(this.context, (m) => this.post(m));
  }

  async pushInit(): Promise<void> {
    if (!this.view) return;
    this.resetClient();
    const authed = Boolean(await this.context.secrets.get(SECRET_ACCESS));
    this.post({ type: 'init', authed, gateway: gatewayUrl() });
  }

  private async loadApprovals(): Promise<void> {
    try {
      const res = (await this.client?.rpc('approvals.list', {})) as
        | { approvals?: unknown[] }
        | undefined;
      this.post({ type: 'approvals', items: res?.approvals ?? [] });
    } catch (e) {
      this.post({ type: 'approvals-error', message: String(e).slice(0, 200) });
    }
  }

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = { enableScripts: true };
    view.webview.html = renderHtml();
    view.webview.onDidReceiveMessage(async (msg: { type: string; [k: string]: unknown }) => {
      switch (msg.type) {
        case 'ready':
          await this.pushInit();
          break;
        case 'login':
          if (await doLogin(this.context)) await this.pushInit();
          break;
        case 'chat-send':
          try {
            await this.client?.sendChat(String(msg.text ?? ''));
          } catch (e) {
            if (String(e).includes('not-authed')) this.post({ type: 'auth-required' });
            else this.post({ type: 'chat', frame: { type: 'error', message: String(e).slice(0, 200) } });
          }
          break;
        case 'approvals-load':
          await this.loadApprovals();
          break;
        case 'approvals-decide':
          try {
            await this.client?.rpc('approvals.decide', {
              id: msg.id,
              approve: Boolean(msg.approve),
            });
          } catch (e) {
            this.post({ type: 'approvals-error', message: String(e).slice(0, 200) });
          }
          await this.loadApprovals();
          break;
      }
    });
    view.onDidDispose(() => {
      this.client?.dispose();
      this.view = undefined;
    });
  }
}

export function activate(context: vscode.ExtensionContext): void {
  const provider = new DuduPanelProvider(context);
  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider('duduclaw.panel', provider, {
      webviewOptions: { retainContextWhenHidden: true },
    }),
    vscode.commands.registerCommand('duduclaw.login', async () => {
      if (await doLogin(context)) await provider.pushInit();
    }),
    vscode.commands.registerCommand('duduclaw.logout', async () => {
      await context.secrets.delete(SECRET_ACCESS);
      await context.secrets.delete(SECRET_REFRESH);
      await provider.pushInit();
      vscode.window.showInformationMessage('DuDuClaw：已登出');
    }),
    vscode.workspace.onDidChangeConfiguration(async (e) => {
      if (e.affectsConfiguration('duduclaw.gatewayUrl')) await provider.pushInit();
    })
  );
}

export function deactivate(): void {}

/** Pure-UI webview: no sockets, no tokens — everything relays via the host. */
function renderHtml(): string {
  return /* html */ `<!DOCTYPE html>
<html lang="zh-Hant">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy"
  content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline';">
<style>
  :root { color-scheme: light dark; }
  body { font-family: var(--vscode-font-family); color: var(--vscode-foreground);
         background: var(--vscode-sideBar-background); margin: 0; padding: 0;
         display: flex; flex-direction: column; height: 100vh; font-size: 13px; }
  .tabs { display: flex; border-bottom: 1px solid var(--vscode-panel-border); }
  .tab { padding: 6px 12px; cursor: pointer; opacity: .7; }
  .tab.active { opacity: 1; border-bottom: 2px solid var(--vscode-focusBorder); }
  .page { flex: 1; overflow-y: auto; padding: 8px; display: none; }
  .page.active { display: block; }
  #chatlog .msg { margin: 6px 0; padding: 6px 9px; border-radius: 8px; white-space: pre-wrap; word-break: break-word; }
  .msg.user { background: var(--vscode-input-background); }
  .msg.bot  { background: var(--vscode-editorWidget-background); }
  .msg.sys  { opacity: .6; font-size: 12px; }
  #composer { display: flex; gap: 6px; padding: 8px; border-top: 1px solid var(--vscode-panel-border); }
  #input { flex: 1; background: var(--vscode-input-background); color: var(--vscode-input-foreground);
           border: 1px solid var(--vscode-input-border, transparent); border-radius: 4px; padding: 6px; resize: none; }
  button { background: var(--vscode-button-background); color: var(--vscode-button-foreground);
           border: 0; border-radius: 4px; padding: 5px 11px; cursor: pointer; }
  button.secondary { background: var(--vscode-button-secondaryBackground); color: var(--vscode-button-secondaryForeground); }
  .card { border: 1px solid var(--vscode-panel-border); border-radius: 8px; padding: 8px; margin: 8px 0; }
  .card .meta { opacity: .7; font-size: 12px; margin-bottom: 4px; }
  .card .actions { display: flex; gap: 6px; margin-top: 8px; }
  .banner { padding: 10px; text-align: center; }
  .status { font-size: 11px; opacity: .6; padding: 2px 8px; }
</style>
</head>
<body>
<div class="tabs">
  <div class="tab active" data-page="chat">💬 對話</div>
  <div class="tab" data-page="approvals">✅ 審批 <span id="apcount"></span></div>
</div>
<div id="banner" class="banner" style="display:none">
  <p>尚未連線到 DuDuClaw gateway。</p>
  <button id="loginBtn">登入</button>
</div>
<div id="page-chat" class="page active"><div id="chatlog"></div></div>
<div id="page-approvals" class="page">
  <div style="display:flex;justify-content:flex-end"><button id="apRefresh" class="secondary">重新整理</button></div>
  <div id="aplist"></div>
</div>
<div id="composer">
  <textarea id="input" rows="2" placeholder="跟你的 AI 員工說話…（Enter 送出，Shift+Enter 換行）"></textarea>
  <button id="send">送出</button>
</div>
<div class="status" id="status"></div>
<script>
const vscode = acquireVsCodeApi();
let AUTHED = false, botBuf = null;
const $ = (id) => document.getElementById(id);
const log = $('chatlog');

function setStatus(t) { $('status').textContent = t; }
function bubble(cls, text) {
  const d = document.createElement('div');
  d.className = 'msg ' + cls; d.textContent = text;
  log.appendChild(d);
  $('page-chat').scrollTop = $('page-chat').scrollHeight;
  return d;
}

document.querySelectorAll('.tab').forEach(t => t.addEventListener('click', () => {
  document.querySelectorAll('.tab').forEach(x => x.classList.remove('active'));
  document.querySelectorAll('.page').forEach(x => x.classList.remove('active'));
  t.classList.add('active');
  $('page-' + t.dataset.page).classList.add('active');
  if (t.dataset.page === 'approvals') vscode.postMessage({ type: 'approvals-load' });
}));

function sendMessage() {
  const text = $('input').value.trim();
  if (!text) return;
  if (!AUTHED) { vscode.postMessage({ type: 'login' }); return; }
  bubble('user', text);
  vscode.postMessage({ type: 'chat-send', text });
  $('input').value = '';
}
$('send').addEventListener('click', sendMessage);
$('input').addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); sendMessage(); }
});
$('apRefresh').addEventListener('click', () => vscode.postMessage({ type: 'approvals-load' }));
$('loginBtn').addEventListener('click', () => vscode.postMessage({ type: 'login' }));

function renderApprovals(items) {
  const list = $('aplist');
  $('apcount').textContent = items.length ? '(' + items.length + ')' : '';
  list.textContent = '';
  if (!items.length) {
    const p = document.createElement('p');
    p.style.opacity = '.6';
    p.textContent = '目前沒有待審批項目。';
    list.appendChild(p);
    return;
  }
  for (const it of items) {
    const card = document.createElement('div'); card.className = 'card';
    const meta = document.createElement('div'); meta.className = 'meta';
    meta.textContent = (it.agent_id || '') + ' · ' + (it.action_kind || it.kind || '') + (it.expires_at ? ' · 到期 ' + it.expires_at : '');
    const body = document.createElement('div');
    body.textContent = it.summary || it.description || it.action || JSON.stringify(it).slice(0, 300);
    const actions = document.createElement('div'); actions.className = 'actions';
    const ok = document.createElement('button'); ok.textContent = '同意';
    const no = document.createElement('button'); no.textContent = '拒絕'; no.className = 'secondary';
    ok.onclick = () => vscode.postMessage({ type: 'approvals-decide', id: it.id, approve: true });
    no.onclick = () => vscode.postMessage({ type: 'approvals-decide', id: it.id, approve: false });
    actions.append(ok, no);
    card.append(meta, body, actions);
    list.appendChild(card);
  }
}

window.addEventListener('message', (ev) => {
  const m = ev.data;
  switch (m.type) {
    case 'init':
      AUTHED = m.authed;
      $('banner').style.display = AUTHED ? 'none' : 'block';
      if (AUTHED) vscode.postMessage({ type: 'approvals-load' });
      break;
    case 'auth-required':
      AUTHED = false;
      $('banner').style.display = 'block';
      break;
    case 'chat': {
      const f = m.frame;
      if (f.type === 'session_info') setStatus('連上 ' + (f.agent_name || 'AI 員工'));
      else if (f.type === 'assistant_chunk') {
        if (!botBuf) botBuf = bubble('bot', '');
        botBuf.textContent += f.content;
      } else if (f.type === 'progress') {
        if (f.kind !== 'keepalive') setStatus('⋯ ' + String(f.content).slice(0, 120));
      } else if (f.type === 'assistant_done') {
        if (botBuf) { botBuf.textContent = f.content; botBuf = null; }
        else bubble('bot', f.content);
        setStatus('');
      } else if (f.type === 'error') {
        botBuf = null;
        bubble('sys', '⚠ ' + f.message);
      }
      break;
    }
    case 'chat-closed': setStatus('對話連線中斷（送訊息時會自動重連）'); break;
    case 'approvals': renderApprovals(m.items); break;
    case 'approvals-error': $('aplist').textContent = '審批載入失敗：' + m.message; break;
  }
});
vscode.postMessage({ type: 'ready' });
</script>
</body>
</html>`;
}
