# Linux 保護模式：驗收手冊（v1.81）

[English](../linux-protected-acceptance.md) · [日本語](../ja-JP/linux-protected-acceptance.md)

這份手冊給發行維護者使用，說明 v1.81 原生 Linux 保護模式驗收要準備什麼、要執行哪些指令，以及怎麼看結果。範圍包含個人與共用 AI 員工、外部連線，以及 Package H 工具。

> **以下每個步驟都會改動主機。** 驗收工具會安裝保護模式服務、建立系統帳號、寫入 `/var/lib/duduclaw*`、`/opt`、`/etc`、修改 `/etc/hosts`、安裝一次性憑證機構，結束後再全部移除。請只在可丟棄的虛擬機上執行。沒有設定 `DUDUCLAW_ISO_DISPOSABLE=1` 時工具會拒絕執行；主機上已有不是它建立的 DuDuClaw 安裝時也會拒絕。工具不會用到你自己的 `~/.duduclaw`，也不會用到你自己的 Claude 或 agy 登入：操作端 CLI 的 `HOME` 與 `DUDUCLAW_HOME` 都指向私有的暫存目錄。

## 涵蓋範圍

| 部分 | 工具 | 內容 |
|---|---|---|
| G1 | `cargo test -p duduclaw-cli`（單元測試，任何 Linux 建置機，不需 root） | 用真正的授權簽發流程與真正的連線工廠，逐一檢查全部 263 個工具，對一位個人員工與一位共用員工：路由、需要的授權、兩種員工各自能否使用、沒有缺少的工廠 |
| G3 | `scripts/live-test/linux-protected/connectors-acceptance.sh` | 驗收表 1–15：個人綁定、撤銷、重新啟動、需要設定、各連線的實測 |
| G2 | `iso_personal_chain` 測試程式（root） | 個人員工：申請 → 核准 → 配置 → 用擁有者的連線執行 → 停用封存（會停掉執行中的工作）→ 還原（同一個 uid）→ spawnd 狀態重設後改以新身分接手（新 uid，舊權杖被拒）→ 清除與釋放（名稱可再用，新 id 與 uid）。共用員工：停用封存 → 還原 |
| H15 | `scripts/live-test/linux-protected/package-h-acceptance.sh` + `iso_package_h_chain` 測試程式（root） | 驗收表 16–27：主機資訊、經由代理讀檔的檔案工具、公開網站工具與拒絕情況、在同一次執行內跑的輔助程式（逾時至少 305 秒、取消）、測試用技能來源與惡意壓縮檔、D8 下的委派工具、錄影開關、就緒狀態原因 |

## 事前準備

**主機。** 每個發行版各一台可丟棄的虛擬機：Ubuntu 22.04、Ubuntu 24.04、Debian 12。需要 root、systemd 為 PID 1、cgroup v2，而且不能是容器。套件：`python3`（3.10 以上）、`util-linux`（`setpriv`）、`passwd`、`openssl`、`ca-certificates`、`coreutils`、`procps`。`/var` 與 `/opt` 約需 2 GB 空間。

**建置。** 一個你自己驗證過的候選版 `duduclaw`，以及同一個 commit 建出的兩個測試程式：

```sh
cargo test -p duduclaw-gateway --test iso_package_h_chain --no-run   # 會印出 .../deps/iso_package_h_chain-<hash>
cargo test -p duduclaw-gateway --test iso_personal_chain --no-run    # 會印出 .../deps/iso_personal_chain-<hash>
mkdir -p /abs/tests && cp <印出的路徑> /abs/tests/   # 再改名為 iso_package_h_chain / iso_personal_chain
```

**工具驅動程式。** 儀表板 RPC 不會替 AI 員工呼叫工具，所以每個實際的工具呼叫都要經過你提供的程式（`DUDUCLAW_ISO_TOOL_DRIVER`）。介面寫在 `scripts/live-test/linux-protected/README.md` 的「Tool driver contract」；H15 另外用到 `seed` 與 `cancel`。沒有這個程式時，相關檢查會是 `SKIP`。

**已簽署的輔助程式**（H21/H22）：發行內容必須包含程式執行、程式碼索引與 Office 的輔助程式；否則 spawnd 不會宣告 `helper_calls`，這些檢查依設計會被拒絕。

**測試網站與技能來源**（H19/H20、H23/H24）：主機需要一個**公開**的 IP 位址（`--fixture-ip`）。保護模式的網路存取只能連到公開位址，內網位址（例如 192.168.x.x）依設計會被拒絕，這些檢查就會 `SKIP`。驗收工具會在該位址提供 `fixtures/public_origin.py`，搭配一次性憑證機構，並在 `/etc/hosts` 加入 `h15.fixture-duduclaw.net`、`rebind.fixture-duduclaw.net`（指向 127.0.0.1）與 `clawhub.ai`。前提是產品的 DNS 與 TLS 會採用 `/etc/hosts` 和系統的信任憑證；若不會，H19/H23 會出現 DNS 或 TLS 錯誤，這時需要真正的公開測試主機（`DUDUCLAW_ISO_PUBLIC_ORIGIN`）。

**第二位管理員**（G2 的還原、清除、釋放）：還原與提前清除的核准規則，需要一位已經連續符合資格至少 7 天的管理員，或管理員紀錄已滿 7 天的主機。全新主機上這些步驟會是 `SKIP`，並寫明原因。要涵蓋它們，請保留一台長期使用的可丟棄主機，並把第二位管理員的登入狀態放在 `DUDUCLAW_ISO_STATE_DIR/admin2.json`。

**擁有者必須提供的外部服務測試帳號**（G3 的第 1、3、7–9、14、15 項；G1、G2、H15 都不需要）：

