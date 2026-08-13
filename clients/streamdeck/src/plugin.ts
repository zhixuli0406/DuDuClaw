// DuDuClaw Stream Deck plugin — physical HITL.
//
// Safety posture: a hardware "approve" button must never be a blind approve.
// The key's LCD title always shows the pending item's summary (truncated),
// polled every 15s — the operator reads WHAT they're approving on the key
// itself before pressing. Decisions route through the gateway's normal
// `approvals.decide` RPC: same authorization, idempotency and accounting as
// the dashboard buttons. Node `ws` carries no browser Origin, so the
// gateway's origin gate treats us as a non-browser client (same architecture
// as the VS Code extension).
import streamDeck, { action, KeyDownEvent, SingletonAction, WillAppearEvent } from '@elgato/streamdeck';
import WebSocket from 'ws';

type GlobalSettings = {
  gatewayUrl?: string;
  email?: string;
  password?: string;
};

type Approval = {
  id: string;
  agent_id?: string;
  action_kind?: string;
  summary?: string;
  description?: string;
};

// ── Gateway client (login + JSON-RPC over WS) ────────────────────────────────

class Gateway {
  private ws: WebSocket | undefined;
  private ready: Promise<void> | undefined;
  private seq = 0;
  private pending = new Map<string, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  private accessToken: string | undefined;
  private refreshToken: string | undefined;

  constructor(private settings: GlobalSettings) {}

  private base(): string {
    return (this.settings.gatewayUrl ?? 'http://127.0.0.1:18789').replace(/\/+$/, '');
  }

  private async login(): Promise<void> {
    const { email, password } = this.settings;
    if (!email || !password) throw new Error('未設定帳密（按鍵設定頁填 gateway/Email/密碼）');
    const res = await fetch(`${this.base()}/api/login`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ email, password }),
    });
    const data = (await res.json().catch(() => ({}))) as {
      access_token?: string;
      refresh_token?: string;
      error?: string;
    };
    if (!res.ok || !data.access_token) throw new Error(`登入失敗：${data.error ?? res.status}`);
    this.accessToken = data.access_token;
    this.refreshToken = data.refresh_token;
  }

  private async refresh(): Promise<boolean> {
    if (!this.refreshToken) return false;
    try {
      const res = await fetch(`${this.base()}/api/refresh`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ refresh_token: this.refreshToken }),
      });
      if (!res.ok) return false;
      const data = (await res.json()) as { access_token?: string; refresh_token?: string };
      if (!data.access_token) return false;
      this.accessToken = data.access_token;
      if (data.refresh_token) this.refreshToken = data.refresh_token;
      return true;
    } catch {
      return false;
    }
  }

  private connect(): Promise<void> {
    if (this.ws && this.ws.readyState === WebSocket.OPEN && this.ready) return this.ready;
    this.ready = (async () => {
      if (!this.accessToken) await this.login();
      const sock = new WebSocket(this.base().replace(/^http/, 'ws') + '/ws');
      this.ws = sock;
      sock.on('message', (raw: Buffer) => {
        let f: { id?: unknown; result?: unknown; error?: { message?: string } };
        try {
          f = JSON.parse(raw.toString('utf8'));
        } catch {
          return;
        }
        if (f.id == null) return;
        const p = this.pending.get(String(f.id));
        if (!p) return;
        this.pending.delete(String(f.id));
        if (f.error) p.reject(new Error(f.error.message ?? 'rpc error'));
        else p.resolve(f.result);
      });
      sock.on('close', () => {
        if (this.ws === sock) {
          this.ws = undefined;
          this.ready = undefined;
        }
        for (const [, p] of this.pending) p.reject(new Error('connection closed'));
        this.pending.clear();
      });
      await new Promise<void>((resolve, reject) => {
        sock.once('open', () => resolve());
        sock.once('error', (e: Error) => reject(e));
      });
      try {
        await this.raw('connect', { jwt: this.accessToken });
      } catch (e) {
        sock.close();
        if (await this.refresh()) {
          this.ready = undefined;
          return this.connect();
        }
        throw e;
      }
    })();
    return this.ready;
  }

  private raw(method: string, params: unknown): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const id = String(++this.seq);
      this.pending.set(id, { resolve, reject });
      this.ws?.send(JSON.stringify({ jsonrpc: '2.0', method, params, id }));
      setTimeout(() => {
        if (this.pending.delete(id)) reject(new Error(`${method} timeout`));
      }, 15000);
    });
  }

  async rpc(method: string, params: unknown): Promise<unknown> {
    await this.connect();
    return this.raw(method, params);
  }

  updateSettings(s: GlobalSettings): void {
    this.settings = s;
    this.accessToken = undefined;
    this.refreshToken = undefined;
    try {
      this.ws?.close();
    } catch {
      /* noop */
    }
  }
}

