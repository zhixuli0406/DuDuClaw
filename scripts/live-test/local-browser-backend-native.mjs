#!/usr/bin/env node
// Run only after independent approval of this launcher + matching test source.
// Real Chrome/native sandbox, synthetic native app/presence/page; no real login.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';

const repo = fs.realpathSync(path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..'));
const arguments_ = process.argv.slice(2);
const argument = name => { const at = arguments_.indexOf(name); return at < 0 ? null : arguments_[at + 1]; };
const probe = argument('--probe') ?? 'backend';
if (!['backend', 'mask-race'].includes(probe)) throw Error('unknown native probe');
const testName = probe === 'mask-race'
  ? 'computer_use_sessions::local_browser::native_fixture::actual_local_masking_reflow_does_not_expose_a_sensitive_field'
  : 'computer_use_sessions::local_browser::native_fixture::actual_local_backend_uses_the_signed_native_lane_and_hardened_chrome';
if (!arguments_.includes('--run-reviewed-native')) throw Error('independent frozen-source approval is required before execution');
const executable = fs.realpathSync(argument('--test-binary') ?? '');
if (!executable.startsWith(path.join(repo, 'target/debug/deps') + path.sep)
    || !/^duduclaw_gateway-[0-9a-f]+$/.test(path.basename(executable))) throw Error('only this worktree test binary is eligible');
const expected = argument('--sha256');
if (!/^[0-9a-f]{64}$/.test(expected ?? '')) throw Error('the exact freshly built test-binary SHA256 is required');
const hash = crypto.createHash('sha256');
for await (const bytes of fs.createReadStream(executable)) hash.update(bytes);
if (hash.digest('hex') !== expected) throw Error('test binary hash changed');

const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'ddc-b9b-native-')));
fs.chmodSync(root, 0o700);
fs.writeFileSync(path.join(root, 'b9b-native-fixture-owned-root'), 'synthetic-only\n', { flag: 'wx', mode: 0o600 });
// Whitelist applies from the first subprocess. No shell, BASH_ENV, host
// identity, real HOME, proxy settings or provider credentials are inherited.
const environment = Object.freeze({ PATH: '/usr/bin:/bin:/usr/sbin:/sbin', HOME: root,
  DUDUCLAW_HOME: root, TMPDIR: root, LANG: 'C', LC_ALL: 'C', RUST_BACKTRACE: '0' });
const owned = new Set();
const errors = [];
let output;
let code = null;
let native;
let cleanup = { cleanup_ok: false, error: 'not checked' };
let interrupt;
const interrupted = new Promise(resolve => { interrupt = resolve; });
const onInterrupt = signal => { errors.push(`received ${signal}`); interrupt(); };
const onInt = () => onInterrupt('SIGINT');
const onTerm = () => onInterrupt('SIGTERM');
process.on('SIGINT', onInt);
process.on('SIGTERM', onTerm);

function launch(program, args, options = {}) {
  const child = spawn(program, args, { cwd: repo, env: environment, stdio: ['ignore', 'pipe', 'pipe'], ...options });
  owned.add(child);
  const closed = new Promise(resolve => {
    child.once('error', error => { errors.push(`owned child error: ${error.code ?? error.name}`); });
    child.once('close', (exit, signal) => { owned.delete(child); resolve({ exit, signal }); });
  });
  // Every owned pipe has an error handler; no unknown emitter error is ignored.
  for (const stream of [child.stdin, child.stdout, child.stderr]) stream?.on('error', error => errors.push(`owned pipe error: ${error.code ?? error.name}`));
  return { child, closed };
}
const timeout = (promise, milliseconds) => new Promise((resolve, reject) => {
  const timer = setTimeout(() => reject(Error('owned child deadline exceeded')), milliseconds);
  promise.then(value => { clearTimeout(timer); resolve(value); }, error => { clearTimeout(timer); reject(error); });
});
async function stop(item) {
  if (!item) return;
  const alive = () => item.child.pid && item.child.exitCode === null && item.child.signalCode === null;
  if (alive()) { try { item.child.kill('SIGTERM'); } catch (error) { errors.push(`owned TERM failed: ${error.code ?? error.name}`); } }
  try { await timeout(item.closed, 2500); return; } catch { /* bounded escalation for this owned child only */ }
  if (alive()) { try { item.child.kill('SIGKILL'); } catch (error) { errors.push(`owned KILL failed: ${error.code ?? error.name}`); } }
  await timeout(item.closed, 5000);
}
async function cleanupBrowsers(mode) {
  const manifest = path.join(root, 'owned-processes.json');
  if (!fs.existsSync(manifest)) return { cleanup_ok: false, error: 'native child inventory missing; run is invalid' };
  let item;
  try {
    item = launch('/usr/bin/python3', ['-I', path.join(repo, 'scripts/live-test/local-browser-native-cleanup.py'), '--manifest', manifest, '--mode', mode]);
    const chunks = []; let size = 0;
    item.child.stdout.on('data', bytes => { size += bytes.length; if (size > 65536) { errors.push('oversized owned cleanup response'); try { item.child.kill('SIGKILL'); } catch { errors.push('owned cleanup child signal failed'); } } else chunks.push(bytes); });
    item.child.stderr.on('data', bytes => { try { fs.appendFileSync(path.join(root, 'cleanup-stderr.log'), bytes); } catch { errors.push('owned cleanup stderr write failed'); } });
    const result = await timeout(item.closed, 15000);
    const body = JSON.parse(Buffer.concat(chunks).toString());
    return result.exit === 0 && result.signal === null ? body : { ...body, cleanup_ok: false };
  } finally { await stop(item); }
}

