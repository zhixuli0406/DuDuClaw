// DuDuClaw for Obsidian — chat with your self-hosted AI employees and send
// notes into their memory.
//
// Network architecture:
// - HTTP (login / refresh) goes through Obsidian's `requestUrl`, which is
//   exempt from renderer CORS — the canonical plugin approach.
// - Chat uses the native WebSocket. Obsidian's renderer sends a FIXED
//   `Origin: app://obsidian.md`, so the gateway accepts it once the operator
//   adds `obsidian.md` to `allowed_origins` (one-time setup, documented in
//   the README and surfaced in the settings tab).
// Tokens (not the password) are persisted via plugin data; the password is
// used once per login and never stored.
import {
  App,
  ItemView,
  Notice,
  Plugin,
  PluginSettingTab,
  Setting,
  WorkspaceLeaf,
  requestUrl,
} from 'obsidian';

const VIEW_TYPE = 'duduclaw-chat';

interface DuduSettings {
  gatewayUrl: string;
  email: string;
  accessToken: string;
  refreshToken: string;
}

const DEFAULT_SETTINGS: DuduSettings = {
  gatewayUrl: 'http://127.0.0.1:18789',
  email: '',
  accessToken: '',
  refreshToken: '',
};

export default class DuduClawPlugin extends Plugin {
  settings: DuduSettings = DEFAULT_SETTINGS;

  async onload(): Promise<void> {
    await this.loadSettings();
    this.registerView(VIEW_TYPE, (leaf) => new ChatView(leaf, this));
    this.addRibbonIcon('paw-print', 'DuDuClaw：開啟對話', () => this.activateView());
    this.addCommand({
      id: 'open-chat',
      name: '開啟 AI 員工對話面板',
      callback: () => this.activateView(),
    });
    this.addCommand({
      id: 'send-note-to-memory',
      name: '把目前筆記存進 AI 員工記憶',
      callback: () => this.sendActiveNoteToMemory(),
    });
    this.addSettingTab(new DuduSettingTab(this.app, this));
  }

  onunload(): void {
    // Obsidian detaches our views automatically; sockets close with them.
  }

  async loadSettings(): Promise<void> {
    this.settings = Object.assign({}, DEFAULT_SETTINGS, await this.loadData());
  }

  async saveSettings(): Promise<void> {
    await this.saveData(this.settings);
  }

  gatewayBase(): string {
    return this.settings.gatewayUrl.replace(/\/+$/, '');
  }

  async login(password: string): Promise<boolean> {
    try {
      const res = await requestUrl({
        url: `${this.gatewayBase()}/api/login`,
        method: 'POST',
        contentType: 'application/json',
        body: JSON.stringify({ email: this.settings.email, password }),
        throw: false,
      });
      const data = res.json as { access_token?: string; refresh_token?: string; error?: string };
      if (res.status !== 200 || !data.access_token) {
        new Notice(`DuDuClaw：登入失敗 — ${data?.error ?? res.status}`);
        return false;
      }
      this.settings.accessToken = data.access_token;
      this.settings.refreshToken = data.refresh_token ?? '';
      await this.saveSettings();
      new Notice('DuDuClaw：登入成功 🐾');
      return true;
    } catch (e) {
      new Notice(`DuDuClaw：連不上 gateway — ${e}`);
      return false;
    }
  }

  async refreshAccessToken(): Promise<boolean> {
    if (!this.settings.refreshToken) return false;
    try {
      const res = await requestUrl({
        url: `${this.gatewayBase()}/api/refresh`,
        method: 'POST',
        contentType: 'application/json',
        body: JSON.stringify({ refresh_token: this.settings.refreshToken }),
        throw: false,
      });
      const data = res.json as { access_token?: string; refresh_token?: string };
      if (res.status !== 200 || !data.access_token) return false;
      this.settings.accessToken = data.access_token;
      if (data.refresh_token) this.settings.refreshToken = data.refresh_token;
      await this.saveSettings();
      return true;
    } catch {
      return false;
    }
  }

  async activateView(): Promise<void> {
    const existing = this.app.workspace.getLeavesOfType(VIEW_TYPE);
    if (existing.length > 0) {
      this.app.workspace.revealLeaf(existing[0]);
      return;
    }
    const leaf = this.app.workspace.getRightLeaf(false);
    if (leaf) {
      await leaf.setViewState({ type: VIEW_TYPE, active: true });
      this.app.workspace.revealLeaf(leaf);
    }
  }

  /** Send the active note into agent memory via the chat channel (the agent
   *  stores it with its own memory tooling — same pattern as the Chrome
   *  clipper: no second write path, the agent is the librarian). */
  async sendActiveNoteToMemory(): Promise<void> {
    const file = this.app.workspace.getActiveFile();
    if (!file) {
      new Notice('DuDuClaw：沒有開啟中的筆記');
      return;
    }
    const content = await this.app.vault.cachedRead(file);
    const view = await this.getOrCreateChatView();
    view?.sendText(
      `請把以下筆記整理後存進記憶（來源：Obsidian「${file.basename}」）：\n\n${content.slice(0, 12000)}`
    );
    new Notice(`DuDuClaw：已把「${file.basename}」交給 AI 員工`);
  }

  private async getOrCreateChatView(): Promise<ChatView | null> {
    await this.activateView();
    const leaf = this.app.workspace.getLeavesOfType(VIEW_TYPE)[0];
    return (leaf?.view as ChatView) ?? null;
  }
}

// ── Chat view (right panel) ──────────────────────────────────────────────────

class ChatView extends ItemView {
  private ws: WebSocket | null = null;
  private botBuf: HTMLElement | null = null;
  private log!: HTMLElement;
  private input!: HTMLTextAreaElement;

