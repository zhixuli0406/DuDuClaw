// DuDuClaw VS Code extension — a thin client for a self-hosted DuDuClaw
// gateway. See rpc.ts for the socket architecture note (why everything lives
// in the extension host, never the webview).
import * as vscode from 'vscode';
import { SECRET_ACCESS, SECRET_REFRESH } from './config';
import { doLogin } from './auth';
import { buildSelectionContext } from './editorContext';
import { DuduPanelProvider, roleLabel } from './panel';
import type { PanelStatusState } from './types';

function updateStatusBar(item: vscode.StatusBarItem, state: PanelStatusState): void {
  if (!state.authed) {
    item.hide();
    return;
  }
  const agent = state.agentName ?? '尚未選擇 AI 員工';
  const role = roleLabel(state.role);
  item.text = `$(hubot) ${agent}` + (role ? ` · ${role}` : '');
  item.tooltip = 'DuDuClaw：點擊切換 AI 員工';
  item.show();
}

export function activate(context: vscode.ExtensionContext): void {
  const statusBar = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
  statusBar.command = 'duduclaw.switchAgent';
  context.subscriptions.push(statusBar);

  const provider = new DuduPanelProvider(context, (state) => updateStatusBar(statusBar, state));

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
    vscode.commands.registerCommand('duduclaw.switchAgent', async () => {
      await provider.pickAgentViaQuickPick();
    }),
    vscode.commands.registerCommand('duduclaw.askAboutSelection', async () => {
      const ctx = buildSelectionContext();
      if (!ctx) {
        vscode.window.showInformationMessage('DuDuClaw：請先選取程式碼。');
        return;
      }
      // Best-effort reveal — the auto-generated `<viewId>.focus` command for
      // a contributed webview view; if the panel was never opened this also
      // creates it. `postMessage` is NOT queued for a webview that hasn't
      // resolved yet, so a failure here (unlikely) means the context is
      // silently dropped rather than delivered late — the user would need
      // to open the DuDuClaw sidebar first and re-run the command. Known,
      // minor limitation; not worth a buffering mechanism for what should
      // be a same-tick reveal in practice.
      try { await vscode.commands.executeCommand('duduclaw.panel.focus'); } catch { /* best-effort */ }
      provider.attachEditorContext(ctx.block, ctx.label);
    }),
    vscode.workspace.onDidChangeConfiguration(async (e) => {
      if (e.affectsConfiguration('duduclaw.gatewayUrl')) await provider.pushInit();
    })
  );
}

export function deactivate(): void {}
