'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { EventEmitter } = require('node:events');
const source = fs.readFileSync(require('node:path').join(__dirname, 'recording_driver.cjs'), 'utf8');

async function fixture(when, env = {}) {
  const input = new EventEmitter();
  input.setEncoding = input.resume = () => {};
  let loaded = 0;
  let launched = 0;
  let navigated = 0;
  let result;
  const writes = new Map();
  let resolveExit;
  const done = new Promise(resolve => { resolveExit = resolve; });
  const context = {
    tracing: { start: async () => {}, stop: async () => {} },
    exposeBinding: async () => {}, addInitScript: async () => {}, on: () => {}, close: async () => {},
    newPage: async () => ({ goto: async () => { navigated++; input.emit('data', 'stop\n'); } })
  };
  const browser = { newContext: async () => context, close: async () => {} };
  const root = env.DUDUCLAW_RECORD_ROOT || '/runtime';
  const contained = env.DUDUCLAW_RECORD_CONTAINED === '1';
  let launchArgs;
  const fakeProcess = { stdin: input, env: { DUDUCLAW_RECORD_PROXY: 'http://127.0.0.1:1234', ...env },
    stdout: { write: (bytes, callback) => { result = JSON.parse(bytes); callback(); } },
    exit: code => resolveExit(code) };
  const sandbox = { URL, setImmediate, setTimeout: () => 1, clearTimeout: () => {}, process: fakeProcess,
    require(name) {
      if (name === 'node:fs') return {
        readFileSync: () => JSON.stringify({ id: 'ab'.repeat(16), url: 'https://example.com', max_seconds: 1 }),
        writeFileSync: (path, bytes) => writes.set(path, bytes)
      };
      assert.equal(name, `${root}/playwright.cjs`); loaded++;
      return { chromium: { launch: async options => {
        launched++; launchArgs = options.args;
        assert.equal(options.chromiumSandbox, !contained);
        assert.equal(options.executablePath, `${root}/chromium`);
        assert.equal(options.proxy.server, fakeProcess.env.DUDUCLAW_RECORD_PROXY);
        assert.ok(!options.args.includes('--no-sandbox'));
        assert.ok(options.args.includes('--disable-dev-shm-usage'));
        assert.ok(!options.args.some(arg => arg.startsWith('--remote-debugging-port')));
        assert.equal(options.args.includes('--js-flags=--jitless'), contained);
        if (when === 'launch') input.emit('data', 'stop\n');
        return browser;
      } } };
    }
  };
  vm.runInNewContext(source, sandbox);
  if (when === 'queued') input.emit('data', 'stop\n');
  const code = await done;
  return { code, loaded, launched, navigated, result, writes, launchArgs };
}

test('stop buffered before launch does not load a browser or fabricate artifacts', async () => {
  const observed = await fixture('queued');
  assert.equal(observed.code, 0);
  assert.equal(observed.loaded, 0); assert.equal(observed.launched, 0); assert.equal(observed.navigated, 0);
  assert.deepEqual(observed.result, { reason: 'stop', actions: 0, started: false, navigation: 'not_attempted' });
  assert.equal(observed.writes.size, 0);
});
test('stop during launch never navigates and flushes the real empty context', async () => {
  const observed = await fixture('launch');
  assert.equal(observed.code, 0); assert.equal(observed.launched, 1); assert.equal(observed.navigated, 0);
  assert.equal(observed.result.started, true); assert.equal(observed.result.reason, 'stop');
  assert.equal(observed.result.navigation, 'not_attempted');
  assert.equal(observed.writes.size, 2);
});
test('HX6 contained in-run layout: /helpers files, call scratch, no Chromium sandbox, jitless', async () => {
  const dir = '/helper-tmp/0123456789abcdef0123456789abcdef/recording';
  const observed = await fixture('navigation', { DUDUCLAW_RECORD_DIR: dir, DUDUCLAW_RECORD_ROOT: '/helpers',
    DUDUCLAW_RECORD_CONTAINED: '1' });
  assert.equal(observed.code, 0); assert.equal(observed.launched, 1);
  assert.deepEqual([...observed.writes.keys()].sort(), [`${dir}/actions.json`, `${dir}/network.json`]);
  assert.ok(observed.launchArgs.includes('--disable-dev-shm-usage'));
});
test('stop during navigation flushes only bounded masked action/network metadata', async () => {
  const observed = await fixture('navigation');
  assert.equal(observed.code, 0); assert.equal(observed.navigated, 1);
  assert.deepEqual(JSON.parse(observed.writes.get('/tmp/recording/network.json')), { responses: [] });
  assert.deepEqual(JSON.parse(observed.writes.get('/tmp/recording/actions.json')), { actions: [] });
});
