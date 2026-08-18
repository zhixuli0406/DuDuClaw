// The webview panel: chat + approvals tabs, agent picker, role-aware UI.
// The webview itself is pure UI (no sockets, no tokens) — everything relays
// through `DuduPanelProvider` to the host-side `GatewayClient`.
import * as vscode from 'vscode';
import { STATE_LAST_AGENT, gatewayUrl, SECRET_ACCESS, SECRET_REFRESH } from './config';
import { doLogin, fetchMe, renderErrorMessage } from './auth';
import { GatewayClient, NOT_AUTHED_ERROR } from './rpc';
import type { AccessLevel, AgentListEntry, AgentOption, MeResponse, PanelStatusState, UserRole } from './types';

function accessLevelLabel(level: AccessLevel): string {
  switch (level) {
    case 'owner': return '完整權限';
    case 'operator': return '可操作';
    case 'viewer': return '僅檢視';
  }
}

/**
 * `agents.list` doesn't carry a per-agent access level for the caller (see
 * the comment on `AgentListEntry` in types.ts) — it only exists in
 * `/api/me`'s `bindings` array. Join by `agent_name`. Admins have no
 * bindings at all (they bypass the binding check server-side) and get
 * `accessLevel: undefined`; the webview shows no badge for them, which is
 * correct — an unbadged agent in an admin's list means "unrestricted", not
 * "unknown".
 */
function mergeAgentOptions(agents: AgentListEntry[], me: MeResponse | undefined): AgentOption[] {
  const byName = new Map<string, AccessLevel>();
  for (const b of me?.bindings ?? []) byName.set(b.agent_name, b.access_level);
  return agents.map((a) => ({
    name: a.name,
    displayName: (typeof a.display_name === 'string' && a.display_name) || a.name,
    accessLevel: byName.get(a.name),
  }));
}

