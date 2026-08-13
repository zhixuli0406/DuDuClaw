# DuDuClaw Claude Code plugin marketplace（種子，待遷出為公開 repo）

反向分發（WP2.6）：在別人的生態裡當入口。整包搬到 `github.com/zhixuli0406/duduclaw-claude-plugins`（👤）後，任何 Claude Code 使用者一行接入：

```
/plugin marketplace add zhixuli0406/duduclaw-claude-plugins
/plugin install duduclaw-connect@duduclaw
```

`duduclaw-connect` 內容：DuDuClaw MCP server 註冊（`npx -y duduclaw mcp-server`，200+ 工具）＋一個「何時用哪個工具」的 usage skill。沒有 hooks（信任負擔最低）。

維護：版本隨 npm 包演進不需每版更新（npx 抓 latest）；marketplace.json 的 plugin version 只在 skill/結構變動時 bump。
