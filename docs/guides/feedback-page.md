# Feedback and suggestions page (GitHub Pages + Haiku auto-triage)

Lets end users report issues without knowing GitHub: they fill out a
Chinese-language form, and the content is automatically organized into a
GitHub issue — Haiku handles classification, labeling, and formatting. No
self-hosted server involved anywhere.

- **Form URL**: <https://zhixuli0406.github.io/DuDuClaw/>
- **Where reports go**: this repo's [Issues](https://github.com/zhixuli0406/DuDuClaw/issues) (`feedback` label)

## How it works

```
User fills out the form (static GitHub Pages page, zero secrets)
   │  Assembles Markdown, redirects to the pre-filled GitHub issue page
   ▼
User clicks submit on GitHub (can drag in screenshots/videos, native GitHub upload)
   │  Issue body carries the <!-- duduclaw-feedback-form v1 --> marker
   ▼
GitHub Actions (feedback-triage.yml, only processes marked issues)
   │  claude-haiku-4-5 + structured outputs: classification/severity/title/formatting
   ▼
Issue is automatically rewritten: organized content + original text tucked into <details> + labels (feedback + category)
```

## Related files

| File | Role |
| --- | --- |
| `feedback/index.html` | The form page itself (self-contained HTML, no external dependencies; styling is a hand-rolled version of the MDS design system) |
| `feedback/inter-latin-wght-normal.woff2` | Inter Variable font (Latin subset, bundled locally rather than loaded from a CDN) |
| `.github/workflows/deploy-feedback-page.yml` | Deploys to GitHub Pages whenever `feedback/**` changes |
| `.github/workflows/feedback-triage.yml` | Triggers Haiku triage when an issue is opened |

## One-time setup

1. GitHub Pages is set to workflow mode (`gh api -X POST repos/<owner>/DuDuClaw/pages -f build_type=workflow`).
2. Repo secret `ANTHROPIC_API_KEY`: `gh secret set ANTHROPIC_API_KEY`.
   If it's not set, triage is simply skipped (the issue is left as-is) — the form flow itself is unaffected.
3. The `feedback` label (already created; if it gets deleted, triage will fail to apply the label).

## Security design

- **Zero secrets on the frontend**: API keys and tokens live only in Actions secrets; the static page never has access to them.
- **Prompt injection protection**: issue content is wrapped in XML tags and explicitly demoted to data;
  model output is constrained by a JSON schema (classification can only land on one of four enum values); the original text is
  always preserved in `<details>`, and the issue is left untouched if triage fails.
- **Script injection protection**: the workflow never interpolates issue body text into a shell command —
  it fetches via `gh api` into a file and assembles JSON with `jq`.
- **Cost**: only issues carrying the form marker trigger a run; input is truncated to 16k characters; a single
  Haiku call costs roughly $0.01 or less.

## Modifying the form

Push a change to `feedback/index.html` on main and it redeploys automatically. When you change the fields, remember
to keep the section names in `feedback-triage.yml`'s system prompt in sync (Problem description (問題描述) / Steps
to reproduce (重現步驟) / Expected behavior (預期行為) / Environment (環境)).

The styling follows the MDS design system (shared with the dashboard's `web/src/components/mds/`): OKLCH
color tokens, layered surfaces, a radius system (10px for buttons/inputs, 14px for cards), Inter plus a
Traditional Chinese system-font fallback, font weights limited to 400/500, a brand-blue CTA, a 3px focus
ring. Because this is a build-free static page, the tokens are hand-written as CSS custom properties
directly in the `<style>` block, and need to be synced manually whenever the MDS tokens change. Dark mode
uses `prefers-color-scheme` (the dashboard's `.dark` class mechanism doesn't apply on a static page).
