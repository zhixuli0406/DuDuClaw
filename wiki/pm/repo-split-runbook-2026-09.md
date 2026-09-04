# DuDuClaw ↔ DuDuClaw-OS 拆分 runbook（2026-09-04）

> 一次性遷移操作。破壞性 git 步驟（`git-filter-repo`）由**操作者親自執行**
> （AI 側被安全分類器擋，跑不了改寫歷史的命令）。
>
> 拍板邊界（2026-09-04）：**完整拆的 Yocto 層版**——
> - OS repo 拿 Yocto 層（`meta-duduclaw/` + `appliance/` + `scripts/release-os.sh`）。
> - Rust workspace **整份留主 repo**（`crates/` 所有成員，含 `duduclaw-shell/comp/os/sysd`——
>   桌面 App 與 OS 都靠它；`duduclaw-shell`←`native-gui`(桌面)、`duduclaw-pets`←`src-tauri`(桌面)，
>   硬抽會斷桌面 App 產品線）。
> - OS 照**現制**繼續 vendor 平台源快照（`meta-duduclaw/.../duduclaw-cli-src/`，
>   由 `refresh-src.sh` 從 `crates/` rsync）——拆完此腳本變**跨 repo 同步**（見 §4）。
> - 保留歷史（filter-repo 抽取）；主 repo 剝乾淨。

## 0. 前置

```bash
# git-filter-repo 已裝但沒進 PATH，用全路徑：
FR=~/Library/Python/3.9/bin/git-filter-repo
$FR --version    # 確認可跑

# 兩個 repo 都先 commit / 收乾淨工作區（filter-repo 要求乾淨樹或 fresh clone）
cd ~/Project/DuDuClaw && git status --short   # 應只剩 gitignored / 別線的檔
```

移動路徑集（`OS_PATHS`）：`meta-duduclaw/` `appliance/` `scripts/release-os.sh`
（若另有純 OS 腳本，加進下面每個 `--path`）。

## 1. 建立 DuDuClaw-OS repo（保留歷史 + 剝大 blob）

```bash
# 從 fresh clone 抽取，原 repo 不動
cd ~/Project
git clone DuDuClaw DuDuClaw-OS
cd DuDuClaw-OS

# 只保留 OS 路徑的歷史，順手剝掉 >100MB blob（flatpak tarball 兩版）
$FR \
  --path meta-duduclaw/ \
  --path appliance/ \
  --path scripts/release-os.sh \
  --strip-blobs-bigger-than 100M

# 驗證：只剩 OS 內容、無大 blob、歷史在
git log --oneline | wc -l                     # 應保有 OS 相關提交
du -sh .git                                    # 應遠小於原 repo（大 blob 已剝）
git ls-files | grep -vE '^(meta-duduclaw|appliance|scripts/release-os)' | head  # 應為空

# 建 GitHub repo 後推送（用 gh 或網頁先建空 repo）
gh repo create zhixuli0406/DuDuClaw-OS --private --source=. --remote=origin --push
# 或手動：
# git remote add origin https://github.com/zhixuli0406/DuDuClaw-OS.git
# git push -u origin main
```

## 2. 主 DuDuClaw repo 剝除 OS 路徑 + 大 blob

```bash
cd ~/Project/DuDuClaw

# 反向剝除：拿掉 OS 路徑（連同其下的大 blob 一起消失於全歷史）
$FR \
  --path meta-duduclaw/ \
  --path appliance/ \
  --path scripts/release-os.sh \
  --invert-paths \
  --strip-blobs-bigger-than 100M

# 驗證：OS 路徑全消、歷史無大 blob
git ls-files | grep -E '^(meta-duduclaw|appliance)' | head   # 應為空
git rev-list --objects --all | git cat-file --batch-check='%(objecttype) %(objectsize) %(rest)' \
  | awk '$1=="blob" && $2>104857600{print $2/1048576" MB "$3}'   # 應為空

# filter-repo 會移除 origin，補回後推送（歷史改寫→force）
git remote add origin https://github.com/zhixuli0406/DuDuClaw.git
git push -u origin main --force-with-lease
```

> ⚠️ `--force-with-lease`：歷史已改寫，與遠端分歧是預期的（你自己的 repo、你自己的改寫）。
> 若有其他 collaborator/CI 依賴舊 SHA，先通知——所有 commit SHA 都會變。

## 3. 磁碟殘留（無害，可留可清）

- filter-repo 只動 git 物件，**不刪工作區未追蹤檔**。主 repo 磁碟上的
  `meta-duduclaw/`（含 931M 未追蹤 tarball）在 §2 後變成純未追蹤目錄——
  若不再於主 repo 建 OS，可 `rm -rf meta-duduclaw appliance`（它們已在 DuDuClaw-OS）。
- DuDuClaw-OS repo 缺那顆 931M flatpak tarball（gitignored 未進歷史）——
  首次 OS bake 前跑 `meta-duduclaw/recipes-duduclaw/duduclaw-flatpak-offline-repo/gen-flatpak-offline-repo.sh` 重生。

## 4. 拆後跨 repo 耦合修（DuDuClaw-OS 內）

OS 的 `refresh-src.sh`（`meta-duduclaw/recipes-duduclaw/duduclaw-cli/`）原本
`rsync ../../../crates/ → duduclaw-cli-src/`——拆後 `crates/` 在另一 repo，相對路徑斷。
三選一（建議 A）：