| 服務 | 內容 | 變數 |
|---|---|---|
| Google | Workspace 測試租戶：兩位使用者的個人 OAuth（個人員工）、主機 OAuth、委派或服務帳戶 | `DUDUCLAW_ISO_GOOGLE_ALICE=1`、`DUDUCLAW_ISO_GOOGLE_HOME=1` |
| Notion | 工作區憑證 | `DUDUCLAW_ISO_NOTION=1` |
| GitHub | 測試組織或儲存庫憑證 | `DUDUCLAW_ISO_GITHUB=1` |
| LINE | Bot 頻道權杖（Discord、Telegram 同理） | `DUDUCLAW_ISO_LINE_TOKEN`（`_DISCORD_TOKEN`、`_TELEGRAM_TOKEN`） |
| Odoo | 可丟棄的執行個體與 API 金鑰 | `DUDUCLAW_ISO_ODOO_URL`（另加 `_DB`、`_USER`、`_KEY`） |
| 資料庫來源 | Postgres 或 MySQL 唯讀帳號與一個資料表 | `DUDUCLAW_ISO_PG_DSN_RO` + `_PG_TABLE`、`DUDUCLAW_ISO_MYSQL_DSN_RO` + `_MYSQL_TABLE` |
| 搜尋 | 搜尋服務金鑰 | `DUDUCLAW_ISO_SEARCH_API_KEY` |

OAuth 帳號請用 `connectors-acceptance.sh --pause-after-setup` 連線。不要把真正的權杖留在共用的 shell 歷史紀錄裡，請用 `sudo env 變數=… 腳本` 傳入。

## 指令

1. **G1**（Linux 建置機，不需 root）：

   ```sh
   cargo test -p duduclaw-cli g1_real_issuer_and_factory -- --nocapture
   ```

2. **G3**（每個發行版）：

   ```sh
   sudo env DUDUCLAW_ISO_DISPOSABLE=1 DUDUCLAW_ISO_TOOL_DRIVER=/abs/driver \
     bash scripts/live-test/linux-protected/connectors-acceptance.sh \
     --candidate /abs/candidate-dir --out /abs/evidence/g3-ubuntu2204
   ```

3. **H15 + G2**（每個發行版；一個腳本會跑兩組測試）：

   ```sh
   sudo env DUDUCLAW_ISO_DISPOSABLE=1 DUDUCLAW_ISO_TOOL_DRIVER=/abs/driver \
     bash scripts/live-test/linux-protected/package-h-acceptance.sh \
     --candidate /abs/candidate-dir --tests-dir /abs/tests --out /abs/evidence/h15-ubuntu2204 \
     [--fixture-ip <本機的公開位址>] [--allow-spawnd-reset]
   ```

   `--allow-spawnd-reset` 允許 G2 把 `/var/lib/duduclaw-spawnd` 移到旁邊，用來驗證改以新身分接手；這會讓主機上所有員工失去主機端配置，只能在不在乎其他資料的主機上使用。`--only h15` 或 `--only g2` 只跑其中一組。`--keep` 保留安裝以便調查；`--reset` 移除上一次留下的安裝。

4. **spawnd 與輔助程式的 root 測試**（不變，每個發行版）：`DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd -- --ignored iso_root_ --test-threads=1`，以及 `crates/duduclaw-execution/tests/recording_root.rs` 的錄影 root 測試（`ISO_ROOT_CHROMIUM`）。

## 怎麼看結果

- 每項檢查一行：`PASS`、`FAIL` 或 `SKIP`，加上編號與說明。測試程式回報的編號前面會加 `H15:` 或 `G2:`（例如 `G2:G2-05a`）。腳本最後會印出表格與數量。
- 結束代碼 `1` 表示至少有一個 `FAIL`，或執行被中斷。結束代碼 `0` 仍可能有 `SKIP`：**請逐一看過**。`SKIP` 一定會寫出缺了什麼（驅動程式、測試網站位址、第二位管理員、重設 spawnd 的許可），永遠不算通過。
- `FAIL` 寫著「… is not implemented in this build (F6c / F6g / F6l / F6 §3.8 / F6 §3.9)」，代表候選版還沒有還原、接手、清除或釋放的 RPC。這是功能缺口，不是主機問題。
- `OUT/facts.tsv` 記錄每次執行的主機資訊：發行版、核心、systemd、Landlock ABI、LSM、spawnd 功能位元（`work_broker`、`helper_calls`、`helper_recording`），以及是否跑過錄影檢查。三個發行版的結果都要保留這份檔案。Ubuntu 22.04 與 Debian 12 的 Landlock ABI 低於 4（無法限制 TCP 連接埠，屬已記錄的殘餘風險）。
- `OUT/<測試程式>.log` 是完整輸出；`OUT/evidence/` 有 RPC 回覆（不含權杖）、驅動程式回覆與測試網站的請求紀錄；`journal.log` 是所有 `duduclaw*` 服務的日誌。
- 最後一行 `PASS cleanup` 表示安裝、服務、帳號、測試憑證機構與 hosts 項目都已移除。`FAIL cleanup` 會保留暫存目錄，之後可用 `--reset` 清理。

**發行判斷。** G1 在 Linux 建置機上通過，而且三個發行版上 G3 的 † 項目以及 H15、G2 的每項檢查都是通過，或是已記錄並被接受的 `SKIP`，才可以發行。全新主機上預期會出現的 `SKIP`：G2 的還原、清除、釋放（核准規則），以及沒有發布錄影輔助程式時的 H26。在錄影檢查於每個發行版都通過之前，錄影開關必須保持關閉（`H16-recording` 為 PASS）。
