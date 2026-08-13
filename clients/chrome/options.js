const $ = (id) => document.getElementById(id);

function show(text, ok) {
  $('msg').textContent = text;
  $('msg').style.color = ok ? '#059669' : '#e11d48';
}

async function ensureHostPermission(url) {
  try {
    const origin = new URL(url).origin + '/*';
    const granted = await chrome.permissions.contains({ origins: [origin] });
    if (granted) return true;
    return await chrome.permissions.request({ origins: [origin] });
  } catch {
    return false;
  }
}

$('loginBtn').addEventListener('click', async () => {
  const gateway = ($('gateway').value.trim() || 'http://127.0.0.1:18789').replace(/\/+$/, '');
  const email = $('email').value.trim();
  const password = $('password').value;
  if (!email || !password) { show('請填 Email 與密碼', false); return; }

  // Non-loopback gateways need a runtime host permission for the login fetch.
  const loopback = /^https?:\/\/(127\.0\.0\.1|localhost)(:\d+)?$/.test(gateway);
  if (!loopback && !(await ensureHostPermission(gateway))) {
    show('需要授權存取 ' + gateway + ' 才能登入', false);
    return;
  }

  show('登入中…', true);
  try {
    const res = await fetch(gateway + '/api/login', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ email, password }),
    });
    const data = await res.json().catch(() => ({}));
    if (!res.ok || !data.access_token) {
      show('登入失敗：' + (data.error || res.status), false);
      return;
    }
    await chrome.storage.local.set({
      gatewayUrl: gateway,
      accessToken: data.access_token,
      refreshToken: data.refresh_token || null,
    });
    $('password').value = '';
    show('登入成功 🐾 可以回側邊欄開始對話了', true);
  } catch (e) {
    show('連不上 gateway：' + e, false);
  }
});

(async function init() {
  const cfg = await chrome.storage.local.get(['gatewayUrl']);
  $('gateway').value = cfg.gatewayUrl || 'http://127.0.0.1:18789';
  $('extid').textContent = chrome.runtime.id;
  $('copyId').addEventListener('click', () => navigator.clipboard.writeText(chrome.runtime.id));
})();