- **A. 環境變數指向主 repo checkout**：改 `refresh-src.sh` 讀
  `DUDUCLAW_CLI_SRC_ROOT`（預設 `../DuDuClaw`），rsync 該處 `crates/`。
  OS 開發者需在旁 checkout 主 repo。最貼近現制、零額外基建。
- **B. 主 repo 發布源 tarball / release tag**：OS 消費某個 CLI release 的
  源封存（穩定、可版本釘），但主 repo 要多一條「發布源封存」產線。
- **C. submodule**：DuDuClaw-OS 加 `crates/` 為 submodule 指向主 repo——
  但 vendored 快照本就是「窄化剪枝過的 workspace」（Cargo.lock 剪枝，見既有註解），
  submodule 給的是完整 workspace，與現制不符，不建議。

## 5. 拆後主 repo 清理（非阻塞，複查即可）

主 repo 仍有多處 `meta-duduclaw/`/`appliance/` 的**註解/字串**引用（`git grep -l`
在 crates/docs/CHANGELOG 等），多為說明 OS 關係、不影響 build。剝除後：
- `.gitignore` 裡 `/meta-duduclaw/.build/`、flatpak tarball、`/sb-keys/` 等 OS 條目可留可清。
- ~~`scripts/release.sh` 若引用 `release-os.sh` 或 yocto 產物路徑，更新為「見 DuDuClaw-OS repo」。~~
  **✅ 已完成（見 §7）**——`release.sh` 的 `yocto_inc`/`yocto_bb` 版號同步 kind 全數移除，
  平台版號流不再碰任何 OS metadata。
- `CHANGELOG.md` / `docs/` 的 OS 段落：doc rot 防治原則下逐一複查，非阻塞。

## 6. 收尾

- 兩 repo 各自 README 首句更新：主 repo=「Multi-Runtime AI Agent 平台」，
  DuDuClaw-OS=「以 DuDuClaw 為原生 agent 的 Yocto OS（值班機 image）」。
- `commercial/`（L3，nested git）不受影響——設計文件仍在主 repo 旁的 gitignored tree，
  若 OS 設計文件要跟去 OS repo，另議（commercial 是獨立私有 repo）。
- 驗證兩邊都能建：主 repo `cargo build`（workspace 完整）；DuDuClaw-OS
  `kas build`（vendored 快照 + §4 修好的 refresh 路徑）。

## 7. 發布產物路由（2026-09-04，拍板落地）

拍板：**平台產物（gateway/desktop）留 DuDuClaw repo；OS 產物（.wic/.iso）進
DuDuClaw-OS repo**；OS 走 **GitHub Releases 公開上傳**、採 **獨立版號**（與平台版號脫鉤）。

### DuDuClaw-OS 側（`scripts/release-os.sh` + 新增 `VERSION`）
- **OS 獨立版號 SoT** = repo 根 `VERSION` 檔（首個非註解行的 bare semver，起始 `0.1.0`——
  0.x 標 bring-up，內部 DISTRO_VERSION 仍帶 `-y1-bringup`，跨 1.0.0 = 宣告 GA）。
  取代原本讀 `Cargo.toml`（拆分後 OS repo 根已無 top-level Cargo.toml，那條讀取是壞的）。
  所有子命令 `v<version>` 參數改**可選**，預設讀 `VERSION`。
- **`publish` 新子命令**：把 `package` 產的四檔（`.wic.zst` + `.sha256` + `.minisig` +
  `manifest.json`）上傳到 `zhixuli0406/DuDuClaw-OS` 的 GitHub Release（tag `v<version>`）。
  上傳前 fail-closed：重驗 minisig 對釘死的 `OS_RELEASE_PUBKEY` + `shasum -c` sidecar；
  idempotent（`--clobber`）；偵測 repo 私有時 warn（不擋）提醒「私有 repo 的 release 資產
  不對外可下載，需 `gh repo edit --visibility public`」。`DUDUCLAW_OS_GH_REPO` 可覆寫目標。
- **內部 Yocto DISTRO_VERSION 刻意不動**（仍 `${DUDUCLAW_PLATFORM_VERSION}-y1-bringup`）：
  它餵 A/B GPT 分割名/UKI 檔名/更新台帳 ProtectVersion，改它要烤像重驗 A/B 鏈——
  故本輪只有 **release 產物名 + GH tag** 帶獨立 OS 版號，DISTRO_VERSION 對齊列為需烤像的後續。
  `manifest.json` schema bump 到 2：`version`=OS 版、新增 `platform_version`=內嵌平台版、
  `distro_version_full`=內部建構版，三者分開記全 provenance。
- 完整流程：`build → smoke → package → publish`（皆手動、皆非自動觸發）。

### DuDuClaw 側（`scripts/release.sh`）
- 移除 `yocto_inc`/`yocto_bb` 兩個版號同步 kind（platform_manifests / extract_version /
  bump / assert 全清）——平台版號流**不再碰任何 OS metadata**。revert 邏輯保留
  `git reset --hard HEAD`（原以 yocto_bb rename 為由，改為通用「一步清 staged+unstaged」註解）。
  收尾 next-steps 第 5 點改指向 `cd ../DuDuClaw-OS && release-os.sh ...`。

### 待使用者處理
- OS repo 目前為 `--private`（§1 建立時）。若要 release 真正公開可下載，需
  `gh repo edit zhixuli0406/DuDuClaw-OS --visibility public`（visibility flip = 使用者關卡）。
- OS 簽章金鑰 `~/.minisign/duduclaw-os-release.key` 首次 `publish` 前需在該機備妥。
