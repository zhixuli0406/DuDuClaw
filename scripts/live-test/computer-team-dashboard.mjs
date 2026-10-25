#!/usr/bin/env node
// Launch the ignored Rust fixture in a fresh subprocess. This script never
// inherits identity, token, provider, evolution or production-home variables.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { closeSync, mkdirSync, mkdtempSync, openSync, realpathSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const binary = realpathSync(resolve(process.argv[2] ?? 'target/debug/deps/required-test-binary'));
const image = process.argv[3];
assert(image && /^[A-Za-z0-9][A-Za-z0-9_./:@-]+$/.test(image), 'pass the explicit locally validated fixture image');
const root = realpathSync(mkdtempSync(join(tmpdir(), 'dudu-team-native-')));
const paths = { HOME: join(root, 'os-home'), DUDUCLAW_HOME: join(root, 'data'), EVOLUTION_EVENTS_DIR: join(root, 'evolution') };
for (const path of Object.values(paths)) mkdirSync(path, { mode: 0o700 });
const env = Object.fromEntries(['PATH', 'TMPDIR', 'LANG'].filter((key) => process.env[key]).map((key) => [key, process.env[key]]));
Object.assign(env, paths, { DUDU_TEAM_NATIVE_FIXTURE_ROOT: root, DUDU_COMPUTER_USE_IMAGE: image });
// Docker's socket is the only optional inherited connection. Do not inherit
// DOCKER_CONFIG, contexts, certificates, or arbitrary client environment.
if (process.env.DOCKER_HOST) {
  assert(/^unix:\/\/\//.test(process.env.DOCKER_HOST) && !/[\r\n\0]/.test(process.env.DOCKER_HOST), 'fixture Docker host must be a local Unix socket');
  env.DOCKER_HOST = process.env.DOCKER_HOST;
}
for (const [key, path] of Object.entries(paths)) assert.equal(realpathSync(env[key]), path);
writeFileSync(join(root, 'launcher.json'), JSON.stringify({ binary, image, root, isolated_paths: paths, environment_keys: Object.keys(env).sort() }, null, 2), { mode: 0o600 });
console.log(`TEAM_NATIVE_ROOT ${root}`);
console.log(`TEAM_NATIVE_LOG ${join(root, 'native.log')}`);
const fd = openSync(join(root, 'native.log'), 'wx', 0o600);
let child;
try {
  child = spawn(binary, ['--ignored', '--exact', 'computer_use_sessions::tests::team_docker::computer_team_native_dashboard_fixture', '--nocapture', '--test-threads=1'], { env, stdio: ['ignore', fd, fd] });
} finally { closeSync(fd); }
const result = await new Promise((done) => {
  child.once('error', (error) => done({ exit_code: null, signal: null, error: error.message }));
  child.once('exit', (exit_code, signal) => done({ exit_code, signal, error: null }));
});
writeFileSync(join(root, 'launcher-result.json'), JSON.stringify({ ...result, boundary: 'synthetic host requester; real isolated gateway/container; no provider or physical-user acceptance' }, null, 2), { mode: 0o600 });
if (result.exit_code !== 0 || result.error || result.signal) process.exitCode = 1;