export class DuduPanelProvider implements vscode.WebviewViewProvider {
  private view: vscode.WebviewView | undefined;
  private client: GatewayClient | undefined;
  private me: MeResponse | undefined;
  private agents: AgentOption[] = [];
  private selectedAgent: string | undefined;

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly onStateChange: (state: PanelStatusState) => void
  ) {}

  private post(msg: unknown): void {
    this.view?.webview.postMessage(msg);
  }

  private currentAgentOption(): AgentOption | undefined {
    return this.agents.find((a) => a.name === this.selectedAgent);
  }

  private emitStatus(): void {
    this.onStateChange({
      authed: Boolean(this.me),
      agentName: this.currentAgentOption()?.displayName,
      role: this.me?.user.role,
    });
  }

  resetClient(): void {
    this.client?.dispose();
    this.client = new GatewayClient(this.context, (m) => this.post(m));
    this.client.switchAgent(this.selectedAgent);
  }

  /** Re-authenticates, reloads role + agent list, and pushes a fresh `init`
   * frame to the webview. Called on webview ready, after login/logout, and
   * on gatewayUrl config changes. */
  async pushInit(): Promise<void> {
    if (!this.view) return;
    this.resetClient();
    const hadToken = Boolean(await this.context.secrets.get(SECRET_ACCESS));
    this.me = hadToken ? await fetchMe(this.context) : undefined;
    if (hadToken && !this.me) {
      // Token present but /api/me rejected it even after the internal
      // refresh retry — treat as logged out rather than showing a UI that
      // claims to be authed with no role/agent data behind it.
      await this.context.secrets.delete(SECRET_ACCESS);
      await this.context.secrets.delete(SECRET_REFRESH);
    }
    const authed = Boolean(this.me);
    if (authed) {
      await this.loadAgents();
    } else {
      this.agents = [];
      this.selectedAgent = undefined;
    }
    this.post({
      type: 'init',
      authed,
      gateway: gatewayUrl(),
      role: this.me?.user.role,
      agents: this.agents,
      selectedAgent: this.selectedAgent,
      // employee: hide the approvals tab outright rather than let them hit
      // the `require_manager!` wall and see "permission denied".
      showApprovals: this.me ? this.me.user.role !== 'employee' : true,
    });
    this.emitStatus();
  }

  private async loadAgents(): Promise<void> {
    try {
      const list = await this.client?.agentsList() ?? [];
      this.agents = mergeAgentOptions(list, this.me);
    } catch {
      this.agents = [];
    }
    const remembered = this.context.workspaceState.get<string>(STATE_LAST_AGENT);
    const valid = this.agents.find((a) => a.name === remembered);
    this.selectedAgent = valid?.name ?? this.agents[0]?.name;
    this.client?.switchAgent(this.selectedAgent);
  }

  /** Switches the active agent for both the chat socket (host side) and the
   * webview display (clears the log — each agent gets its own conversation,
   * never a mixed one; see `GatewayClient.switchAgent` for how cross-talk
   * from an in-flight previous-agent reply is prevented). */
  async switchAgent(agentName: string): Promise<void> {
    if (!this.agents.some((a) => a.name === agentName) || agentName === this.selectedAgent) return;
    this.selectedAgent = agentName;
    await this.context.workspaceState.update(STATE_LAST_AGENT, agentName);
    this.client?.switchAgent(agentName);
    this.post({ type: 'agent-switched', agent: agentName });
    this.emitStatus();
  }

  /** Backing implementation for the `duduclaw.switchAgent` command. */
  async pickAgentViaQuickPick(): Promise<void> {
    if (!this.me) {
      vscode.window.showInformationMessage('DuDuClaw：請先登入。');
      return;
    }
    if (!this.agents.length) {
      vscode.window.showInformationMessage('DuDuClaw：目前沒有可切換的 AI 員工。');
      return;
    }
    const pick = await vscode.window.showQuickPick(
      this.agents.map((a) => ({
        label: a.name === this.selectedAgent ? `$(check) ${a.displayName}` : a.displayName,
        description: a.accessLevel ? accessLevelLabel(a.accessLevel) : undefined,
        agentName: a.name,
      })),
      { placeHolder: '選擇要對話的 AI 員工' }
    );
    if (pick) await this.switchAgent(pick.agentName);
  }

  private async loadApprovals(): Promise<void> {
    if (this.me?.user.role === 'employee') {
      // Defense in depth: the tab is hidden client-side too, but a stray
      // message (e.g. sent before `init` lands) must not reach the server.
      this.post({ type: 'approvals', items: [] });
      return;
    }
    try {
      const res = (await this.client?.rpc('approvals.list', {})) as
        | { approvals?: unknown[] }
        | undefined;
      this.post({ type: 'approvals', items: res?.approvals ?? [] });
    } catch (e) {
      this.post({ type: 'approvals-error', message: renderErrorMessage(e) });
    }
  }

  /**
   * Task tab (design doc §4 P1) — every role can see it (unlike approvals),
   * always scoped to the currently selected AI employee. `tasks.list`
   * requires an `agent_id` for non-admins server-side (`check_agent_filter!`
   * in handlers.rs); this client always has one when authed, so the "no
   * agent selected" branch below is really just a defensive empty-state, not
   * a permission dodge.
   */
  private async loadTasks(): Promise<void> {
    if (!this.selectedAgent) {
      this.post({ type: 'tasks', items: [] });
      return;
    }
    try {
      const items = (await this.client?.tasksList(this.selectedAgent)) ?? [];
      this.post({ type: 'tasks', items });
    } catch (e) {
      this.post({ type: 'tasks-error', message: renderErrorMessage(e) });
    }
  }

  /**
   * 交辦／想一想 (design doc §4 P1): creates a goal-loop task for the
   * currently selected agent via `tasks.goal_create`. `planFirst` mirrors
   * the dashboard AssignSheet's third mode — the server generates a plan and
   * parks the task `needs_human` instead of dispatching immediately.
   */
  private async createGoal(text: string, planFirst: boolean): Promise<void> {
    if (!this.selectedAgent) {
      this.post({ type: 'goal-error', message: '請先選擇 AI 員工。' });
      return;
    }
    try {
      const res = await this.client?.goalCreate(this.selectedAgent, text, planFirst);
      this.post({
        type: 'goal-created',
        planFirst: Boolean(res?.plan_first),
        taskId: res?.task?.id,
      });
    } catch (e) {
      if (String(e).includes(NOT_AUTHED_ERROR)) this.post({ type: 'auth-required' });
      else this.post({ type: 'goal-error', message: renderErrorMessage(e) });
    }
  }

  /** needs_human resolution (重試／標記完成／放棄) — same RPC shape and the
   * same three actions as the dashboard's shared `NeedsHumanActions`
   * component. Always reloads the task list afterwards so the card reflects
   * the new status (or disappears once resolved). */
  private async decideTask(taskId: string, action: 'retry' | 'done' | 'abort', note?: string): Promise<void> {
    try {
      await this.client?.goalDecide(taskId, action, note);
    } catch (e) {
      this.post({ type: 'tasks-error', message: renderErrorMessage(e) });
    }
    await this.loadTasks();
  }

  /**
   * `duduclaw.askAboutSelection` (design doc §4 P1 "編輯器上下文"): the
   * command handler in extension.ts builds the DATA-fenced block (file path
   * + line range + capped selection text) and hands it here to relay into
   * the webview as a removable "attached context" chip — the block is only
   * actually sent once the user hits Send (see the webview script's
   * `sendMessage`), combined with whatever they type.
   */
  attachEditorContext(block: string, label: string): void {
    this.post({ type: 'attach-context', block, label });
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
            if (String(e).includes(NOT_AUTHED_ERROR)) this.post({ type: 'auth-required' });
            else this.post({ type: 'chat', frame: { type: 'error', message: renderErrorMessage(e) } });
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
            this.post({ type: 'approvals-error', message: renderErrorMessage(e) });
          }
          await this.loadApprovals();
          break;
        case 'switch-agent':
          await this.switchAgent(String(msg.agent ?? ''));
          break;
        case 'goal-create':
          await this.createGoal(String(msg.text ?? ''), Boolean(msg.planFirst));
          break;
        case 'tasks-load':
          await this.loadTasks();
          break;
        case 'task-decide': {
          const action = msg.action;
          if (action === 'retry' || action === 'done' || action === 'abort') {
            await this.decideTask(String(msg.taskId ?? ''), action, (msg.note as string | undefined) || undefined);
          }
          break;
        }
      }
    });
    view.onDidDispose(() => {
      this.client?.dispose();
      this.view = undefined;
    });
  }
}