  constructor(leaf: WorkspaceLeaf, private plugin: DuduClawPlugin) {
    super(leaf);
  }

  getViewType(): string {
    return VIEW_TYPE;
  }
  getDisplayText(): string {
    return 'DuDuClaw';
  }
  getIcon(): string {
    return 'paw-print';
  }

  async onOpen(): Promise<void> {
    const root = this.contentEl;
    root.empty();
    root.addClass('duduclaw-chat');
    this.log = root.createDiv({ cls: 'duduclaw-log' });
    const composer = root.createDiv({ cls: 'duduclaw-composer' });
    this.input = composer.createEl('textarea', {
      attr: { rows: '2', placeholder: '跟你的 AI 員工說話…（Enter 送出）' },
    });
    const send = composer.createEl('button', { text: '送出' });
    send.addEventListener('click', () => this.sendFromInput());
    this.input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        this.sendFromInput();
      }
    });
    this.connect();
  }

  async onClose(): Promise<void> {
    try {
      this.ws?.close();
    } catch {
      /* noop */
    }
    this.ws = null;
  }

  private bubble(cls: string, text: string): HTMLElement {
    const el = this.log.createDiv({ cls: `duduclaw-msg duduclaw-${cls}` });
    el.setText(text);
    this.log.scrollTop = this.log.scrollHeight;
    return el;
  }

  private connect(retried = false): void {
    const token = this.plugin.settings.accessToken;
    if (!token) {
      this.bubble('sys', '尚未登入——請到設定頁填 gateway/Email 並登入。');
      return;
    }
    try {
      this.ws?.close();
    } catch {
      /* noop */
    }
    const ws = new WebSocket(this.plugin.gatewayBase().replace(/^http/, 'ws') + '/ws/chat');
    this.ws = ws;
    ws.onopen = () => ws.send(JSON.stringify({ type: 'auth', token }));
    ws.onmessage = async (ev) => {
      let m: { type?: string; content?: string; message?: string; agent_name?: string };
      try {
        m = JSON.parse(String(ev.data));
      } catch {
        return;
      }
      switch (m.type) {
        case 'assistant_chunk':
          if (!this.botBuf) this.botBuf = this.bubble('bot', '');
          this.botBuf.setText(this.botBuf.getText() + (m.content ?? ''));
          break;
        case 'assistant_done':
          if (this.botBuf) {
            this.botBuf.setText(m.content ?? '');
            this.botBuf = null;
          } else {
            this.bubble('bot', m.content ?? '');
          }
          break;
        case 'error': {
          this.botBuf = null;
          const msg = m.message ?? '';
          if (/auth|token|jwt|unauthor/i.test(msg) && !retried) {
            if (await this.plugin.refreshAccessToken()) {
              this.connect(true);
              return;
            }
          }
          this.bubble('sys', `⚠ ${msg}`);
          break;
        }
      }
    };
    ws.onclose = () => {
      // Reconnect lazily on next send; a closed socket while idle is normal.
    };
    ws.onerror = () => {
      this.bubble(
        'sys',
        '⚠ 連線失敗——確認 gateway 有開，且 config.toml [gateway] allowed_origins 已加入 "obsidian.md"。'
      );
    };
  }

  sendText(text: string): void {
    if (!text.trim()) return;
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      this.connect();
      setTimeout(() => this.sendText(text), 700);
      return;
    }
    this.bubble('user', text.length > 300 ? text.slice(0, 300) + '…' : text);
    this.ws.send(JSON.stringify({ type: 'user_message', content: text }));
  }

  private sendFromInput(): void {
    const t = this.input.value.trim();
    if (!t) return;
    this.input.value = '';
    this.sendText(t);
  }
}

// ── Settings tab ─────────────────────────────────────────────────────────────

class DuduSettingTab extends PluginSettingTab {
  constructor(
    app: App,
    private plugin: DuduClawPlugin
  ) {
    super(app, plugin);
  }

  display(): void {
    const { containerEl } = this;
    containerEl.empty();
    containerEl.createEl('h2', { text: 'DuDuClaw' });

    new Setting(containerEl)
      .setName('Gateway 位址')
      .setDesc('你的 DuDuClaw gateway（本機預設 http://127.0.0.1:18789）')
      .addText((t) =>
        t.setValue(this.plugin.settings.gatewayUrl).onChange(async (v) => {
          this.plugin.settings.gatewayUrl = v.trim();
          await this.plugin.saveSettings();
        })
      );

    new Setting(containerEl).setName('Email').addText((t) =>
      t.setValue(this.plugin.settings.email).onChange(async (v) => {
        this.plugin.settings.email = v.trim();
        await this.plugin.saveSettings();
      })
    );

    let password = '';
    new Setting(containerEl)
      .setName('密碼')
      .setDesc('僅用於登入換取 token，不會被儲存')
      .addText((t) => {
        t.inputEl.type = 'password';
        t.onChange((v) => (password = v));
      })
      .addButton((b) =>
        b
          .setButtonText('登入')
          .setCta()
          .onClick(async () => {
            await this.plugin.login(password);
          })
      );

    containerEl.createEl('p', {
      text: '一次性設定：對話走 WebSocket，Obsidian 會帶 app://obsidian.md 的來源標頭——請在 gateway 的 config.toml [gateway] allowed_origins 加入 "obsidian.md" 並重載設定。',
      cls: 'setting-item-description',
    });
    containerEl.createEl('p', {
      text: '隱私：本外掛只連線到上方設定的 gateway，無遙測、無第三方服務。',
      cls: 'setting-item-description',
    });
  }
}
