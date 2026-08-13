/* DuDuClaw WebChat widget — connects ONLY to the site-owner-configured
 * gateway (see settings). WebChat protocol: auth frame with the public
 * widget key, then user_message / assistant_chunk / assistant_done. */
(function () {
  'use strict';
  var cfg = window.DUDUCLAW_WC;
  if (!cfg || !cfg.gateway || !cfg.key) return;

  var ws = null, botBuf = null, open = false;

  var root = document.createElement('div');
  root.id = 'dwc-root';
  root.className = 'dwc-' + (cfg.position === 'left' ? 'left' : 'right');
  root.innerHTML =
    '<button id="dwc-bubble" aria-label="Chat">🐾</button>' +
    '<div id="dwc-panel" hidden>' +
    '  <div id="dwc-head"><span></span><button id="dwc-close" aria-label="Close">×</button></div>' +
    '  <div id="dwc-log" role="log" aria-live="polite"></div>' +
    '  <div id="dwc-compose"><textarea id="dwc-input" rows="1" placeholder="輸入訊息…"></textarea>' +
    '  <button id="dwc-send">送出</button></div>' +
    '  <div id="dwc-brand"><a href="https://github.com/zhixuli0406/DuDuClaw" target="_blank" rel="noopener">Powered by DuDuClaw 🐾</a></div>' +
    '</div>';
  document.body.appendChild(root);
  root.querySelector('#dwc-head span').textContent = cfg.title || 'AI 客服';

  var log = root.querySelector('#dwc-log');
  var input = root.querySelector('#dwc-input');

  function bubble(cls, text) {
    var d = document.createElement('div');
    d.className = 'dwc-msg dwc-' + cls;
    d.textContent = text;
    log.appendChild(d);
    log.scrollTop = log.scrollHeight;
    return d;
  }

  function connect() {
    if (ws && ws.readyState === WebSocket.OPEN) return;
    ws = new WebSocket(cfg.gateway.replace(/^http/, 'ws') + '/ws/chat');
    ws.onopen = function () {
      ws.send(JSON.stringify({ type: 'auth', token: 'widget:' + cfg.key }));
    };
    ws.onmessage = function (ev) {
      var m;
      try { m = JSON.parse(ev.data); } catch (e) { return; }
      if (m.type === 'assistant_chunk') {
        if (!botBuf) botBuf = bubble('bot', '');
        botBuf.textContent += m.content;
      } else if (m.type === 'assistant_done') {
        if (botBuf) { botBuf.textContent = m.content; botBuf = null; }
        else bubble('bot', m.content);
      } else if (m.type === 'error') {
        botBuf = null;
        bubble('sys', '⚠ ' + m.message);
      }
    };
    ws.onclose = function () { /* reconnect on next send */ };
  }

  function send() {
    var text = input.value.trim();
    if (!text) return;
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      connect();
      setTimeout(send, 700);
      return;
    }
    bubble('user', text);
    ws.send(JSON.stringify({ type: 'user_message', content: text }));
    input.value = '';
  }

  root.querySelector('#dwc-bubble').addEventListener('click', function () {
    open = !open;
    root.querySelector('#dwc-panel').hidden = !open;
    if (open) { connect(); input.focus(); }
  });
  root.querySelector('#dwc-close').addEventListener('click', function () {
    open = false;
    root.querySelector('#dwc-panel').hidden = true;
  });
  root.querySelector('#dwc-send').addEventListener('click', send);
  input.addEventListener('keydown', function (e) {
    if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); }
  });
})();
