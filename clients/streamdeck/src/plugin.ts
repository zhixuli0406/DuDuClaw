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

const NEW_GATEWAY = 'http://127.0.0.1:18889';
const LEGACY_GATEWAY = 'http://127.0.0.1:18789';

async function isDuduClawGateway(base: string): Promise<boolean> {
  try {
    const res = await fetch(`${base}/healthz`, {
      signal: AbortSignal.timeout(500),
    });
    const body = (await res.json()) as { service?: unknown };
    return body.service === 'duduclaw-gateway';
  } catch {
    return false;
  }
}

async function resolveImplicitGateway(settings: GlobalSettings): Promise<GlobalSettings> {
  if (settings.gatewayUrl) return settings;
  const [currentLive, legacyLive] = await Promise.all([
    isDuduClawGateway(NEW_GATEWAY),
    isDuduClawGateway(LEGACY_GATEWAY),
  ]);
  const hasCredentials = Boolean(settings.email || settings.password);
  const gatewayUrl = hasCredentials
    ? legacyLive ? LEGACY_GATEWAY : undefined
    : NEW_GATEWAY;
  if (!gatewayUrl) return settings;
  const resolved = { ...settings, gatewayUrl };
  await streamDeck.settings.setGlobalSettings(resolved);
  return resolved;
}

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
    if (!this.settings.gatewayUrl) {
      throw new Error('尚未確認 DuDuClaw gateway，請先在設定中指定 gateway URL');
    }
    return this.settings.gatewayUrl.replace(/\/+$/, '');
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
      if (!(await isDuduClawGateway(this.base()))) return false;
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
      if (!(await isDuduClawGateway(this.base()))) {
        throw new Error('拒絕傳送憑證：目標不是 DuDuClaw gateway');
      }
      if (!this.accessToken) await this.login();
      const sock = new WebSocket(this.base().replace(/^http/, 'ws') + '/ws');
      this.ws = sock;
      // Replies are `{type:"res", id, ok, payload, error}` (see
      // `crates/duduclaw-gateway/src/protocol.rs`); `error` is a bare JSON
      // value, and server-pushed `event` frames carry no `id`.
      sock.on('message', (raw: Buffer) => {
        let f: {
          type?: string;
          id?: unknown;
          ok?: boolean;
          payload?: unknown;
          error?: unknown;
        };
        try {
          f = JSON.parse(raw.toString('utf8'));
        } catch {
          return;
        }
        if (f.type !== 'res' || f.id == null) return;
        const p = this.pending.get(String(f.id));
        if (!p) return;
        this.pending.delete(String(f.id));
        if (f.ok === false) {
          const msg = typeof f.error === 'string' ? f.error : JSON.stringify(f.error);
          p.reject(new Error(msg || 'rpc error'));
        } else {
          p.resolve(f.payload);
        }
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
      // `type: 'req'` is mandatory — an untagged frame fails the gateway's
      // `WsFrame` deserialization and gets the socket closed as an auth failure.
      this.ws?.send(JSON.stringify({ type: 'req', id, method, params }));
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
streamDeck.settings.onDidReceiveGlobalSettings<GlobalSettings>(async (ev) => {
  gateway.updateSettings(await resolveImplicitGateway(ev.settings));
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
        await ev.action.setTitle(`DuDuClaw\n待審 ${res.approvals?.length ?? 0}`);
      } catch {
        await ev.action.setTitle('DuDuClaw\n未連線');
      }
    };
    await refresh();
    setInterval(refresh, 15000);
  }

  override async onKeyDown(): Promise<void> {
    const settings = await resolveImplicitGateway(
      await streamDeck.settings.getGlobalSettings<GlobalSettings>(),
    );
    const url = settings.gatewayUrl ?? NEW_GATEWAY;
    await streamDeck.system.openUrl(url);
  }
}

streamDeck.actions.registerAction(new ApproveAction());
streamDeck.actions.registerAction(new DenyAction());
streamDeck.actions.registerAction(new StatusAction());
streamDeck.connect();