let gateway = new Gateway({});
streamDeck.settings.onDidReceiveGlobalSettings<GlobalSettings>((ev) => {
  gateway.updateSettings(ev.settings);
});

async function oldestApproval(): Promise<Approval | undefined> {
  const res = (await gateway.rpc('approvals.list', {})) as { approvals?: Approval[] };
  return res.approvals?.[0];
}

function keyTitle(prefix: string, a: Approval | undefined, count: number): string {
  if (!a) return `${prefix}\n(無待審)`;
  const label = (a.summary ?? a.description ?? a.action_kind ?? a.id).slice(0, 24);
  return `${prefix} ${count}件\n${label}`;
}

// ── Actions ──────────────────────────────────────────────────────────────────

abstract class DecideAction extends SingletonAction {
  protected abstract prefix: string;
  protected abstract approve: boolean;
  private timer: NodeJS.Timeout | undefined;

  override async onWillAppear(ev: WillAppearEvent): Promise<void> {
    const refresh = async () => {
      try {
        const res = (await gateway.rpc('approvals.list', {})) as { approvals?: Approval[] };
        const items = res.approvals ?? [];
        await ev.action.setTitle(keyTitle(this.prefix, items[0], items.length));
      } catch (e) {
        await ev.action.setTitle(`${this.prefix}\n⚠ 未連線`);
        streamDeck.logger.warn(`refresh failed: ${e}`);
      }
    };
    await refresh();
    this.timer = setInterval(refresh, 15000);
  }

  override async onKeyDown(ev: KeyDownEvent): Promise<void> {
    try {
      const a = await oldestApproval();
      if (!a) {
        await ev.action.showOk();
        return;
      }
      await gateway.rpc('approvals.decide', { id: a.id, approve: this.approve });
      await ev.action.showOk();
      await ev.action.setTitle(`${this.prefix}\n已${this.approve ? '同意' : '拒絕'}`);
    } catch (e) {
      streamDeck.logger.error(`decide failed: ${e}`);
      await ev.action.showAlert();
    }
  }
}

@action({ UUID: 'com.duduclaw.deck.approve' })
class ApproveAction extends DecideAction {
  protected prefix = '✅';
  protected approve = true;
}

@action({ UUID: 'com.duduclaw.deck.deny' })
class DenyAction extends DecideAction {
  protected prefix = '⛔';
  protected approve = false;
}

@action({ UUID: 'com.duduclaw.deck.status' })
class StatusAction extends SingletonAction {
  override async onWillAppear(ev: WillAppearEvent): Promise<void> {
    const refresh = async () => {
      try {
        const res = (await gateway.rpc('approvals.list', {})) as { approvals?: Approval[] };
        await ev.action.setTitle(`🐾 DuDuClaw\n待審 ${res.approvals?.length ?? 0}`);
      } catch {
        await ev.action.setTitle('🐾 DuDuClaw\n未連線');
      }
    };
    await refresh();
    setInterval(refresh, 15000);
  }

  override async onKeyDown(): Promise<void> {
    const url = (await streamDeck.settings.getGlobalSettings<GlobalSettings>()).gatewayUrl ?? 'http://127.0.0.1:18789';
    await streamDeck.system.openUrl(url);
  }
}

streamDeck.actions.registerAction(new ApproveAction());
streamDeck.actions.registerAction(new DenyAction());
streamDeck.actions.registerAction(new StatusAction());
streamDeck.connect();
