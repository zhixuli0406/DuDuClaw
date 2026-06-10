# Press kit 素材清單

> 為 HN / Product Hunt / 部落格上線前準備。每一項都標明用途、規格、預估錄製時間，
> 以便能在一個週末錄完。

---

## A. GIF 動畫（必出）

| # | 主題 | 規格 | 用途 | 工具 | 預估時間 |
|---|---|---|---|---|---|
| **A1** | LINE → AI 回覆閉環（30 秒） | 1280×720, ≤ 5 MB, 30 fps | 全部地方都用（README、PH、Landing） | OBS + gifski | 1.5 hr |
| **A2** | Multi-Runtime 切換（30 秒） | 1280×720, ≤ 5 MB | PH gallery | OBS + gifski | 2 hr |
| **A3** | Evolution 演化前後對比（45 秒） | 1280×720, ≤ 8 MB | PH gallery + blog | OBS + gifski | 3 hr |
| **A4** | License activate + 模組解鎖（20 秒） | 1280×720, ≤ 4 MB | LicensePage / Settings 頁背景 | terminal + obs | 1 hr |

**錄製腳本**（A1 為例）：
```
0-3s   切到 LINE 對話視窗，輸入「今天營業到幾點」按送出
3-6s   切到 Mac Studio 終端，cargo watch 看到 prediction engine hit + cost = 0
6-9s   切回 LINE，AI 回覆「今晚營業至 21:30」+ 客戶滿意 ❤
9-15s  慢動作回放：左邊對話、右邊終端，並排
15-25s 文字疊圖：本次對話成本 = NT$0、回應時間 = 800ms
25-30s 收尾畫面：DuDuClaw 🐾 logo + "GitHub: zhixuli0406/DuDuClaw"
```

## B. 靜態截圖

| # | 主題 | 規格 | 用途 |
|---|---|---|---|
| **B1** | Dashboard 主頁總覽 | 1920×1080，Light + Dark 兩版 | README、PH、Landing hero |
| **B2** | LicensePage 帶 active license | 1920×1080 | PH gallery |
| **B3** | LicensePage OpenSource mode | 1920×1080 | 對比說明用 |
| **B4** | AgentsPage 多 agent 列表 | 1920×1080 | PH gallery |
| **B5** | Cost Telemetry — cache hit rate | 1920×1080 | HN 留言 + blog |
| **B6** | Evolution 演化歷史視圖 | 1920×1080 | PH gallery |
| **B7** | Security hooks 配置畫面 | 1920×1080 | 給合規敏感客戶看 |
| **B8** | Settings → 多 OAuth account rotation | 1920×1080 | 開發者社群會問 |

**錄製規範**：
- 使用 1920×1080 解析度 + 系統字體大小 110%（macOS 顯示器設定）
- Light mode 開 stone-50 背景；Dark mode 開 stone-900
- 截圖內所有客戶資料用假資料：customer_id=cus_demo、subscription_id=sub_demo、所有名字用「王小明 / 林大華」
- 截完用 ImageOptim 壓縮 → 目標 < 200 KB / 張

## C. 架構圖

| # | 主題 | 工具 | 用途 |
|---|---|---|---|
| **C1** | 系統架構總覽（12 crate + Python bridge） | Excalidraw → SVG | README + blog |
| **C2** | AgentRuntime trait dispatch flow | Excalidraw | HN 技術留言用 |
| **C3** | Evolution GVU loop 4 層驗證 | Excalidraw | PH + blog |
| **C4** | PayUni webhook crypto pipeline | Excalidraw | 內部文件 + blog |
| **C5** | Cloud multi-tenancy 拓樸（Mac Studio + 兩台 Hetzner） | Excalidraw | blog + 投資人遊說（保留） |

**規範**：
- 全部用 Excalidraw（手繪風格 = 不裝專業，反而更可信）
- 配色：amber-500 / orange-400 / stone-700（跟 brand 一致）
- 中英文版本各一份（zh-TW 內部文件用；EN PH/HN 用）

## D. 影片（建議但非必須）

| # | 主題 | 規格 | 用途 |
|---|---|---|---|
| **D1** | 5 分鐘產品介紹（英文） | 1080p mp4 | PH 首屏自動播 + YouTube |
| **D2** | 90 秒 trailer | 1080p mp4 | LinkedIn / X 廣告 |
| **D3** | 「為什麼 Apache 2.0 還要付錢」說書版（10 分鐘） | 1080p mp4 | Blog 配影片 + YT channel |

如果時間不夠，**只錄 D1 一個**。D1 的口播底稿請從 product-hunt-launch.md 的 description 段落改成口語化。

---

## E. 媒體 / Press Kit 包

| 內容 | 路徑 | 狀態 |
|---|---|---|
| 高解析 Logo（512 / 1024 / 2048 px PNG + SVG） | `marketing/press-kit/logos/` | TODO |
| 創辦人正臉照（高 + 中 + 低解析） | `marketing/press-kit/portraits/` | TODO 自拍 |
| 一句話簡介 zh-TW + EN | `marketing/press-kit/boilerplate.md` | TODO |
| 三段不同字數的產品描述（30 / 100 / 250 字） | `marketing/press-kit/boilerplate.md` | TODO |
| 媒體聯絡 email + LINE OA QR Code | `marketing/press-kit/contact.md` | TODO |

---

## F. 任務分配（自己一個人也要排）

| 週 | 任務 | 預估工時 |
|---|---|---|
| W1 上半 | 錄 A1 + B1 + B2（PH/HN 最低需求） | 6 hr |
| W1 下半 | 寫 D1 口播底稿 + 練稿 | 2 hr |
| W2 上半 | 錄 D1 + 剪輯 | 6 hr |
| W2 下半 | 補錄 A2-A4、B3-B8、C1-C3 | 8 hr |
| W3 上半 | 包高解析 logo + 拍正臉照 + 寫 boilerplate | 3 hr |
| W3 下半 | 全部素材 review + 上 CDN（Cloudflare R2） | 3 hr |
| W4 | 發布 PH + HN（沒新工作，只回留言 + 監控） | 0-3 hr |

**鐵律**：上線前 3 天內**不再錄新素材**。讓自己有時間 review。

---

## G. 已知陷阱

- **截圖內千萬不要有真實 customer_id / subscription_id / fingerprint**（會被別人拿去交叉比對你客戶）
- **不要用 macOS 系統截圖預設陰影**（太肥）— 用 `screencapture -o -t png` 去陰影
- **GIF 不要超過 5 MB**（PH 會壓爛、HN 不顯示）
- **影片不要超過 90 秒**（HN 留言區放影片連結時，>90秒 沒人點）
- **千萬不要在素材內包真實 API key、license key、PayUni MerKey**（你會痛恨自己）
