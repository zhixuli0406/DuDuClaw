'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { EventEmitter } = require('node:events');
const source = fs.readFileSync(require('node:path').join(__dirname, 'recording_driver.cjs'), 'utf8');

async function fixture(when) {
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
  const fakeProcess = { stdin: input, env: { DUDUCLAW_RECORD_PROXY: 'http://127.0.0.1:1234' },
    stdout: { write: (bytes, callback) => { result = JSON.parse(bytes); callback(); } },
    exit: code => resolveExit(code) };
  const sandbox = { URL, setImmediate, setTimeout: () => 1, clearTimeout: () => {}, process: fakeProcess,
    require(name) {
      if (name === 'node:fs') return {
        readFileSync: () => JSON.stringify({ id: 'ab'.repeat(16), url: 'https://example.com', max_seconds: 1 }),
        writeFileSync: (path, bytes) => writes.set(path, bytes)
      };
      assert.equal(name, '/runtime/playwright.cjs'); loaded++;
      return { chromium: { launch: async options => {
        launched++;
        assert.equal(options.chromiumSandbox, true);
        assert.equal(options.executablePath, '/runtime/chromium');
        assert.equal(options.proxy.server, fakeProcess.env.DUDUCLAW_RECORD_PROXY);
        assert.ok(!options.args.includes('--no-sandbox'));
        if (when === 'launch') input.emit('data', 'stop\n');
        return browser;
      } } };
    }
  };
  vm.runInNewContext(source, sandbox);
  if (when === 'queued') input.emit('data', 'stop\n');
  const code = await done;
  return { code, loaded, launched, navigated, result, writes };
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
test('stop during navigation flushes only bounded masked action/network metadata', async () => {
  const observed = await fixture('navigation');
  assert.equal(observed.code, 0); assert.equal(observed.navigated, 1);
  assert.deepEqual(JSON.parse(observed.writes.get('/tmp/recording/network.json')), { responses: [] });
  assert.deepEqual(JSON.parse(observed.writes.get('/tmp/recording/actions.json')), { actions: [] });
});
