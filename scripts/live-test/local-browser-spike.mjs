#!/usr/bin/env node
// Dedicated, synthetic macOS Chrome probe. Never attach to an existing browser.
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, readdirSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServer } from 'node:http';
import { createSocket } from 'node:dgram';
import { randomBytes } from 'node:crypto';

assert.equal(process.platform, 'darwin', 'macOS-only probe');
assert(process.argv.slice(2).every(arg => arg === '--native-fixture'), 'unknown probe option');
const nativeFixture = process.argv.includes('--native-fixture');
const root = mkdtempSync(join(tmpdir(), 'ddc-local-browser-spike-'));
const home = join(root, 'home');
const profile = join(home, '.duduclaw/local_browser/synthetic/profile');
mkdirSync(profile, { recursive: true, mode: 0o700 });
mkdirSync(join(root, 'crash-dumps'), { mode: 0o700 });
mkdirSync(join(profile, 'Default'), { mode: 0o700 });
writeFileSync(join(profile, 'Default/Preferences'), JSON.stringify({
  credentials_enable_service: false,
  profile: { password_manager_enabled: false, default_content_setting_values: { automatic_downloads: 2, geolocation: 2, notifications: 2, media_stream_mic: 2, media_stream_camera: 2 } },
  autofill: { profile_enabled: false, credit_card_enabled: false },
  // Chromium reads these per-profile preferences into RendererPreferences;
  // the legacy force-webrtc command-line flag alone failed the real STUN trap.
  webrtc: { ip_handling_policy: 'disable_non_proxied_udp', ip_handling_url: [] },
  download: { default_directory: join(root, 'downloads-disabled') },
}));
const chrome = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
assert(existsSync(chrome));
// The guard is additional live-test protection, not a claimed shipping control.
const guard = `(version 1)
(allow default)
(deny network-outbound)
(allow network-outbound (remote ip "localhost:*"))
(deny mach-lookup (global-name "com.apple.securityd") (global-name "com.apple.securityd.xpc"))
(deny file-read* file-write* (subpath "/Users/lizhixu/Library/Keychains")
 (subpath "/Users/lizhixu/Library/Application Support/Google/Chrome")
 (subpath "/Users/lizhixu/.duduclaw") (subpath "/Users/lizhixu/.claude")
 (subpath "/Users/lizhixu/.codex") (subpath "/Users/lizhixu/.config"))\n`;
writeFileSync(join(root, 'guard.sb'), guard);
const events = [];
const errors = [];
const proxySockets = new Set();
const trapSockets = new Set();
function trackSockets(server, sockets) {
  server.on('error', error => errors.push(`owned server: ${error.code || error.name}`));
  server.on('connection', socket => {
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
    socket.on('error', error => {
      // Chrome may abort a refused navigation/auth tunnel. These errors are
      // transport shutdowns, not uncaught events outside the cleanup finally.
      if (['ECONNRESET', 'EPIPE'].includes(error.code)) events.push({ kind: 'owned_peer_disconnect', code: error.code });
      else errors.push(`owned socket: ${error.code || error.name}`);
    });
  });
}
const credential = randomBytes(32).toString('base64url');
const authorization = `Basic ${Buffer.from(`session:${credential}`).toString('base64')}`;
const proxy = createServer((req, res) => {
  events.push({ kind: 'proxy_http', method: req.method, url: req.url });
  res.writeHead(403).end();
});
proxy.on('connect', (req, socket) => {
  const authenticated = req.headers['proxy-authorization'] === authorization;
  events.push({ kind: 'proxy_connect', authority: req.url, authenticated });
  socket.end(authenticated ? 'HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n'
    : 'HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm="probe"\r\nContent-Length: 0\r\n\r\n');
});
let trapHits = 0;
const trap = createServer((_, res) => { trapHits++; res.end('BYPASS'); });
trackSockets(proxy, proxySockets);
trackSockets(trap, trapSockets);
const udp = createSocket('udp4');
let udpHits = 0;
udp.on('message', () => { udpHits++; });
udp.on('error', error => errors.push(`owned UDP server: ${error.code || error.name}`));
await Promise.all([
  new Promise(r => proxy.listen(0, '127.0.0.1', r)),
  new Promise(r => trap.listen(0, '127.0.0.1', r)),
  new Promise(r => udp.bind(0, '127.0.0.1', r)),
]);
const args = [
  '--remote-debugging-pipe', `--user-data-dir=${profile}`,
  '--no-first-run', '--no-default-browser-check', '--disable-background-networking',
  '--disable-component-update', '--disable-sync', '--disable-extensions',
  '--disable-component-extensions-with-background-pages', '--disable-default-apps',
  '--disable-domain-reliability', '--metrics-recording-only', '--disable-breakpad',
  ...(nativeFixture ? ['--use-mock-keychain'] : []),
  '--disable-quic', '--force-webrtc-ip-handling-policy=disable_non_proxied_udp',
  `--proxy-server=http://127.0.0.1:${proxy.address().port}`, '--proxy-bypass-list=<-loopback>',
  '--disable-features=MediaRouter,DialMediaRouteProvider,OptimizationHints,AutofillServerCommunication', '--window-size=1000,700', 'about:blank',
];
writeFileSync(join(root, 'launch.json'), JSON.stringify({ chrome, args, nativeFixture,
  boundary: nativeFixture ? 'native host trust; no additional OS sandbox; synthetic fixture keychain only' : 'extra seatbelt plus Chrome native sandbox' }, null, 2));
