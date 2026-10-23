import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, writeFileSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { OwnedResources, withOwnedResources, minimalEnv, closeServer, deadline } from './google-connectors-harness.mjs';

async function childReady(owned, program, env) {
  const record = owned.spawn(process.execPath, ['-e', program], env, { stdio: ['ignore', 'pipe', 'pipe'] });
  const [data] = await deadline(once(record.child.stdout, 'data'), 3000, 'controlled child ready');
  assert.equal(data.toString().trim(), 'ready');
  return record;
}
function gone(record) {
  assert(record.closed);
  assert(record.child.exitCode !== null || record.child.signalCode !== null);
  assert.throws(() => process.kill(record.child.pid, 0), { code: 'ESRCH' });
}

test('first shell does not inherit BASH_ENV, ENV, model keys or other ambient variables', async () => {
  const home = mkdtempSync(join(tmpdir(), 'ddc-harness-env-'));
  const marker = join(home, 'ambient-startup-ran');
  const startup = join(home, 'startup.sh');
  writeFileSync(startup, `printf contaminated > '${marker}'\n`);
  try {
    // Contaminate only a fresh outer Node, with explicitly synthetic values.
    // The test process never reads or saves a host model credential.
    const shellCheck = 'test -z "${OPENAI_API_KEY+x}"';
    const program = `
      import assert from 'node:assert/strict';
      import { minimalEnv, withOwnedResources } from ${JSON.stringify(new URL('./google-connectors-harness.mjs', import.meta.url).href)};
      await withOwnedResources(async (owned) => {
        const env = minimalEnv(${JSON.stringify(home)}, '/usr/bin:/bin');
        assert(!('BASH_ENV' in env) && !('ENV' in env) && !('OPENAI_API_KEY' in env));
        const child = owned.spawn('/bin/bash', ['-c', ${JSON.stringify(shellCheck)}], env, { stdio: 'ignore' });
        await owned.success(child, 3000, 'controlled first shell');
      });
    `;
    const outer = spawnSync(process.execPath, ['--input-type=module', '-e', program], {
      env: { ...minimalEnv(home, '/usr/bin:/bin'), BASH_ENV: startup, ENV: startup, OPENAI_API_KEY: 'synthetic-ambient-only' },
      encoding: 'utf8', timeout: 5000, killSignal: 'SIGKILL',
    });
    assert.equal(outer.status, 0, outer.stderr); assert.equal(outer.signal, null);
    assert(!existsSync(marker));
  } finally { rmSync(home, { recursive: true }); }
});

test('late TERM exit is awaited before the successful result is published', async () => {
  let record;
  const start = performance.now();
  const result = await withOwnedResources(async (owned) => {
    record = await childReady(owned, "process.on('SIGTERM',()=>setTimeout(()=>process.exit(0),120));console.log('ready');setInterval(()=>{},1000)", minimalEnv(tmpdir(), '/usr/bin:/bin'));
    return { pass: true };
  }, { termMs: 1500, killMs: 1500 });
  gone(record); assert.equal(record.child.exitCode, 0);
  assert(performance.now() - start >= 120); assert.equal(result.pass, true);
});

test('TERM-ignoring child is killed and signalCode exit is confirmed before return', async () => {
  let record;
  await withOwnedResources(async (owned) => {
    record = await childReady(owned, "process.on('SIGTERM',()=>{});console.log('ready');setInterval(()=>{},1000)", minimalEnv(tmpdir(), '/usr/bin:/bin'));
  }, { termMs: 80, killMs: 1500 });
  gone(record); assert.equal(record.child.exitCode, null); assert.equal(record.child.signalCode, 'SIGKILL');
});

test('early setup failure closes already-created local server and owned child without a success result', async () => {
  let record, server, published = false;
  await assert.rejects(async () => {
    await withOwnedResources(async (owned) => {
      server = createServer((_req, res) => res.end('synthetic'));
      owned.defer(() => closeServer(server));
      server.listen(0, '127.0.0.1'); await once(server, 'listening');
      record = await childReady(owned, "process.on('SIGTERM',()=>{});console.log('ready');setInterval(()=>{},1000)", minimalEnv(tmpdir(), '/usr/bin:/bin'));
      throw new Error('controlled setup assertion failed');
    }, { termMs: 80, killMs: 1500 });
    published = true;
  }, /controlled setup assertion failed/);
  assert(!published); assert(!server.listening); gone(record);
});

test('cleanup failure prevents success and does not skip remaining owned cleanup', async () => {
  let record, published = false;
  await assert.rejects(async () => {
    await withOwnedResources(async (owned) => {
      record = await childReady(owned, "process.on('SIGTERM',()=>{});console.log('ready');setInterval(()=>{},1000)", minimalEnv(tmpdir(), '/usr/bin:/bin'));
      owned.defer(async () => { throw new Error('controlled cleanup failure'); }, { beforeChildren: true });
      return { pass: true };
    }, { termMs: 80, killMs: 1500 });
    published = true;
  }, /fixture failed during cleanup/);
  assert(!published); gone(record);
});

test('spawn failure is collected without an unhandled error or leftover child', async () => {
  const owned = new OwnedResources({ termMs: 80, killMs: 1500 });
  const record = owned.spawn('/nonexistent/local-fixture-binary', [], minimalEnv(tmpdir(), '/usr/bin:/bin'), { stdio: 'ignore' });
  await assert.rejects(owned.success(record, 1500, 'controlled missing child'), { code: 'ENOENT' });
  await owned.cleanup(); assert(record.closed); assert(!record.child.pid);
});
