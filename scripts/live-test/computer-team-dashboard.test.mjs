import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

test('the real launcher excludes inherited homes, event paths and credential variables before spawn', () => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'dudu-team-launcher-check-')));
  const inherited = join(root, 'inherited-do-not-write');
  mkdirSync(inherited, { mode: 0o700 });
  writeFileSync(join(inherited, '.healthcheck'), 'untouched');
  const fake = join(root, 'synthetic-test-process');
  writeFileSync(fake, `#!/usr/bin/env node
const fs = require('node:fs');
fs.writeFileSync(process.env.EVOLUTION_EVENTS_DIR + '/.healthcheck', 'fresh-only');
process.stdout.write(JSON.stringify({home:process.env.HOME,dudu:process.env.DUDUCLAW_HOME,evolution:process.env.EVOLUTION_EVENTS_DIR,keys:Object.keys(process.env)}) + '\\n');
`);
  chmodSync(fake, 0o700);
  const launcher = join(dirname(fileURLToPath(import.meta.url)), 'computer-team-dashboard.mjs');
  const env = Object.fromEntries(['PATH', 'TMPDIR', 'LANG'].filter((key) => process.env[key]).map((key) => [key, process.env[key]]));
  Object.assign(env, {
    HOME: inherited, DUDUCLAW_HOME: inherited, EVOLUTION_EVENTS_DIR: inherited,
    OPENAI_API_KEY: 'synthetic-must-not-inherit', GOOGLE_APPLICATION_CREDENTIALS: 'synthetic-must-not-inherit',
    DUDUCLAW_AGENT_ID: 'synthetic-must-not-inherit', BASH_ENV: 'synthetic-must-not-inherit',
  });
  const child = spawnSync(process.execPath, [launcher, fake, 'fixture/image:synthetic'], { env, encoding: 'utf8', timeout: 20_000 });
  assert.equal(child.status, 0, child.stderr);
  const fixture = child.stdout.match(/^TEAM_NATIVE_ROOT (.+)$/m)?.[1];
  assert(fixture);
  const captured = JSON.parse(readFileSync(join(fixture, 'native.log'), 'utf8'));
  assert.equal(captured.home, join(fixture, 'os-home'));
  assert.equal(captured.dudu, join(fixture, 'data'));
  assert.equal(captured.evolution, join(fixture, 'evolution'));
  for (const key of ['OPENAI_API_KEY', 'GOOGLE_APPLICATION_CREDENTIALS', 'DUDUCLAW_AGENT_ID', 'BASH_ENV']) assert(!captured.keys.includes(key));
  assert.equal(readFileSync(join(inherited, '.healthcheck'), 'utf8'), 'untouched');
  assert.equal(readFileSync(join(captured.evolution, '.healthcheck'), 'utf8'), 'fresh-only');
  assert.equal(JSON.parse(readFileSync(join(fixture, 'launcher-result.json'), 'utf8')).exit_code, 0);
});