const child = spawn(nativeFixture ? chrome : '/usr/bin/sandbox-exec', nativeFixture ? args : ['-f', join(root, 'guard.sb'), chrome, ...args], {
  env: { PATH: '/usr/bin:/bin:/usr/sbin:/sbin', HOME: home, DUDUCLAW_HOME: join(home, '.duduclaw'), TMPDIR: root, LANG: 'en_US.UTF-8', BREAKPAD_DUMP_LOCATION: join(root, 'crash-dumps'), CHROME_LOG_FILE: join(root, 'chrome-native.log') },
  stdio: ['ignore', 'ignore', 'pipe', 'pipe', 'pipe'],
  detached: true,
});
let stderr = '';
child.stderr.on('data', b => { stderr += b; });
let nextId = 0;
let buffer = Buffer.alloc(0);
const pending = new Map();
const handlers = [];
let pipeFailure;
let cleaning = false;
function failPipe(error) {
  if (cleaning && ['EPIPE', 'ECONNRESET'].includes(error.code)) return;
  pipeFailure = new Error(`owned browser transport: ${error.code || error.name}`);
  errors.push(String(pipeFailure));
  for (const p of pending.values()) { clearTimeout(p.timer); p.reject(pipeFailure); }
  pending.clear();
}
child.on('error', failPipe);
child.stderr.on('error', failPipe);
child.stdio[3].on('error', failPipe);
child.stdio[4].on('error', failPipe);
function call(method, params = {}, sessionId) {
  if (pipeFailure) return Promise.reject(pipeFailure);
  const id = ++nextId;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timeout ${method}`)); }, 10000);
    pending.set(id, { resolve, reject, timer });
    child.stdio[3].write(`${JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) })}\0`);
  });
}
child.stdio[4].on('data', b => {
  buffer = Buffer.concat([buffer, b]);
  let i;
  while ((i = buffer.indexOf(0)) !== -1) {
    let msg;
    try { msg = JSON.parse(buffer.subarray(0, i).toString()); }
    catch (error) { failPipe(error); buffer = Buffer.alloc(0); return; }
    buffer = buffer.subarray(i + 1);
    if (msg.id) {
      const p = pending.get(msg.id);
      if (p) { clearTimeout(p.timer); pending.delete(msg.id); msg.error ? p.reject(new Error(JSON.stringify(msg.error))) : p.resolve(msg.result); }
    } else for (const handler of handlers) Promise.resolve(handler(msg)).catch(e => errors.push(String(e)));
  }
});
const results = {};
let page;
let chooser = 0;
let newTargets = 0;
let phase = 'deny';
let failure;
// Inventory only owned process metadata, never unrelated process arguments.
function inventory() {
  const text = execFileSync('/bin/ps', ['-axo', 'pid=,ppid=,pgid=,lstart=,comm='], { encoding: 'utf8' });
  const all = text.split('\n').flatMap(line => {
    const match = line.match(/^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.{24})\s+(.*)$/);
    return match ? [{ pid: +match[1], parent: +match[2], group: +match[3], born: match[4], executable: match[5] }] : [];
  });
  return all.filter(p => p.group === child.pid);
}
const owned = new Map();
function rememberOwned() { for (const p of inventory()) owned.set(p.pid, p); }
function sameProcess(p) { const old = owned.get(p.pid); return old && old.born === p.born && old.executable === p.executable && old.group === p.group; }
function assertNoDownloads(directory, observations) {
  assert(!existsSync(directory) || readdirSync(directory).length === 0, 'configured download directory contains output');
  assert(!observations.some(e => e.method === 'Browser.downloadProgress' && e.params.state === 'completed'), 'a download completed');
  assert(observations.some(e => e.method === 'Browser.downloadProgress' && e.params.state === 'canceled'), 'no canceled download evidence');
}
try {
  rememberOwned();
  writeFileSync(join(root, 'owned-child.json'), JSON.stringify({ pid: child.pid, group: child.pid, profile, startedAt: new Date().toISOString(), initialInventory: [...owned.values()] }, null, 2));
  results.version = await call('Browser.getVersion');
  results.boundary = nativeFixture ? 'NATIVE_HOST_SYNTHETIC_NO_EXTRA_OS_ISOLATION' : 'EXTRA_OS_SEATBELT';
  assert(!args.some(x => x.includes('no-sandbox') || x.includes('remote-debugging-port')));
  await call('Browser.setDownloadBehavior', { behavior: 'deny', eventsEnabled: true });
  await call('Target.setDiscoverTargets', { discover: true });
  const { targetInfos } = await call('Target.getTargets');
  const target = targetInfos.find(t => t.type === 'page');
  assert(target);
  page = (await call('Target.attachToTarget', { targetId: target.targetId, flatten: true })).sessionId;
  handlers.push(async msg => {
    if (msg.method.startsWith('Browser.download')) events.push({ phase, method: msg.method, params: msg.params });
    if (msg.method === 'Target.targetCreated' && msg.params.targetInfo.type === 'page' && msg.params.targetInfo.targetId !== target.targetId) {
      newTargets++; await call('Target.closeTarget', { targetId: msg.params.targetInfo.targetId });
    }
    if (msg.method === 'Fetch.authRequired') {
      const p = msg.params;
      const c = p.authChallenge;
      // Credentials are only supplied to this exact proxy challenge.
      const trusted = c.source === 'Proxy' && c.origin === `http://127.0.0.1:${proxy.address().port}`;
      await call('Fetch.continueWithAuth', { requestId: p.requestId, authChallengeResponse: trusted
        ? { response: 'ProvideCredentials', username: 'session', password: credential }
        : { response: 'CancelAuth' } }, msg.sessionId);
    }
    if (msg.method === 'Fetch.requestPaused') {
      const { requestId, request } = msg.params;
      if (request.url === 'https://allowed.example/synthetic-fixture') {
        await call('Fetch.fulfillRequest', { requestId, responseCode: 200, responseHeaders: [{ name: 'Content-Type', value: 'text/html' }], body: Buffer.from('<!doctype html><title>Synthetic browser safety test</title><input id=p type=password value=synthetic-secret><input id=f type=file><a id=d download=test.txt href="data:text/plain,synthetic">download</a>').toString('base64') }, msg.sessionId);
        return;
      }
      const allowed = /^https:\/\/(allowed|auth)\.example\//.test(request.url)
        || request.url.startsWith(`http://127.0.0.1:${trap.address().port}/`);
      await call(allowed ? 'Fetch.continueRequest' : 'Fetch.failRequest', allowed
        ? { requestId } : { requestId, errorReason: 'BlockedByClient' }, msg.sessionId);
    }
    if (msg.method === 'Page.fileChooserOpened') { chooser++; /* intercepted; never pass paths */ }
  });
  await call('Page.enable', {}, page);
  await call('Runtime.enable', {}, page);
  await call('Page.setInterceptFileChooserDialog', { enabled: true }, page);
  await call('Fetch.enable', { handleAuthRequests: true, patterns: [{ urlPattern: '*' }] }, page);
  await call('Page.navigate', { url: 'https://allowed.example/synthetic-fixture' }, page);
  for (let i = 0; i < 40; i++) {
    const loaded = await call('Runtime.evaluate', { expression: '!!document.getElementById("p")', returnByValue: true }, page);
    if (loaded.result.value) break;
    await new Promise(r => setTimeout(r, 50));
  }
  const evaluate = async expression => (await call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true, userGesture: true }, page)).result.value;
  results.secureSyntheticOrigin = await evaluate('({secure:isSecureContext,origin:location.origin,webTransport:typeof WebTransport})');
  assert.deepEqual(results.secureSyntheticOrigin, { secure: true, origin: 'https://allowed.example', webTransport: 'function' });
  // This is the agent-facing navigation boundary, independent of what raw CDP
  // (which employees never receive) could do. Refusal must precede dispatch.
  let navigationDispatches = 0;
  const guardedNavigate = async url => {
    const parsed = new URL(url);
    if (parsed.protocol !== 'https:' || !['allowed.example', 'auth.example'].includes(parsed.hostname) || parsed.port || parsed.username || parsed.password) throw new Error('navigation_scheme_or_host_refused');
    navigationDispatches++;
    return call('Page.navigate', { url }, page);
  };
  results.passwordFocus = await evaluate('document.getElementById("p").focus(); document.activeElement.type');
  assert.equal(results.passwordFocus, 'password');
  await evaluate('document.getElementById("f").click(); true');
  await evaluate('document.getElementById("d").click(); true');
  await new Promise(r => setTimeout(r, 500));
  const actualDownloadDirectory = join(root, 'downloads-disabled');
  assertNoDownloads(actualDownloadDirectory, events.filter(e => e.phase === 'deny'));
  results.downloadDeniedWithCancelEvent = true;
  // Prove the checker is sensitive to a real successful synthetic download.
  // It writes only into a distinct disposable fixture directory.
  phase = 'negative_seam';
  const negativeDownloads = join(root, 'download-negative-seam');
  await call('Browser.setDownloadBehavior', { behavior: 'allow', downloadPath: negativeDownloads, eventsEnabled: true });
  await evaluate('document.getElementById("d").click(); true');
  for (let i = 0; i < 40 && !events.some(e => e.phase === phase && e.method === 'Browser.downloadProgress' && e.params.state === 'completed'); i++) await new Promise(r => setTimeout(r, 50));
  assert(events.some(e => e.phase === phase && e.method === 'Browser.downloadProgress' && e.params.state === 'completed'), 'negative seam did not complete');
  assert.throws(() => assertNoDownloads(negativeDownloads, events.filter(e => e.phase === 'negative_seam')));
  results.downloadNegativeSeamDetected = true;
  await call('Browser.setDownloadBehavior', { behavior: 'deny', eventsEnabled: true });
  phase = 'deny';
  // Native loopback traps make bypass observable despite the outer offline guard.
  results.loopbackFetch = await evaluate(`fetch('http://127.0.0.1:${trap.address().port}/trap').then(()=> 'unexpected').catch(()=> 'blocked')`);
  results.rtc = await evaluate(`(async()=>{const p=new RTCPeerConnection({iceServers:[{urls:'stun:127.0.0.1:${udp.address().port}'}]});p.createDataChannel('probe');await p.setLocalDescription(await p.createOffer());await new Promise(r=>setTimeout(r,1800));const s=p.localDescription.sdp;p.close();return {hasUdpCandidate:/a=candidate:.* udp /i.test(s)};})()`);
  results.rtcUdpHits = udpHits;
  results.webTransport = await evaluate(`(async()=>{let transport;try{transport=new WebTransport('https://127.0.0.1:${udp.address().port}/',{serverCertificateHashes:[{algorithm:'sha-256',value:new Uint8Array(32)}]});const outcome=await Promise.race([transport.ready.then(()=>'unexpected-ready',error=>String(error)),new Promise(resolve=>setTimeout(()=>resolve('bounded-not-ready'),1800))]);transport.close();return {attempted:true,outcome};}catch(error){if(transport)transport.close();return {attempted:true,outcome:String(error)};}})()`);
  results.webTransportUdpHits = udpHits - results.rtcUdpHits;
  assert.equal(results.webTransport.attempted, true);
  assert.notEqual(results.webTransport.outcome, 'unexpected-ready');
  const sentinel = 'SYNTHETIC_FILE_CONTENT_MUST_NOT_REACH_PAGE';
  const sentinelPath = join(root, 'synthetic-sentinel.txt');
  writeFileSync(sentinelPath, sentinel);
  const beforeDispatch = navigationDispatches;
  results.deniedSchemes = [];
  for (const url of [`file://${sentinelPath}`, 'data:text/plain,synthetic-denied', 'chrome://version/']) {
    await assert.rejects(guardedNavigate(url), /navigation_scheme_or_host_refused/);
    results.deniedSchemes.push(new URL(url).protocol);
  }
  assert.equal(navigationDispatches, beforeDispatch, 'forbidden scheme reached CDP');
  assert(!(await evaluate('document.documentElement.innerText')).includes(sentinel));
  assert.equal(readFileSync(sentinelPath, 'utf8'), sentinel);
  results.sentinelNotReadByPage = true;
  await guardedNavigate('https://auth.example/');
  await new Promise(r => setTimeout(r, 1200));
  await call('Target.createTarget', { url: 'about:blank' });
  await new Promise(r => setTimeout(r, 300));
  results.chooserIntercepted = chooser;
  results.newTargetsClosed = newTargets;
  results.trapHits = trapHits;
  results.udpHits = udpHits;
  const preferences = JSON.parse(readFileSync(join(profile, 'Default/Preferences'), 'utf8'));
  results.profileWebRtcPolicy = preferences.webrtc?.ip_handling_policy;
  assert.equal(results.profileWebRtcPolicy, 'disable_non_proxied_udp');
  results.proxyAuthVerified = events.some(e => e.kind === 'proxy_connect' && e.authority === 'auth.example:443' && e.authenticated);
  assert.equal(trapHits, 0); assert.equal(udpHits, 0); assert.equal(results.rtc.hasUdpCandidate, false);
  assert(chooser >= 1); assert(newTargets >= 1); assert(results.proxyAuthVerified);
  rememberOwned();
  assert(owned.size > 1, 'renderer process inventory missing');
  try { results.listeners = execFileSync('/usr/sbin/lsof', ['-nP', '-a', '-p', [...owned.keys()].join(','), '-iTCP', '-sTCP:LISTEN'], { encoding: 'utf8' }); }
  catch (e) { assert.equal(e.status, 1); results.listeners = ''; }
  assert(!results.listeners);
  assertNoDownloads(actualDownloadDirectory, events.filter(e => e.phase === 'deny'));
  assert.equal(errors.length, 0, errors.join('\n'));
  results.productAssertionsPassed = true;
} catch (error) {
  failure = error;
} finally {
  cleaning = true;
  // Evidence writes are deliberately after cleanup. A full disk cannot skip it.
  const cleanupErrors = [];
  try {
    rememberOwned();
    if (child.exitCode === null && child.signalCode === null) {
      await Promise.race([call('Browser.close').catch(() => {}), new Promise(r => setTimeout(r, 1000))]);
      for (let i = 0; i < 20 && inventory().some(sameProcess); i++) await new Promise(r => setTimeout(r, 100));
    }
    for (const signal of ['SIGTERM', 'SIGKILL']) {
      const remaining = inventory();
      if (remaining.length) {
        assert(remaining.every(sameProcess), 'refusing to signal an unrecognized process group member');
        process.kill(-child.pid, signal);
        for (let i = 0; i < 20 && inventory().some(sameProcess); i++) await new Promise(r => setTimeout(r, 100));
      }
    }
    assert.equal(inventory().length, 0, 'owned browser descendants remain');
    results.cleanupOwnedBrowserExited = true;
  } catch (error) { cleanupErrors.push(String(error)); }
  try {
    for (const sockets of [proxySockets, trapSockets]) for (const socket of sockets) socket.destroy();
    const close = server => new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('owned server close timeout')), 3000);
      server.close(error => { clearTimeout(timer); error ? reject(error) : resolve(); });
    });
    await Promise.all([close(proxy), close(trap), new Promise(r => udp.close(r))]);
    assert.equal(proxySockets.size + trapSockets.size, 0, 'owned HTTP sockets remain');
    results.cleanupServersClosed = true;
  } catch (error) { cleanupErrors.push(String(error)); }
  for (const p of pending.values()) { clearTimeout(p.timer); p.reject(new Error('probe shutdown')); }
  pending.clear();
  results.cleanupErrors = cleanupErrors;
  if (cleanupErrors.length && !failure) failure = new Error(cleanupErrors.join('\n'));
  if (errors.length && !failure) failure = new Error(errors.join('\n'));
  results.status = failure ? 'FAIL_SYNTHETIC_SPIKE' : 'PASS_SYNTHETIC_SPIKE';
  try {
    writeFileSync(join(root, 'result.json'), JSON.stringify({ results, events, errors, failure: failure ? String(failure) : null }, null, 2));
    writeFileSync(join(root, 'chrome.log'), stderr);
  } catch (error) {
    if (!failure) failure = error;
    results.status = 'FAIL_EVIDENCE_WRITE';
  }
  console.log(JSON.stringify({ root, results }, null, 2));
  if (failure) throw failure;
}
