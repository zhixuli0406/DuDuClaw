# GitHub integration (issue/PR search + read + comment)

Connect a GitHub account so your AI employees can search issues and pull
requests, read them in full, and post comments. DuDuClaw talks to the GitHub
REST API natively — there is no third-party MCP server to install. The access
token is stored in DuDuClaw's encrypted OAuth vault.

## What you get

Five agent-facing MCP tools, gated by two scopes (`github:read` /
`github:write`):

| Tool | Class | What it does |
|------|-------|--------------|
| `github_status` | read | Connection diagnostics: connected?, granted scopes. Reads local state only. |
| `github_search_issues` | read | Search issues and PRs with GitHub search syntax (`repo:owner/name is:open label:bug`). Returns repo/number/title/state/is_pr/updated/url. |
| `github_issue_read` | read | Read one issue: title, state, author, body (truncated if long), and the most recent 10 comments. |
| `github_pr_read` | read | Read one PR: metadata (base/head/state/merged/mergeable) plus the changed-file list (filename/status/additions/deletions, up to 50 files). Diff contents are not fetched. |
| `github_issue_comment` | write | Post a comment on an issue or PR. **Publicly visible.** |

### Safety design

- **Comments are public.** `github_issue_comment` posts a publicly visible
  statement — treat it as an outbound communication. **Gate it behind approval:**

  ```toml
  [capabilities]
  approval_required_tools = ["github_issue_comment"]
  ```

- **Read stays read.** The read-class tools cannot modify anything on GitHub.
- **No diff bodies.** `github_pr_read` lists changed files with add/delete counts
  but never pulls the diff content, keeping responses bounded.
- **Least privilege.** Only the `repo` scope is requested (needed to read/comment
  on private repositories). Public-only usage still works — the scope simply
  also covers private repos when granted.

## Prerequisites: create a GitHub OAuth App

You supply your own GitHub OAuth App (DuDuClaw never ships shared credentials).
One-time setup:

1. Open [GitHub → Settings → Developer settings](https://github.com/settings/developers).
2. Under **OAuth Apps**, click **New OAuth App**.
3. Set the **Authorization callback URL** to exactly:

   ```
   http://localhost:18789/api/mcp/oauth/callback
   ```

4. Register the app, then copy the **Client ID** and click **Generate a new
   client secret** to get the **Client secret**.

The requested scope is:

```
repo
```

`repo` grants read + comment access to issues and pull requests on both public
and private repositories the account can see. If you only need public repos you
can still connect with `repo`; it is the minimal scope that also unlocks private
repositories.

## Connect from the dashboard

1. Go to **Integrations → GitHub** (`/manage/integrations?tab=github`).
2. Paste the Client ID and Client secret, then click **Connect GitHub**.
3. A GitHub consent window opens. Approve access. The window confirms success and
   the dashboard flips to **GitHub is connected**.

The client credentials are persisted (secret encrypted at rest) so
re-authorizing later does not require re-entering the secret.

## About the token

A classic GitHub OAuth App token has **no expiry** (`expires_at` is empty — the
healthy default). If your OAuth App opts into **token expiration**, GitHub issues
a `refresh_token`; DuDuClaw then refreshes the token in place using your stored
client credentials when it expires. Both shapes are handled automatically.

## Token exchange details (for the curious)

GitHub's token endpoint replies **form-encoded by default**; DuDuClaw sends
`Accept: application/json` so it returns JSON instead. This is handled in the
OAuth layer — no configuration needed.

## Troubleshooting

- **"GitHub is not connected."** — No token stored. Connect from the dashboard.
- **`401 Unauthorized`** — The authorization was revoked or is invalid.
  Reconnect.
- **`403`** — Usually a missing `repo` scope for a private repository, or a rate
  limit. `github_status` shows a note when `repo` is not granted; reconnect to
  grant it.
- **`404` "not found"** — Check the owner/repo/number, or grant `repo` for a
  private repository.
- **Callback URL mismatch during consent** — The OAuth App's Authorization
  callback URL must be exactly `http://localhost:18789/api/mcp/oauth/callback`.

Run `github_status` at any time for a live diagnosis.
