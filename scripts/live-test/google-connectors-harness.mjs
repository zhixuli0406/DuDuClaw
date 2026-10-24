// Lifecycle for the local Google fixture. Never inherit the operator's env.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { tmpdir } from 'node:os';

export function minimalEnv(home, path, extra = {}) {
  const allowed = new Set(['DUDUCLAW_HOME', 'DUDUCLAW_BIN', 'DUDUCLAW_LIVE_TEST_GOOGLE_API_BASE', 'DUDUCLAW_MCP_API_KEY', 'DUDUCLAW_AGENT_ID', 'DUDUCLAW_AGENT_TOKEN']);
  const env = { HOME: home, PATH: path, TMPDIR: tmpdir(), LANG: 'C' };
  for (const [name, value] of Object.entries(extra)) {
    assert(allowed.has(name), `fixture environment variable is not allowed: ${name}`);
    if (value !== undefined) env[name] = String(value);
  }
  return env;
}

export async function deadline(promise, ms, label) {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${label} timed out`)), ms); })]); }
  finally { clearTimeout(timer); }
}

const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function untilClosed(record, ms) {
  const end = performance.now() + ms;
  while (!record.closed || groupAlive(record)) {
    if (performance.now() >= end) throw new Error(`child ${record.child.pid ?? 'not spawned'} did not stop`);
    await pause(10);
  }
}
function groupAlive(record) {
  if (!record.child.pid) return false;
  try { process.kill(-record.child.pid, 0); return true; }
  catch (error) { if (error.code === 'ESRCH') return false; throw error; }
}
function signalGroup(record, signal) {
  if (!record.child.pid) return;
  try { process.kill(-record.child.pid, signal); }
  catch (error) { if (error.code !== 'ESRCH') throw error; }
}

export class OwnedResources {
  children = new Set();
  closers = [];
  constructor({ termMs = 5000, killMs = 5000 } = {}) {
    assert(process.platform !== 'win32', 'the fixture uses POSIX owned process groups');
    this.termMs = termMs; this.killMs = killMs;
  }
  spawn(command, args, env, options = {}) {
    assert(env && typeof env.HOME === 'string' && typeof env.PATH === 'string', 'explicit minimal child environment required');
    assert(!('BASH_ENV' in env) && !('ENV' in env), 'shell startup environment is forbidden');
    const child = spawn(command, args, { ...options, env, detached: true });
    const record = { child, closed: false, error: undefined, closing: undefined };
    record.completion = new Promise((resolve) => {
      child.on('error', (error) => { record.error = error; });
      child.once('close', (code, signal) => {
        record.closed = true; record.code = code; record.signal = signal;
        resolve({ code, signal, error: record.error });
      });
    });
    this.children.add(record);
    return record;
  }
  async success(record, ms, label) {
    const result = await deadline(record.completion, ms, label);
    if (result.error) throw result.error;
    assert.equal(result.signal, null, `${label} killed by ${result.signal}`);
    assert.equal(result.code, 0, `${label} exited ${result.code}`);
    return result;
  }
  defer(close, { beforeChildren = false } = {}) { this.closers.push({ close, beforeChildren }); }
  async stop(record) {
    if (record.closing) return record.closing;
    record.closing = (async () => {
      // close confirms stdio drained as well as exitCode/signalCode. A child
      // killed by a signal has exitCode=null; that must not mean it is alive.
      if (record.closed && !groupAlive(record)) return;
      signalGroup(record, 'SIGTERM');
      try { await untilClosed(record, this.termMs); }
      catch (termError) {
        signalGroup(record, 'SIGKILL');
        try { await untilClosed(record, this.killMs); }
        catch (killError) { throw new AggregateError([termError, killError], 'owned child cleanup failed'); }
      }
      assert(record.closed && (record.error || record.child.exitCode !== null || record.child.signalCode !== null), 'owned child exit was not confirmed');
    })();
    return record.closing;
  }
  async cleanup() {
    const failures = [];
    const run = async (fn) => { try { await fn(); } catch (error) { failures.push(error); } };
    for (const closer of this.closers.toReversed().filter((item) => item.beforeChildren)) await run(closer.close);
    for (const record of [...this.children].reverse()) await run(() => this.stop(record));
    for (const closer of this.closers.toReversed().filter((item) => !item.beforeChildren)) await run(closer.close);
    if (failures.length) throw new AggregateError(failures, 'fixture cleanup failed');
  }
}

export async function withOwnedResources(work, options) {
  const owned = new OwnedResources(options);
  let result, workError;
  try { result = await work(owned); } catch (error) { workError = error; }
  try { await owned.cleanup(); }
  catch (cleanupError) { throw new AggregateError(workError ? [workError, cleanupError] : [cleanupError], 'fixture failed during cleanup'); }
  if (workError) throw workError;
  return result;
}

export async function closeServer(server) {
  server.closeAllConnections();
  if (!server.listening) return;
  await deadline(new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve())), 5000, 'local server cleanup');
}