/** zh-TW label for the account-wide role, used by both the status bar
 * (extension.ts) and could be reused by the webview in future — kept here
 * next to `PanelStatusState`'s producer so the two stay in sync. */
export function roleLabel(role: UserRole | undefined): string {
  switch (role) {
    case 'admin': return '管理者';
    case 'manager': return '經理';
    case 'employee': return '員工';
    default: return '';
  }
}

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
  .agentbar { display: none; align-items: center; gap: 6px; padding: 6px 8px;
              border-bottom: 1px solid var(--vscode-panel-border); }
  .agentbar select { flex: 1; background: var(--vscode-dropdown-background);
                      color: var(--vscode-dropdown-foreground);
                      border: 1px solid var(--vscode-dropdown-border, transparent);
                      border-radius: 4px; padding: 3px 6px; }
  .badge { font-size: 11px; opacity: .75; padding: 1px 7px; border-radius: 999px;
           border: 1px solid var(--vscode-panel-border); white-space: nowrap; }
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
  .card .actions { display: flex; gap: 6px; margin-top: 8px; flex-wrap: wrap; }
  .card.needs-human { border-color: var(--vscode-editorWarning-foreground, #cca700); border-width: 1.5px; }
  .card .title { font-weight: 600; }
  .card .body { font-size: 12px; opacity: .8; margin-top: 2px; white-space: pre-wrap; word-break: break-word; }
  .banner { padding: 10px; text-align: center; }
  .status { font-size: 11px; opacity: .6; padding: 2px 8px; }
  .modebar { display: flex; gap: 4px; padding: 6px 8px 0; }
  .modebtn { padding: 3px 9px; border-radius: 999px; border: 1px solid var(--vscode-panel-border);
             background: transparent; color: var(--vscode-foreground); cursor: pointer; font-size: 11px; opacity: .7; }
  .modebtn.active { opacity: 1; background: var(--vscode-button-background); color: var(--vscode-button-foreground); border-color: transparent; }
  .ctxchip { display: none; align-items: center; gap: 6px; margin: 6px 8px 0; padding: 4px 8px;
             border-radius: 6px; background: var(--vscode-editorWidget-background); font-size: 12px; }
  .ctxchip .x { cursor: pointer; opacity: .7; margin-left: auto; }
  .msg.user details { margin: 0; }
  .msg.user summary { cursor: pointer; opacity: .8; font-size: 12px; }
  .msg.user details pre { white-space: pre-wrap; word-break: break-word; margin: 4px 0 0; font-size: 12px; opacity: .9; font-family: var(--vscode-editor-font-family, monospace); }
  .msg.user .question { margin-top: 6px; }
  .noteRow { display: none; gap: 6px; margin-top: 6px; }
  .noteRow input { flex: 1; background: var(--vscode-input-background); color: var(--vscode-input-foreground);
                    border: 1px solid var(--vscode-input-border, transparent); border-radius: 4px; padding: 4px 6px; }
</style>
</head>
<body>
<div class="tabs">
  <div class="tab active" data-page="chat">💬 對話</div>
  <div class="tab" id="tabTasks" data-page="tasks">📋 任務</div>
  <div class="tab" id="tabApprovals" data-page="approvals">✅ 審批 <span id="apcount"></span></div>
</div>
<div id="agentbar" class="agentbar">
  <select id="agentSelect" aria-label="選擇 AI 員工"></select>
  <span id="agentBadge" class="badge"></span>
</div>
<div id="banner" class="banner" style="display:none">
  <p>尚未連線到 DuDuClaw gateway。</p>
  <button id="loginBtn">登入</button>
</div>
<div id="page-chat" class="page active"><div id="chatlog"></div></div>
<div id="page-tasks" class="page">
  <div style="display:flex;justify-content:flex-end"><button id="tasksRefresh" class="secondary">重新整理</button></div>
  <div id="tasklist"></div>
</div>
<div id="page-approvals" class="page">
  <div style="display:flex;justify-content:flex-end"><button id="apRefresh" class="secondary">重新整理</button></div>
  <div id="aplist"></div>
</div>
<div id="modebar" class="modebar">
  <button type="button" class="modebtn active" data-mode="ask">💬 問一問</button>
  <button type="button" class="modebtn" data-mode="delegate">📌 交辦</button>
  <button type="button" class="modebtn" data-mode="plan">🧭 想一想</button>
</div>
<div id="ctxchip" class="ctxchip">
  <span id="ctxlabel"></span>
  <span class="x" id="ctxremove" title="移除附加內容">✕</span>
</div>
<div id="composer">
  <textarea id="input" rows="2" placeholder="跟你的 AI 員工說話…（Enter 送出，Shift+Enter 換行）"></textarea>
  <button id="send">送出</button>
</div>
<div class="status" id="status"></div>
<script>
const vscode = acquireVsCodeApi();
let AUTHED = false, botBuf = null, AGENTS = [], SELECTED_AGENT = undefined;
let MODE = 'ask';
let PENDING_CONTEXT = null;
const $ = (id) => document.getElementById(id);
const log = $('chatlog');

const STATUS_LABEL = {
  todo: '排隊中', pending: '排隊中', in_progress: '執行中', review: '驗收中',
  revising: '修正中', needs_human: '等你決定', done: '已完成', cancelled: '已放棄',
  failed: '失敗', blocked: '被擋住',
};
const PAUSE_LABEL = {
  no_progress: '卡住沒進展', budget_exhausted: '次數或時限用盡', blocked_needs_decision: '等你決策',
  infra: '系統問題', restart: '系統重啟後暫停', unknown: '需要人工確認',
};
const MODE_PLACEHOLDER = {
  ask: '跟你的 AI 員工說話…（Enter 送出，Shift+Enter 換行）',
  delegate: '描述要交辦的目標…AI 員工會自主執行到完成或卡住為止',
  plan: '描述目標，AI 員工會先擬一份計畫給你核准，才開始執行',
};

function setStatus(t) { $('status').textContent = t; }
function bubble(cls, text) {
  const d = document.createElement('div');
  d.className = 'msg ' + cls; d.textContent = text;
  log.appendChild(d);
  $('page-chat').scrollTop = $('page-chat').scrollHeight;
  return d;
}
/** Renders the "user" bubble for a send that may carry attached editor
 * context — the DATA-fenced block collapses behind a <details>/<summary>
 * (design doc §4 P1 "面板顯示時上下文區塊摺疊顯示"), the free-text question
 * (if any) renders plainly below it. */
function renderUserBubble(ctx, text) {
  const d = document.createElement('div');
  d.className = 'msg user';
  if (ctx) {
    const details = document.createElement('details');
    const summary = document.createElement('summary');
    summary.textContent = '📎 ' + ctx.label;
    const pre = document.createElement('pre');
    pre.textContent = ctx.block;
    details.append(summary, pre);
    d.appendChild(details);
  }
  if (text) {
    const p = document.createElement('div');
    p.className = ctx ? 'question' : '';
    p.textContent = text;
    d.appendChild(p);
  }
  log.appendChild(d);
  $('page-chat').scrollTop = $('page-chat').scrollHeight;
}
function clearChatLog() {
  log.textContent = '';
  botBuf = null;
  setStatus('');
}
function renderContextChip() {
  const chip = $('ctxchip');
  if (!PENDING_CONTEXT) { chip.style.display = 'none'; return; }
  chip.style.display = 'flex';
  $('ctxlabel').textContent = '📎 已附加：' + PENDING_CONTEXT.label;
}
$('ctxremove').addEventListener('click', () => { PENDING_CONTEXT = null; renderContextChip(); });

function accessLevelLabel(level) {
  switch (level) {
    case 'owner': return '完整權限';
    case 'operator': return '可操作';
    case 'viewer': return '僅檢視';
    default: return '';
  }
}

function renderAgentBar() {
  const bar = $('agentbar');
  if (!AUTHED || !AGENTS.length) { bar.style.display = 'none'; return; }
  bar.style.display = 'flex';
  const sel = $('agentSelect');
  sel.textContent = '';
  for (const a of AGENTS) {
    const opt = document.createElement('option');
    opt.value = a.name;
    opt.textContent = a.displayName + (a.accessLevel ? ' · ' + accessLevelLabel(a.accessLevel) : '');
    if (a.name === SELECTED_AGENT) opt.selected = true;
    sel.appendChild(opt);
  }
  const current = AGENTS.find(a => a.name === SELECTED_AGENT);
  $('agentBadge').textContent = current && current.accessLevel ? accessLevelLabel(current.accessLevel) : '';
}
$('agentSelect').addEventListener('change', (e) => {
  vscode.postMessage({ type: 'switch-agent', agent: e.target.value });
});

function switchTab(page) {
  document.querySelectorAll('.tab').forEach(x => x.classList.toggle('active', x.dataset.page === page));
  document.querySelectorAll('.page').forEach(x => x.classList.toggle('active', x.id === 'page-' + page));
  if (page === 'approvals') vscode.postMessage({ type: 'approvals-load' });
  if (page === 'tasks') vscode.postMessage({ type: 'tasks-load' });
}
document.querySelectorAll('.tab').forEach(t => t.addEventListener('click', () => {
  if (t.style.display === 'none') return;
  switchTab(t.dataset.page);
}));

document.querySelectorAll('.modebtn').forEach((b) => b.addEventListener('click', () => {
  MODE = b.dataset.mode;
  document.querySelectorAll('.modebtn').forEach((x) => x.classList.toggle('active', x === b));
  $('input').placeholder = MODE_PLACEHOLDER[MODE];
}));

function sendMessage() {
  const text = $('input').value.trim();
  const ctx = PENDING_CONTEXT;
  if (!text && !ctx) return;
  if (!AUTHED) { vscode.postMessage({ type: 'login' }); return; }
  const fullText = ctx ? (ctx.block + (text ? '\\n\\n' + text : '')) : text;
  renderUserBubble(ctx, text);
  if (MODE === 'ask') {
    vscode.postMessage({ type: 'chat-send', text: fullText });
  } else {
    vscode.postMessage({ type: 'goal-create', text: fullText, planFirst: MODE === 'plan' });
    bubble('sys', MODE === 'plan' ? '⋯ 正在請 AI 員工擬計畫…' : '⋯ 正在交辦…');
  }
  $('input').value = '';
  PENDING_CONTEXT = null;
  renderContextChip();
}
$('send').addEventListener('click', sendMessage);
$('input').addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); sendMessage(); }
});
$('apRefresh').addEventListener('click', () => vscode.postMessage({ type: 'approvals-load' }));
$('tasksRefresh').addEventListener('click', () => vscode.postMessage({ type: 'tasks-load' }));
$('loginBtn').addEventListener('click', () => vscode.postMessage({ type: 'login' }));

