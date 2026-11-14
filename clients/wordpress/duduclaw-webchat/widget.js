/* DuDuClaw WebChat widget — connects ONLY to the site-owner-configured
 * gateway (see settings). WebChat protocol: auth frame with the public
 * widget key, then user_message / assistant_chunk / assistant_done. */
(function () {
  'use strict';
  var cfg = window.DUDUCLAW_WC;
  if (!cfg || !cfg.gateway || !cfg.key) return;

  var ws = null, botBuf = null, open = false;
  var ready = false, started = false, suspended = false, destroyed = false;
  var reconnectTimer = null, connectTimer = null, reconnectAttempt = 0;
  var unsent = [], lastSent = null, awaitingReply = false;
  // There was no widget locale mechanism. Use the page language, defaulting
  // to the original Traditional Chinese UI when the page has no lang attribute.
  var strings = {
    'zh-TW': {
      chat: '聊天', close: '關閉', title: 'AI 客服', placeholder: '輸入訊息…', send: '送出',
      connected: '已連線', reconnecting: '重新連線中…', disconnected: '連線已中斷',
      reconnect: '重新連線', notSent: '未送出', retry: '重試', retrying: '連線後重試',
      interrupted: '連線中斷，回覆可能沒有送達', error: '暫時無法取得回覆，請稍後再試。'
    },
    en: {
      chat: 'Chat', close: 'Close', title: 'AI support', placeholder: 'Type a message…', send: 'Send',
      connected: 'Connected', reconnecting: 'Reconnecting…', disconnected: 'Connection lost',
      reconnect: 'Reconnect', notSent: 'Not sent', retry: 'Retry', retrying: 'Retry when connected',
      interrupted: 'Connection interrupted, the reply may not have arrived',
      error: 'A reply is unavailable right now. Please try again later.'
    }
  };
  var lang = (document.documentElement.lang || 'zh-TW').toLowerCase();
  var t = strings[lang.split('-')[0] === 'zh' ? 'zh-TW' : 'en'];

  // DuDuClaw C1 cat mark, inline so the widget needs no extra request. The
  // bubble is navy, so the ink layer is cream and the tail stays amber.
  var MARK =
    '<svg class="dwc-mark" viewBox="23 38 1231 1231" aria-hidden="true" focusable="false">' +
    '<g transform="translate(0 1254) scale(0.1 -0.1)"><path class="dwc-ink" d="M6520 11193 c-28 -26 -62 -82 -251 -416 -43 -76 -94 -164 -113 -195 -18 -31 -62 -106 -96 -167 -35 -60 -164 -285 -288 -500 -496 -862 -577 -1033 -672 -1425 -58 -240 -62 -295 -67 -801 -6 -620 10 -770 116 -1039 153 -394 445 -688 1155 -1170 27 -18 195 -137 336 -239 58 -41 175 -125 261 -185 86 -61 279 -201 428 -311 149 -110 281 -207 293 -215 21 -14 21 -14 14 70 -3 47 -13 308 -21 580 -8 272 -20 615 -25 761 -25 668 -6 871 104 1087 147 290 361 460 776 616 339 128 442 186 593 334 186 184 271 354 272 542 0 128 -32 207 -112 281 -27 24 -238 227 -470 452 l-423 410 -2 612 -3 613 -24 26 c-27 28 -76 42 -113 31 -12 -4 -233 -217 -490 -473 l-467 -466 -49 105 c-27 57 -122 262 -212 454 -90 193 -166 355 -170 360 -4 6 -34 68 -65 138 -62 139 -80 157 -149 157 -25 0 -45 -8 -66 -27z M4392 7085 c-968 -643 -1509 -1112 -2023 -1755 -705 -882 -1024 -1836 -924 -2770 93 -880 722 -1648 1455 -1776 88 -15 328 -16 3100 -9 2134 5 3007 10 3012 18 26 42 21 611 -7 787 -120 760 -636 1571 -1476 2319 -449 399 -724 612 -1499 1156 -639 449 -895 679 -1093 985 -191 295 -274 590 -293 1040 -4 88 -10 160 -13 160 -3 0 -111 -70 -239 -155z"/>' +
    '<path class="dwc-accent" d="M11255 7273 c-201 -28 -596 -153 -893 -282 -1455 -636 -2239 -1930 -1835 -3031 69 -189 217 -461 390 -716 256 -378 431 -747 517 -1089 84 -336 88 -792 11 -1206 -34 -183 -36 -172 28 -164 607 75 1114 503 1312 1110 59 178 76 286 82 501 9 310 -26 500 -191 1029 -232 741 -289 1069 -256 1465 44 539 223 936 815 1805 38 55 105 154 151 221 104 153 126 196 127 250 1 94 -87 131 -258 107z"/></g></svg>';

  var root = document.createElement('div');
  root.id = 'dwc-root';
  root.className = 'dwc-' + (cfg.position === 'left' ? 'left' : 'right');
  root.innerHTML =
    '<button id="dwc-bubble" aria-label="Chat">' + MARK + '</button>' +
    '<div id="dwc-panel" hidden>' +
    '  <div id="dwc-head"><div><span id="dwc-title"></span><div id="dwc-status" role="status" aria-live="polite"><span id="dwc-dot" aria-hidden="true"></span><span id="dwc-status-text"></span></div></div><button id="dwc-reconnect" type="button" hidden></button><button id="dwc-close" type="button">×</button></div>' +
    '  <div id="dwc-log" role="log" aria-live="polite"></div>' +
    '  <div id="dwc-compose"><textarea id="dwc-input" rows="1" placeholder="輸入訊息…"></textarea>' +
    '  <button id="dwc-send">送出</button></div>' +
    '  <div id="dwc-brand"><a href="https://github.com/zhixuli0406/DuDuClaw" target="_blank" rel="noopener">Powered by DuDuClaw</a></div>' +
    '</div>';
  document.body.appendChild(root);
  root.querySelector('#dwc-title').textContent = cfg.title || t.title;
  root.querySelector('#dwc-bubble').setAttribute('aria-label', t.chat);
  root.querySelector('#dwc-close').setAttribute('aria-label', t.close);
  root.querySelector('#dwc-input').placeholder = t.placeholder;
  root.querySelector('#dwc-input').setAttribute('aria-label', t.placeholder);
  root.querySelector('#dwc-send').textContent = t.send;
  var reconnectButton = root.querySelector('#dwc-reconnect');
  reconnectButton.textContent = t.reconnect;
  reconnectButton.addEventListener('click', recover);

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

  function setStatus(state) {
    root.querySelector('#dwc-status').className = 'dwc-' + state;
    root.querySelector('#dwc-status-text').textContent = t[state];
    reconnectButton.hidden = state !== 'disconnected';
  }

  function clearTimers() {
    clearTimeout(reconnectTimer);
    clearTimeout(connectTimer);
    reconnectTimer = connectTimer = null;
  }

  function stopSocket() {
    var socket = ws;
    ws = null;
    ready = false;
    if (socket) {
      socket.onopen = socket.onmessage = socket.onclose = socket.onerror = null;
      socket.close();
    }
  }

  function markInterrupted() {
    if (awaitingReply && lastSent) lastSent.notice.textContent = t.interrupted;
  }

  function scheduleReconnect() {
    if (destroyed || suspended || reconnectTimer !== null) return;
    if (reconnectAttempt >= 10) {
      setStatus('disconnected');
      unsent.forEach(function (entry) {
        entry.retryRequested = false;
        entry.retry.disabled = false;
        entry.retry.textContent = t.retry;
      });
      return;
    }
    setStatus('reconnecting');
    var delay = Math.min(1000 * Math.pow(2, reconnectAttempt), 30000);
    reconnectAttempt += 1;
    reconnectTimer = setTimeout(function () {
      reconnectTimer = null;
      connect();
    }, delay);
  }

  function connectionLost(socket) {
    if (socket !== ws || destroyed || suspended) return;
    clearTimeout(connectTimer);
    connectTimer = null;
    markInterrupted();
    stopSocket();
    scheduleReconnect();
  }

  function connect() {
    if (destroyed || suspended || ws || reconnectTimer !== null) return;
    started = true;
    setStatus('reconnecting');
    var socket;
    try {
      socket = new WebSocket(cfg.gateway.replace(/^http/, 'ws') + '/ws/chat');
    } catch (e) {
      scheduleReconnect();
      return;
    }
    ws = socket;
    // A transport opening is not authentication success. session_info is the
    // existing server greeting; rejected or stalled handshakes count as failures.
    connectTimer = setTimeout(function () { connectionLost(socket); }, 15000);
    socket.onopen = function () {
      if (socket !== ws) return;
      try {
        socket.send(JSON.stringify({ type: 'auth', token: 'widget:' + cfg.key }));
      } catch (e) { connectionLost(socket); }
    };
    socket.onmessage = function (ev) {
      if (socket !== ws) return;
      var m;
      try { m = JSON.parse(ev.data); } catch (e) { return; }
      if (!m || typeof m !== 'object') return;
      if (m.type === 'session_info') {
        clearTimeout(connectTimer);
        connectTimer = null;
        ready = true;
        reconnectAttempt = 0;
        setStatus('connected');
        unsent.slice().forEach(function (entry) {
          if (entry.retryRequested) transmit(entry);
        });
      } else if ((m.type === 'assistant_chunk' || m.type === 'assistant_done') && typeof m.content === 'string') {
        if (lastSent) lastSent.notice.textContent = '';
        if (m.type === 'assistant_chunk') {
          if (!botBuf) botBuf = bubble('bot', '');
          botBuf.textContent += m.content;
        } else {
          if (botBuf) botBuf.textContent = m.content;
          else bubble('bot', m.content);
          botBuf = null;
          awaitingReply = false;
        }
      } else if (m.type === 'error') {
        // Never expose server reasons, codes or operator details to visitors.
        botBuf = null;
        awaitingReply = false;
        bubble('sys', t.error);
      }
    };
    socket.onclose = socket.onerror = function () { connectionLost(socket); };
  }

  function visitorMessage(text) {
    var node = bubble('user', '');
    var content = document.createElement('span');
    content.textContent = text;
    var notice = document.createElement('div');
    notice.className = 'dwc-notice';
    notice.setAttribute('aria-live', 'polite');
    var retry = document.createElement('button');
    retry.type = 'button';
    retry.className = 'dwc-retry';
    retry.textContent = t.retry;
    retry.hidden = true;
    node.appendChild(content);
    node.appendChild(notice);
    node.appendChild(retry);
    var entry = { text: text, notice: notice, retry: retry, sent: false, retryRequested: false };
    retry.addEventListener('click', function () {
      if (entry.sent || entry.retryRequested) return;
      entry.retryRequested = true;
      retry.disabled = true;
      retry.textContent = t.retrying;
      if (!transmit(entry) && !ws && reconnectTimer === null) recover();
    });
    return entry;
  }

  function transmit(entry) {
    if (entry.sent || !ready || !ws || ws.readyState !== WebSocket.OPEN) return false;
    // Consume the explicit retry before sending; reconnect must never replay a
    // message whose delivery may already have reached the gateway.
    entry.retryRequested = false;
    try {
      ws.send(JSON.stringify({ type: 'user_message', content: entry.text }));
    } catch (e) {
      entry.retry.disabled = false;
      entry.retry.textContent = t.retry;
      connectionLost(ws);
      return false;
    }
    entry.sent = true;
    entry.retry.hidden = true;
    entry.notice.textContent = '';
    unsent = unsent.filter(function (item) { return item !== entry; });
    lastSent = entry;
    awaitingReply = true;
    if (input.value.trim() === entry.text) input.value = '';
    return true;
  }

  function send() {
    var text = input.value.trim();
    if (!text) return;
    var entry = unsent.filter(function (item) { return item.text === text; })[0];
    if (!entry) entry = visitorMessage(text);
    if (transmit(entry)) return;
    if (unsent.indexOf(entry) === -1) unsent.push(entry);
    entry.notice.textContent = t.notSent;
    entry.retry.hidden = false;
    // Preserve the draft and require an explicit retry. Opening a connection
    // here does not enqueue this message for automatic delivery.
    if (!started) connect();
  }

  function recover() {
    if (!started || destroyed || suspended) return;
    markInterrupted();
    clearTimers();
    stopSocket();
    reconnectAttempt = 0;
    connect();
  }

  function onVisible() {
    if (document.visibilityState === 'visible') recover();
  }

  function onPageHide() {
    suspended = true;
    markInterrupted();
    clearTimers();
    stopSocket();
  }

  function onPageShow() {
    suspended = false;
    recover();
  }

  function teardown() {
    destroyed = true;
    clearTimers();
    stopSocket();
    document.removeEventListener('visibilitychange', onVisible);
    window.removeEventListener('online', recover);
    window.removeEventListener('pageshow', onPageShow);
    window.removeEventListener('pagehide', onPageHide);
    observer.disconnect();
  }

  document.addEventListener('visibilitychange', onVisible);
  window.addEventListener('online', recover);
  window.addEventListener('pageshow', onPageShow);
  window.addEventListener('pagehide', onPageHide);
  var observer = new MutationObserver(function () {
    if (!root.isConnected) teardown();
  });
  observer.observe(document.body, { childList: true, subtree: true });
  setStatus('disconnected');

  root.querySelector('#dwc-bubble').addEventListener('click', function () {
    open = !open;
    root.querySelector('#dwc-panel').hidden = !open;
    if (open) { if (!started) connect(); input.focus(); }
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
