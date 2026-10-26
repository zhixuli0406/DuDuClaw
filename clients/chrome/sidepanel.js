// Side panel — chat over the gateway's WebChat WS (`/ws/chat`) + HITL
// approvals over the dashboard JSON-RPC WS (`/ws`). Tokens come from
// chrome.storage.local (written by the options page login flow).
//
// NOTE (gateway origin allowlist): extension pages send
// `Origin: chrome-extension://<id>`. The gateway's origin gate accepts it once
// the BARE extension id is added to `allowed_origins` (config.toml) or
// `DUDUCLAW_ALLOWED_ORIGINS` — the options page shows the exact id to paste.

let GW = null, TOKEN = null, REFRESH = null;
let chatWs = null, rpcWs = null, rpcId = 0, botBuf = null;
let connectionGeneration = 0;
const rpcPending = new Map();

const $ = (id) => document.getElementById(id);
const log = $('chatlog');

function setStatus(t) { $('status').textContent = t; }
function bubble(cls, text) {
  const d = document.createElement('div');
  d.className = 'msg ' + cls;
  d.textContent = text;
  log.appendChild(d);
  $('page-chat').scrollTop = $('page-chat').scrollHeight;
  return d;
}

document.querySelectorAll('.tab').forEach((t) => t.addEventListener('click', () => {
  document.querySelectorAll('.tab').forEach((x) => x.classList.remove('active'));
  document.querySelectorAll('.page').forEach((x) => x.classList.remove('active'));
  t.classList.add('active');
  $('page-' + t.dataset.page).classList.add('active');
  if (t.dataset.page === 'approvals') loadApprovals();
}));

const wsBase = (base = GW) => base.replace(/^http/, 'ws');

async function isDuduClawGateway(base = GW) {
  if (!base) return false;
  try {
    const res = await fetch(base + '/healthz', {
      signal: AbortSignal.timeout(750),
    });
    const body = await res.json();
    return body?.service === 'duduclaw-gateway';
  } catch {
    return false;
  }
}

function resetConnections(reason = 'gateway identity changed') {
  connectionGeneration += 1;
  const oldChat = chatWs;
  const oldRpc = rpcWs;
  chatWs = null;
  rpcWs = null;
  botBuf = null;
  try { oldChat?.close(); } catch {}
  try { oldRpc?.close(); } catch {}
  for (const [, pending] of rpcPending) pending.reject(new Error(reason));
  rpcPending.clear();
}

async function tryRefreshToken() {
  if (!REFRESH) return false;
  try {
    if (!(await isDuduClawGateway())) return false;
    const res = await fetch(GW + '/api/refresh', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ refresh_token: REFRESH }),
    });
    if (!res.ok) return false;
    const data = await res.json();
    if (!data.access_token) return false;
    TOKEN = data.access_token;
    const patch = { accessToken: TOKEN };
    if (data.refresh_token) { REFRESH = data.refresh_token; patch.refreshToken = REFRESH; }
    await chrome.storage.local.set(patch);
    return true;
  } catch { return false; }
}

async function connectChat() {
  if (!GW || !TOKEN) return;
  const base = GW;
  const token = TOKEN;
  const generation = connectionGeneration;
  if (!(await isDuduClawGateway(base))) {
    setStatus('拒絕傳送 token：目標不是 DuDuClaw gateway');
    return;
  }
  if (generation !== connectionGeneration || base !== GW || token !== TOKEN) return;
  try { chatWs && chatWs.close(); } catch {}
  const sock = new WebSocket(wsBase(base) + '/ws/chat');
  chatWs = sock;
  sock.onopen = () => {
    if (chatWs !== sock || generation !== connectionGeneration || base !== GW || token !== TOKEN) {
      sock.close();
      return;
    }
    sock.send(JSON.stringify({ type: 'auth', token }));
    setStatus('對話已連線');
  };
  sock.onclose = () => {
    if (chatWs !== sock) return;
    chatWs = null;
    setStatus('對話連線中斷（送訊息時會自動重連；若持續失敗，請確認 gateway allowed_origins 已加入本擴充功能 id — 見設定頁）');
  };
  sock.onmessage = async (ev) => {
    if (chatWs !== sock || generation !== connectionGeneration) return;
    let m; try { m = JSON.parse(ev.data); } catch { return; }
    switch (m.type) {
      case 'session_info': setStatus('連上 ' + (m.agent_name || 'AI 員工')); break;
      case 'assistant_chunk':
        if (!botBuf) botBuf = bubble('bot', '');
        botBuf.textContent += m.content; break;
      case 'progress':
        if (m.kind !== 'keepalive') setStatus('⋯ ' + String(m.content).slice(0, 120)); break;
      case 'assistant_done':
        if (botBuf) { botBuf.textContent = m.content; botBuf = null; } else bubble('bot', m.content);
        setStatus(''); break;
      case 'error':
        botBuf = null;
        if (/auth|token|jwt|unauthor/i.test(m.message) && await tryRefreshToken()) { connectChat(); break; }
        bubble('sys', '⚠ ' + m.message); break;
    }
  };
}

function sendMessage() {
  const text = $('input').value.trim();
  if (!text) return;
  if (!TOKEN) { chrome.runtime.openOptionsPage(); return; }
  if (!chatWs || chatWs.readyState !== WebSocket.OPEN) {
    connectChat();
    setTimeout(sendMessage, 600);
    return;
  }
  bubble('user', text);
  chatWs.send(JSON.stringify({ type: 'user_message', content: text }));
  $('input').value = '';
}
$('send').addEventListener('click', sendMessage);
$('input').addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); sendMessage(); }
});