function renderTasks(items) {
  const list = $('tasklist');
  list.textContent = '';
  if (!items.length) {
    const p = document.createElement('p');
    p.style.opacity = '.6';
    p.textContent = '目前沒有任務。';
    list.appendChild(p);
    return;
  }
  for (const t of items) {
    const card = document.createElement('div');
    card.className = 'card' + (t.status === 'needs_human' ? ' needs-human' : '');
    const meta = document.createElement('div'); meta.className = 'meta';
    meta.textContent = (STATUS_LABEL[t.status] || t.status || '') +
      (t.status === 'needs_human' && t.pause_reason ? ' · ' + (PAUSE_LABEL[t.pause_reason] || t.pause_reason) : '');
    const title = document.createElement('div'); title.className = 'title';
    title.textContent = t.title || '(未命名任務)';
    const body = document.createElement('div'); body.className = 'body';
    body.textContent = (t.plan_pending || t.judge_feedback || t.description || '').slice(0, 240);
    card.append(meta, title, body);
    if (t.status === 'needs_human') {
      const actions = document.createElement('div'); actions.className = 'actions';
      const retry = document.createElement('button'); retry.textContent = '重試';
      const done = document.createElement('button'); done.className = 'secondary'; done.textContent = '標記完成';
      const abort = document.createElement('button'); abort.className = 'secondary'; abort.textContent = '放棄';
      const noteRow = document.createElement('div'); noteRow.className = 'noteRow';
      const noteInput = document.createElement('input');
      noteInput.placeholder = '給下一輪的指示（選填）';
      const noteGo = document.createElement('button'); noteGo.textContent = '送出重試';
      retry.onclick = () => { noteRow.style.display = noteRow.style.display === 'flex' ? 'none' : 'flex'; };
      noteGo.onclick = () => {
        vscode.postMessage({ type: 'task-decide', taskId: t.id, action: 'retry', note: noteInput.value });
        noteRow.style.display = 'none';
        noteInput.value = '';
      };
      done.onclick = () => vscode.postMessage({ type: 'task-decide', taskId: t.id, action: 'done' });
      abort.onclick = () => vscode.postMessage({ type: 'task-decide', taskId: t.id, action: 'abort' });
      actions.append(retry, done, abort);
      noteRow.append(noteInput, noteGo);
      card.append(actions, noteRow);
    }
    list.appendChild(card);
  }
}

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
      AGENTS = m.agents || [];
      SELECTED_AGENT = m.selectedAgent;
      $('banner').style.display = AUTHED ? 'none' : 'block';
      $('tabApprovals').style.display = m.showApprovals === false ? 'none' : '';
      if (m.showApprovals === false && document.querySelector('.tab.active').dataset.page === 'approvals') {
        switchTab('chat');
      }
      renderAgentBar();
      clearChatLog();
      if (AUTHED) vscode.postMessage({ type: 'approvals-load' });
      break;
    case 'auth-required':
      AUTHED = false;
      $('banner').style.display = 'block';
      $('agentbar').style.display = 'none';
      break;
    case 'agent-switched':
      SELECTED_AGENT = m.agent;
      renderAgentBar();
      clearChatLog();
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
    case 'tasks': renderTasks(m.items); break;
    case 'tasks-error': $('tasklist').textContent = '任務載入失敗：' + m.message; break;
    case 'attach-context':
      PENDING_CONTEXT = { block: m.block, label: m.label };
      renderContextChip();
      switchTab('chat');
      $('input').focus();
      break;
    case 'goal-created':
      bubble('sys', m.planFirst
        ? '✅ 計畫已生成，等待你核准 — 到「任務」分頁查看並核准'
        : '✅ 已交辦' + (m.taskId ? '（任務編號 ' + String(m.taskId).slice(0, 8) + '…）' : ''));
      break;
    case 'goal-error':
      bubble('sys', '⚠ ' + m.message);
      break;
  }
});
vscode.postMessage({ type: 'ready' });
</script>
</body>
</html>`;
}
