# 官網 demo sandbox（WP1.3）——「先玩再裝」漏斗

零安裝試玩：官網掛聊天泡泡 → 訪客直接跟 demo 員工「嘟嘟」對話（WebChat 訪客模式，WP1.8 建的 gateway 能力）。本目錄是完整部署產物；**實際部署與網域是 👤 關卡**。

## 組成

- `home-seed/` — demo 站的 `DUDUCLAW_HOME` 種子：
  - `config.toml`：`public_widget` 開、`allowed_origins` 綁官網網域、dispatch 關
  - `agents/demo/`：導覽員 SOUL（展示邊界明確）＋ agent.toml（工具全關、**日預算 500 美分硬停**——訪客會看到白話停工說明，成本不會失控）
- `embed-snippet.html` — 官網嵌入片段（複用 WordPress 外掛的 widget.js/css，放官網 static 即可）

## 部署（👤，SA 已具權限，照 gcp-deploy 慣例）

```bash
# 1. 生 widget_key 並改 home-seed/config.toml（widget_key + allowed_origins）
openssl rand -hex 16

# 2. Cloud Run 部署（ghcr image + seed 掛載法：把 home-seed 烤進小 wrapper image）
cat > Dockerfile <<'EOF'
FROM ghcr.io/zhixuli0406/duduclaw:latest
COPY --chown=duduclaw:duduclaw home-seed/ /home/duduclaw/.duduclaw/
EOF
gcloud run deploy duduclaw-demo \
  --source . --region asia-east1 --project louis-460302 \
  --port 18789 --memory 1Gi --max-instances 1 --min-instances 0 \
  --set-env-vars DUDUCLAW_BIND=0.0.0.0 \
  --set-secrets ANTHROPIC_API_KEY=duduclaw-demo-anthropic:latest \
  --allow-unauthenticated

# 3. 官網放 widget 檔（clients/wordpress/duduclaw-webchat/widget.{js,css} 複製到官網 /assets/）
#    再貼 embed-snippet.html（換 GATEWAY/KEY），allowed_origins 需含官網網域。
```

## 濫用護欄（已內建，部署前逐項確認）

| 護欄 | 機制 |
|---|---|
| 成本斷路器 | agent.toml `[budget] daily_cap_cents=500 + hard_stop`（花費達上限 → 白話停工訊息＋通知） |
| 連線上限 | WebChat per-IP 上限（MAX_CONNECTIONS_PER_USER）＋全域 semaphore（10） |
| 工具面 | demo 員工 `allowed_tools=[]` deny-by-default、dispatch 關、無跨員工權限 |
| 來源 | `allowed_origins` 只列官網網域；widget key 只授權匿名對話 |
| 內容 | SOUL 邊界（不離題、不執行任務）＋既有 injection scanner |

## 驗收（部署後）

- [ ] 官網頁面開聊天泡泡 → 送「你是誰」→ 收到導覽員回覆（含免費層尾註）
- [ ] 第二個瀏覽器 session 開新對話 → 看不到前一位訪客內容（訪客身份唯一性）
- [ ] 灌爆測試：同 IP 開 4 個連線 → 第 4 個被拒
- [ ] `daily_cap_cents` 調成 1 重啟 → 對話收到白話停工訊息 → 調回 500
