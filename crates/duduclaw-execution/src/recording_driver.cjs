'use strict';
// Embedded in the signed helper. HX6: the Rust parent sets the in-run layout
// (scratch under /helper-tmp/<call>, signed files under /helpers) and starts
// this process under the nested browser Landlock (no /work, TCP connect only
// to the forwarder port on Landlock ABI >= 4). Chromium's own sandbox cannot
// start under the common filter (no user namespaces, NNP), so in that
// contained mode the helper sandbox is the containment.
const fs = require('node:fs');
const dir = process.env.DUDUCLAW_RECORD_DIR || '/tmp/recording';
const root = process.env.DUDUCLAW_RECORD_ROOT || '/runtime';
const contained = process.env.DUDUCLAW_RECORD_CONTAINED === '1';
const cfg = JSON.parse(fs.readFileSync(`${dir}/request.json`, 'utf8'));
const actions = [];
const network = [];
let finish;
let stopReason = null;
const stopped = new Promise(resolve => { finish = reason => { stopReason = reason; resolve(reason); }; });
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => {
  input += chunk;
  if (input === 'stop\n') finish('stop');
  else if (!'stop\n'.startsWith(input)) process.exit(64);
});
process.stdin.on('end', () => { if (input !== 'stop\n') process.exit(74); });
process.stdin.resume();
const deadline = setTimeout(() => finish('deadline'), cfg.max_seconds * 1000);
function origin(raw) {
  try { const u = new URL(raw); return ['http:', 'https:'].includes(u.protocol) ? u.origin : '<other>'; }
  catch { return '<invalid>'; }
}
async function main() {
  // Let an already buffered stop reach the listener before loading dependencies
  // or launching a browser. An early stop produces no fabricated artifacts.
  await new Promise(resolve => setImmediate(resolve));
  if (stopReason) {
    clearTimeout(deadline);
    process.stdout.write(JSON.stringify({ reason: stopReason, actions: 0, started: false, navigation: 'not_attempted' }), () => process.exit(0));
    return;
  }
  const { chromium } = require(`${root}/playwright.cjs`);
  // Playwright talks to Chromium over its default pipe transport; no DevTools
  // TCP port is ever opened. Shared memory goes to TMPDIR (the call scratch),
  // never /dev/shm. Contained mode also runs V8 without JIT (less exploit
  // surface for page script; cost: slower page JavaScript, acceptable here).
  const args = ['--proxy-bypass-list=<-loopback>', '--disable-quic', '--disable-background-networking',
    '--disable-dev-shm-usage'];
  if (contained) args.push('--js-flags=--jitless');
  const browser = await chromium.launch({ headless: true, chromiumSandbox: !contained,
    executablePath: `${root}/chromium`, proxy: { server: process.env.DUDUCLAW_RECORD_PROXY, bypass: '' }, args });
  const context = await browser.newContext({ serviceWorkers: 'block', acceptDownloads: false });
  await context.tracing.start({ screenshots: true, snapshots: true, sources: false });
  await context.exposeBinding('__ddcRecord', (_source, action) => {
    if (actions.length < 1000 && action && ['click', 'fill', 'submit'].includes(action.kind)) {
      actions.push({ kind: action.kind, tag: String(action.tag || '').slice(0, 32) });
    }
  });
  await context.addInitScript(() => {
    for (const kind of ['click', 'change', 'submit']) {
      window.addEventListener(kind, event => {
        window.__ddcRecord({ kind: kind === 'change' ? 'fill' : kind,
          tag: String(event.target && event.target.tagName || '').toLowerCase() }).catch(() => {});
      }, true);
    }
  });
  context.on('response', response => {
    if (network.length < 1000) network.push({ origin: origin(response.url()), status: response.status() });
  });
  const page = await context.newPage();
  // A stop during navigation must flush too. Never send raw navigation errors,
  // URLs, headers, cookies, request bodies or typed values to the gateway.
  let navigation = 'not_attempted';
  if (!stopReason) navigation = await Promise.race([
    page.goto(cfg.url, { waitUntil: 'domcontentloaded', timeout: 30000 }).then(() => 'completed', () => 'failed'),
    stopped.then(() => 'interrupted')
  ]);
  const reason = await stopped;
  await context.tracing.stop({ path: `${dir}/trace.zip` });
  await context.close();
  await browser.close();
  fs.writeFileSync(`${dir}/actions.json`, JSON.stringify({ actions }), { flag: 'wx', mode: 0o600 });
  fs.writeFileSync(`${dir}/network.json`, JSON.stringify({ responses: network }), { flag: 'wx', mode: 0o600 });
  clearTimeout(deadline);
  process.stdout.write(JSON.stringify({ reason, actions: actions.length, started: true, navigation }), () => process.exit(0));
}
main().catch(() => process.exit(74));