function rpc(method, params) {
  return new Promise((resolve, reject) => {
    if (!rpcWs || rpcWs.readyState !== WebSocket.OPEN) {
      reject(new Error('RPC socket is not connected'));
      return;
    }
    const id = String(++rpcId);
    rpcPending.set(id, { resolve, reject });
    // The gateway speaks its own `WsFrame` protocol (crates/duduclaw-gateway/
    // src/protocol.rs), NOT JSON-RPC 2.0. An untagged frame fails its
    // deserialization, which it reports as a failed handshake and closes the
    // socket — surfacing here as a misleading transport error.
    rpcWs.send(JSON.stringify({ type: 'req', id, method, params }));
    setTimeout(() => { if (rpcPending.delete(id)) reject(new Error(method + ' timeout')); }, 15000);
  });
}

async function connectRpc() {
  if (!GW || !TOKEN) throw new Error('gateway or token is not configured');
  const base = GW;
  const token = TOKEN;
  const generation = connectionGeneration;
  if (!(await isDuduClawGateway(base))) {
    throw new Error('refusing to send token: target is not a DuDuClaw gateway');
  }
  if (generation !== connectionGeneration || base !== GW || token !== TOKEN) {
    throw new Error('gateway identity changed while connecting');
  }
  return new Promise((resolve, reject) => {
    if (rpcWs && rpcWs.readyState === WebSocket.OPEN) return resolve(null);
    const sock = new WebSocket(wsBase(base) + '/ws');
    rpcWs = sock;
    sock.onmessage = (ev) => {
      if (rpcWs !== sock || generation !== connectionGeneration) return;
      let f; try { f = JSON.parse(ev.data); } catch { return; }
      // Replies are `{type:"res", id, ok, payload, error}`; `error` is a bare
      // JSON value (usually a string). Server-pushed `event` frames carry no
      // `id` and must not be mistaken for responses.
      if (f.type !== 'res' || f.id == null) return;
      const p = rpcPending.get(String(f.id));
      if (p) {
        rpcPending.delete(String(f.id));
        if (f.ok === false) {
          const msg = typeof f.error === 'string' ? f.error : JSON.stringify(f.error);
          p.reject(new Error(msg || 'request failed'));
        } else {
          p.resolve(f.payload);
        }
      }
    };
    sock.onopen = async () => {
      if (rpcWs !== sock || generation !== connectionGeneration || base !== GW || token !== TOKEN) {
        sock.close();
        reject(new Error('gateway identity changed while connecting'));
        return;
      }
      try { await rpc('connect', { jwt: token }); resolve(null); }
      catch (e) {
        if (await tryRefreshToken()) {
          try { await rpc('connect', { jwt: TOKEN }); return resolve(null); } catch (e2) { return reject(e2); }
        }
        reject(e);
      }
    };
    sock.onclose = () => {
      if (rpcWs === sock) rpcWs = null;
      reject(new Error('RPC connection closed'));
    };
    sock.onerror = () => reject(new Error('WS 連線失敗（gateway 沒開？或 allowed_origins 未加入擴充功能 id）'));
  });
}

async function loadApprovals() {
  if (!TOKEN) return;
  const list = $('aplist');
  try {
    await connectRpc();
    const res = await rpc('approvals.list', {});
    const items = (res && res.approvals) || [];
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
      ok.onclick = () => decide(it.id, true);
      no.onclick = () => decide(it.id, false);
      actions.append(ok, no);
      card.append(meta, body, actions);
      list.appendChild(card);
    }
  } catch (e) {
    list.textContent = '審批載入失敗：' + String(e).slice(0, 200);
  }
}

async function decide(id, approve) {
  try { await rpc('approvals.decide', { id, approve }); await loadApprovals(); }
  catch (e) { setStatus('審批失敗：' + String(e).slice(0, 120)); }
}
$('apRefresh').addEventListener('click', loadApprovals);
$('openOptions').addEventListener('click', () => chrome.runtime.openOptionsPage());

/** Pull a pending clip (context-menu selection) into the composer. */
async function drainClip() {
  const { pendingClip } = await chrome.storage.session.get('pendingClip');
  if (!pendingClip) return;
  await chrome.storage.session.remove('pendingClip');
  $('input').value =
    '請把以下網頁內容整理後存進記憶（來源：' + pendingClip.title + ' ' + pendingClip.url + '）：\n\n' + pendingClip.text;
  $('input').focus();
}
chrome.storage.session.onChanged.addListener((ch) => { if (ch.pendingClip?.newValue) drainClip(); });

(async function init() {
  const cfg = await chrome.storage.local.get(['gatewayUrl', 'accessToken', 'refreshToken']);
  GW = cfg.gatewayUrl ? cfg.gatewayUrl.replace(/\/+$/, '') : null;
  TOKEN = cfg.accessToken || null;
  REFRESH = cfg.refreshToken || null;
  $('banner').style.display = TOKEN && GW ? 'none' : 'block';
  if (TOKEN && GW) { connectChat(); loadApprovals(); }
  drainClip();
})();

chrome.storage.local.onChanged.addListener(async (ch) => {
  if (ch.accessToken || ch.gatewayUrl || ch.refreshToken) {
    resetConnections('gateway credentials changed');
    const cfg = await chrome.storage.local.get(['gatewayUrl', 'accessToken', 'refreshToken']);
    GW = cfg.gatewayUrl ? cfg.gatewayUrl.replace(/\/+$/, '') : null;
    TOKEN = cfg.accessToken || null;
    REFRESH = cfg.refreshToken || null;
    $('banner').style.display = TOKEN && GW ? 'none' : 'block';
    if (TOKEN && GW) { connectChat(); loadApprovals(); }
  }
});
