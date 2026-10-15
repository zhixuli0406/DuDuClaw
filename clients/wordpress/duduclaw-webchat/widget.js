/* DuDuClaw WebChat widget — connects ONLY to the site-owner-configured
 * gateway (see settings). WebChat protocol: auth frame with the public
 * widget key, then user_message / assistant_chunk / assistant_done. */
(function () {
  'use strict';
  var cfg = window.DUDUCLAW_WC;
  if (!cfg || !cfg.gateway || !cfg.key) return;

  var ws = null, botBuf = null, open = false;

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
    '  <div id="dwc-head"><span></span><button id="dwc-close" aria-label="Close">×</button></div>' +
    '  <div id="dwc-log" role="log" aria-live="polite"></div>' +
    '  <div id="dwc-compose"><textarea id="dwc-input" rows="1" placeholder="輸入訊息…"></textarea>' +
    '  <button id="dwc-send">送出</button></div>' +
    '  <div id="dwc-brand"><a href="https://github.com/zhixuli0406/DuDuClaw" target="_blank" rel="noopener">Powered by DuDuClaw</a></div>' +
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