try {
  output = fs.openSync(path.join(root, 'native-test.log'), 'wx', 0o600);
  native = launch(executable, ['--ignored', '--exact', testName, '--test-threads=1', '--nocapture'], { stdio: ['ignore', output, output] });
  const result = await Promise.race([timeout(native.closed, 180000), interrupted.then(() => { throw Error('native fixture interrupted'); })]);
  code = result.exit;
  if (result.signal !== null) errors.push(`native test exited by ${result.signal}`);
} catch (error) {
  errors.push(error.message);
} finally {
  try { await stop(native); } catch (error) { errors.push(`native child cleanup: ${error.message}`); }
  try {
    cleanup = await cleanupBrowsers('check');
    if (cleanup.cleanup_ok !== true) {
      const before = cleanup;
      cleanup = { ...await cleanupBrowsers('terminate'), required_fallback: true, before };
    }
  } catch (error) { errors.push(`browser cleanup: ${error.message}`); }
  if (output !== undefined) { try { fs.closeSync(output); } catch (error) { errors.push(`output close: ${error.message}`); } }
  if (owned.size !== 0) errors.push('owned child set is not empty after cleanup');
  process.off('SIGINT', onInt);
  process.off('SIGTERM', onTerm);
}
// Evidence and PASS are emitted only after verified cleanup. An evidence I/O
// failure cannot bypass the finally block or turn the result into PASS.
const leasePath = path.join(root, 'owned-machine-lease.lock');
let leaseClean = false;
try {
  const lease = fs.lstatSync(leasePath);
  leaseClean = lease.isFile() && !lease.isSymbolicLink() && fs.realpathSync(leasePath) === leasePath
    && lease.uid === process.getuid() && (lease.mode & 0o077) === 0 && lease.size === 0;
  if (!leaseClean) errors.push('owned fixture machine lease is unsafe or cleanup remains unverified');
} catch { errors.push('owned fixture machine lease evidence is missing'); }
const passedStatus = probe === 'mask-race' ? 'PASS_SYNTHETIC_MASK_PROBE_ONLY' : 'PASS_SYNTHETIC_BACKEND_ONLY';
const status = code === 0 && cleanup.cleanup_ok === true && cleanup.required_fallback !== true && leaseClean && errors.length === 0 ? passedStatus : 'FAIL_OR_INVALID';
const result = { status, probe, root, test_binary_sha256: expected, exit: code, cleanup, errors,
  machine_lease: { path: leasePath, owned_fixture_only: true, clean: leaseClean },
  boundary: 'Real Chrome/backend/pipe/profile/guards; synthetic app key, presence, host human proof, pages and cfg(test) owned machine lease; no real global uid lease, physical lock, provider, signed Tauri app or release acceptance' };
try { fs.writeFileSync(path.join(root, 'result.json'), JSON.stringify(result, null, 2) + '\n', { flag: 'wx', mode: 0o600 }); }
catch { result.status='FAIL_OR_INVALID'; errors.push('evidence write failed after cleanup'); console.error('Evidence write failed after cleanup'); process.exitCode = 1; }
console.log(JSON.stringify(result));
if (result.status !== passedStatus) process.exitCode = 1;
