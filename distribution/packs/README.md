# Free starter packs（premium 降級包）

畫廊初始供給：從 22 個 premium 產業板模挑出六個高搜尋量產業，降級出免費入門版
（2026-08-13 拍板）。每包＝**單一 AI 員工**的 expert pack（data lane，免簽章）。

## 降級刀法（固定規則）

| 保留 | 移除（付費版價值） |
|---|---|
| persona／核心工作流程／Response Style | wiki 產業法規知識包（compliance/processes/glossary） |
| **全部安全邊界與 Escalation Rules**（免費核心不閹割） | FAQ 題庫 |
| 行業用語表 | 多員工團隊劇本（teams） |
| | 加購話術／情緒降溫腳本（ecommerce 的 PREMIUM 標記段） |

來源：`commercial/templates-premium/<ind>-pro/SOUL.md`（header 換 Starter 註記＋升級指引）。
新增產業照同一規則產出即可。

## 目前六包

ecommerce-starter｜realestate-starter｜education-starter｜fitness-starter｜vet-starter｜clinic-starter

驗證紀錄（2026-08-13）：六包 `duduclaw expert install <dir> --dry-run` 全過；
vet-starter 真安裝活測過（SOUL/agent.toml/partial 合併正確）。

## 建置與發佈

```bash
# 1. 打 zip（dist/ 為 gitignored 工件，隨時可重建）
cd distribution/packs
for d in *-starter; do (cd $d && zip -qrX ../dist/pack-$d-1.0.0.zip .); done

# 2. 上架 registry（👤：duduclaw-registry repo 上線後）
#    每包跑 expert publish 產 entry（自動算 sha256＋判 data lane）：
duduclaw expert publish distribution/packs/<slug> --zip distribution/packs/dist/pack-<slug>-1.0.0.zip
#    zip 先上傳到 GitHub release 資產（或其他 https 主機），entry 的 url 指向該處，
#    然後照 distribution/registry/README.md 的三步 PR 流程送件。

# 3. 畫廊：data/templates.json 已含六包（install.registry_slug），
#    registry entry 合併後 gallery generate.mjs 也會自動長出 registry 版頁面（去重以 slug 為準）。
```

注意：畫廊頁渲染的安裝指令是 `duduclaw expert install registry:<slug>`——在 registry
repo 上線並合併這六包 entry 之前，該指令會誠實回 404。部署畫廊（👤）前先完成 registry 送件。
