#!/usr/bin/env node
// 板模畫廊靜態生成器（WP1.5）— 零依賴：讀 data/templates.json，輸出
// out/index.html 與 out/t/<slug>/index.html。每頁自帶 SEO meta / OG 標籤，
// 是 n8n 模式的「一板模一長尾關鍵字 landing page」。
// 用法：node generate.mjs   （輸出到 ./out，適合直接丟 GitHub Pages / Cloud Run 靜態站）
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const data = JSON.parse(readFileSync(join(here, 'data/templates.json'), 'utf8'));
const out = join(here, 'out');

// WP2.2 收尾：registry index（../registry/index/*.json）的 pack 自動長出
// 畫廊頁——registry 是 SSOT，畫廊零重複維護。底線開頭檔（範例）略過。
import { readdirSync, existsSync } from 'node:fs';
const registryIndex = join(here, '../registry/index');
if (existsSync(registryIndex)) {
  for (const f of readdirSync(registryIndex)) {
    if (!f.endsWith('.json') || f.startsWith('_')) continue;
    try {
      const e = JSON.parse(readFileSync(join(registryIndex, f), 'utf8'));
      data.templates.push({
        slug: e.slug,
        title: e.title,
        industry: (e.categories ?? [])[0] ?? 'pack',
        description: e.description,
        keywords: e.tags ?? [],
        roles: [],
        kind: 'pack',
        install: { registry_slug: e.slug },
      });
    } catch {
      console.warn(`skip unparsable registry entry: ${f}`);
    }
  }
}

const esc = (s) =>
  String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

const shell = (title, desc, canonical, body) => `<!DOCTYPE html>
<html lang="zh-Hant">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${esc(title)}</title>
<meta name="description" content="${esc(desc)}">
<link rel="canonical" href="${esc(canonical)}">
<meta property="og:title" content="${esc(title)}">
<meta property="og:description" content="${esc(desc)}">
<meta property="og:type" content="website">
<style>
  :root { color-scheme: light dark; }
  body { font-family: system-ui, -apple-system, "PingFang TC", sans-serif; max-width: 720px;
         margin: 0 auto; padding: 32px 20px; line-height: 1.75; color: #1c1917; background: #fafaf9; }
  @media (prefers-color-scheme: dark) { body { color: #fafaf9; background: #1c1917; } }
  a { color: #d97706; }
  .card { border: 1px solid rgba(120,113,108,.35); border-radius: 14px; padding: 18px 20px; margin: 14px 0; }
  .tag { display: inline-block; font-size: 12px; border: 1px solid rgba(120,113,108,.4);
         border-radius: 999px; padding: 1px 10px; margin-right: 6px; opacity: .8; }
  pre { background: rgba(120,113,108,.12); padding: 12px 14px; border-radius: 10px; overflow-x: auto; }
  footer { margin-top: 40px; font-size: 13px; opacity: .65; }
  h1 { line-height: 1.3; }
</style>
</head>
<body>
${body}
<footer>🐾 <a href="https://github.com/zhixuli0406/DuDuClaw">DuDuClaw</a> — 自架的 AI 員工平台。LINE 一對一回覆走 Reply API 不計費。</footer>
</body>
</html>`;

const installBlock = (t) => {
  if (t.kind === 'pack' && t.install?.registry_slug) {
    return `<h2>一鍵匯入</h2>
<pre>duduclaw expert install registry:${esc(t.install.registry_slug)}</pre>
<p>安裝時自動驗證 sha256（含 hooks/skills 的包另驗發佈者簽章）。</p>`;
  }
  if (t.kind === 'pack' && t.install?.pack_url) {
    return `<h2>一鍵匯入</h2>
<pre>duduclaw expert install ${esc(t.install.pack_url)}</pre>
<p>或在儀表板「AI 團隊包」頁貼上這個網址安裝。</p>`;
  }
  return `<h2>快速開始</h2>
<pre>${esc(t.install?.wizard ?? 'npx duduclaw onboard')}</pre>
<p>裝好後在通道頁綁 LINE，掃 QR 就能開聊（一對一回覆免費）。</p>`;
};

mkdirSync(join(out, 't'), { recursive: true });

// ── per-template landing pages ──
for (const t of data.templates) {
  const url = `${data.site.base_url}/t/${t.slug}/`;
  const body = `
<p><a href="../../">← ${esc(data.site.title)}</a></p>
<h1>${esc(t.title)}</h1>
<p>${(t.keywords ?? []).map((k) => `<span class="tag">${esc(k)}</span>`).join('')}</p>
<p>${esc(t.description)}</p>
${installBlock(t)}
<h2>包含角色</h2>
<ul>${(t.roles ?? []).map((r) => `<li>${esc(r)}</li>`).join('')}</ul>`;
  const dir = join(out, 't', t.slug);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, 'index.html'), shell(`${t.title}｜DuDuClaw 板模`, t.description, url, body));
}

// ── index ──
const cards = data.templates
  .map(
    (t) => `<div class="card">
  <h2 style="margin:0 0 6px"><a href="t/${esc(t.slug)}/">${esc(t.title)}</a></h2>
  <p style="margin:0 0 8px"><span class="tag">${esc(t.industry)}</span>${t.kind === 'pack' ? '<span class="tag">一鍵匯入</span>' : '<span class="tag">內建板模</span>'}</p>
  <p style="margin:0">${esc(t.description)}</p>
</div>`
  )
  .join('\n');
writeFileSync(
  join(out, 'index.html'),
  shell(data.site.title, data.site.tagline, `${data.site.base_url}/`, `<h1>${esc(data.site.title)}</h1>\n<p>${esc(data.site.tagline)}</p>\n${cards}`)
);

console.log(`generated ${data.templates.length + 1} pages under ${out}`);
