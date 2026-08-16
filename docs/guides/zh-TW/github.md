# GitHub 整合（issue／PR 搜尋＋讀取＋留言）

連接一個 GitHub 帳號，讓你的 AI 員工能搜尋 issue 與 pull request、完整讀取
內容，並張貼留言。DuDuClaw 原生對接 GitHub REST API，不需要安裝任何第三方
MCP server。存取權杖會存放在 DuDuClaw 加密的 OAuth vault 中。

## 你會拿到什麼

五個面向 agent 的 MCP 工具，由兩個 scope 把關（`github:read` ／
`github:write`）：

| 工具 | 類別 | 功能 |
|------|------|------|
| `github_status` | read | 連線診斷：是否已連接？已授權哪些 scope？只讀取本機狀態。 |
| `github_search_issues` | read | 用 GitHub 搜尋語法搜尋 issue 與 PR（`repo:owner/name is:open label:bug`）。回傳 repo／number／title／state／is_pr／updated／url。 |
| `github_issue_read` | read | 讀取單一 issue：標題、狀態、作者、內文（過長會截斷），以及最近 10 則留言。 |
| `github_pr_read` | read | 讀取單一 PR：metadata（base／head／state／merged／mergeable）加上異動檔案清單（filename／status／additions／deletions，最多 50 個檔案）。不會抓取 diff 內容。 |
| `github_issue_comment` | write | 在 issue 或 PR 上張貼留言。**公開可見。** |

### 安全設計

- **留言是公開的。** `github_issue_comment` 會發出一則公開可見的聲明，請把
  它當成對外溝通看待。**建議把它掛上審批關卡：**

  ```toml
  [capabilities]
  approval_required_tools = ["github_issue_comment"]
  ```

- **read 就是唯讀。** read 類的工具無法對 GitHub 做任何修改。
- **不含 diff 內容。** `github_pr_read` 只列出異動檔案與增刪行數，絕不拉取
  diff 內容，讓回應大小維持在可控範圍。
- **最小權限。** 只會請求 `repo` scope（讀取／留言私有 repository 需要這個
  scope）。只用在公開 repo 也照樣能運作，這個 scope 只是在被授權時順便涵
  蓋私有 repo。

## 前置需求：建立 GitHub OAuth App

你需要自備一個 GitHub OAuth App（DuDuClaw 不會內建共用憑證）。一次性設定
步驟：

1. 開啟 [GitHub → Settings → Developer settings](https://github.com/settings/developers)。
2. 在 **OAuth Apps** 底下，點 **New OAuth App**。
3. 把 **Authorization callback URL** 精確設為：

   ```
   http://localhost:18789/api/mcp/oauth/callback
   ```

4. 註冊完成後複製 **Client ID**，再點 **Generate a new client secret** 取得
   **Client secret**。

請求的 scope 是：

```
repo
```

`repo` 會授予讀取與留言的權限，涵蓋這個帳號看得到的公開與私有 repository
上的 issue 與 pull request。如果你只需要公開 repo，一樣可以用 `repo` 連
線，它是同時能解鎖私有 repo 的最小 scope。

## 從儀表板連線

1. 前往**管理 → 整合／工具連線 → 工具伺服器**（`/manage/integrations`）。
2. 捲到**需要授權的服務**區塊，找到 **GitHub** 卡片。
3. 點卡片上的 **Configure**。貼上 Client ID 與 Client secret，對話框也會
   顯示要登記的精確 callback URL，必須跟你在上面步驟 3 填的完全一致。
4. 會跳出 GitHub 的同意授權視窗，核准後卡片就會變成 **Authenticated**。

Client 憑證會被保存下來（secret 靜態加密儲存），之後要重新授權不需要再
輸入一次 secret。

## 關於 Token

傳統 GitHub OAuth App 的 token **不會過期**（`expires_at` 是空值，這是正
常且健康的預設狀態）。如果你的 OAuth App 有開啟 **token expiration**，
GitHub 就會核發 `refresh_token`；DuDuClaw 會在 token 過期時，用你已存好
的 client 憑證原地刷新。兩種形態都會自動處理。

## Token 交換細節（給好奇的人看）

GitHub 的 token endpoint 預設會回傳 **form-encoded** 格式；DuDuClaw 會帶上
`Accept: application/json`，讓它改回傳 JSON。這在 OAuth 層已經處理好，不
需要你另外設定。

## 疑難排解

- **「GitHub is not connected.」** 代表沒有存到 token，從儀表板連線即可。
- **`401 Unauthorized`** 代表授權已被撤銷或已失效，重新連線即可。
- **`403`** 通常是私有 repository 缺少 `repo` scope，或碰到 rate limit。
  沒有授權 `repo` 時 `github_status` 會顯示提示，重新連線授權即可。
- **`404` 「not found」** 請檢查 owner／repo／number 是否正確，或為私有
  repository 授權 `repo`。
- **同意授權時 callback URL 不符** OAuth App 的 Authorization callback URL
  必須精確等於 `http://localhost:18789/api/mcp/oauth/callback`。

隨時都可以執行 `github_status` 取得即時診斷。
