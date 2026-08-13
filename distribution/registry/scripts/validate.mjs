#!/usr/bin/env node
// Registry entry validator — zero dependencies so CI is `node scripts/validate.mjs`.
// Hand-rolled checks mirroring schema/entry.schema.json (the schema file is the
// human/tooling reference; this file is the enforcement — keep both in sync).
// Fails closed: any problem in any entry exits non-zero with a full report.
import { readFileSync, readdirSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const indexDir = join(root, 'index');
const problems = [];
const slugs = new Set();

const entries = readdirSync(indexDir).filter((f) => f.endsWith('.json') && !f.startsWith('_'));
for (const file of entries) {
  const p = (msg) => problems.push(`${file}: ${msg}`);
  let e;
  try {
    e = JSON.parse(readFileSync(join(indexDir, file), 'utf8'));
  } catch (err) {
    p(`invalid JSON — ${err.message}`);
    continue;
  }

  const req = ['slug', 'kind', 'title', 'description', 'publisher', 'license', 'version', 'archive_url', 'sha256'];
  for (const k of req) if (e[k] == null) p(`missing required field: ${k}`);

  if (e.slug != null) {
    if (!/^[a-z0-9][a-z0-9-]{1,63}$/.test(e.slug)) p(`bad slug: ${e.slug}`);
    if (file !== `${e.slug}.json`) p(`file name must be <slug>.json (slug=${e.slug})`);
    if (slugs.has(e.slug)) p(`duplicate slug: ${e.slug}`);
    slugs.add(e.slug);
  }
  if (e.kind != null && e.kind !== 'pack') p(`unsupported kind: ${e.kind}`);
  if (e.title != null && (e.title.length < 3 || e.title.length > 80)) p('title length out of 3–80');
  if (e.description != null && (e.description.length < 10 || e.description.length > 400)) p('description length out of 10–400');
  if (e.publisher != null && !/^[A-Za-z0-9](?:[A-Za-z0-9]|-(?=[A-Za-z0-9])){0,38}$/.test(e.publisher)) p(`bad publisher: ${e.publisher}`);
  if (e.version != null && !/^[0-9]+\.[0-9]+\.[0-9]+$/.test(e.version)) p(`bad semver: ${e.version}`);
  if (e.sha256 != null && !/^[0-9a-f]{64}$/.test(e.sha256)) p('sha256 must be 64 lowercase hex chars');
  for (const k of ['archive_url', 'minisig_url', 'homepage']) {
    if (e[k] != null && !/^https:\/\//.test(e[k])) p(`${k} must be https`);
  }

  // Two-lane trust model: executable content ⇒ signature mandatory, and the
  // publisher must have a registered minisign key in publishers/<user>/.
  const codeLane = Boolean(e.contains?.hooks || e.contains?.skills);
  if (codeLane) {
    if (!e.minisig_url) p('code lane (hooks/skills present) requires minisig_url');
    const keyPath = join(root, 'publishers', e.publisher ?? '', 'minisign.pub');
    if (!existsSync(keyPath)) p(`code lane requires a registered key at publishers/${e.publisher}/minisign.pub`);
  }
}

if (problems.length) {
  console.error(`✗ ${problems.length} problem(s):`);
  for (const m of problems) console.error(`  - ${m}`);
  process.exit(1);
}
console.log(`✓ ${entries.length} entrie(s) valid`);
