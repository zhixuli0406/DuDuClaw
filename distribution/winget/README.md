# winget 上架（Windows Package Manager）

`manifests/` 下是可直接提交 [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) 的三件套（version / installer / defaultLocale），對應 `winget install ZhixuLi.DuDuClaw`。

## 發佈流程（每版）

1. Release 資產發佈後，更新三份 yaml 的 `PackageVersion` 與 `InstallerUrl`，`InstallerSha256` 用官方 `.sha256` 資產的值（大寫）：
   ```bash
   curl -fsSL https://github.com/zhixuli0406/DuDuClaw/releases/download/v<VER>/duduclaw-windows-x64.zip.sha256
   ```
2. Windows 機器上本機驗證（或跳過直接交 PR 吃 CI 驗證）：
   ```powershell
   winget validate --manifest distribution/winget/manifests/z/ZhixuLi/DuDuClaw/<VER>
   winget install --manifest distribution/winget/manifests/z/ZhixuLi/DuDuClaw/<VER>
   ```
3. 提交：fork microsoft/winget-pkgs → 把 `manifests/z/ZhixuLi/DuDuClaw/<VER>/` 三份檔案放到相同路徑 → PR。自動化驗證通過後通常數小時～數天合併。
   - 也可用 `wingetcreate update ZhixuLi.DuDuClaw --version <VER> --urls <zip-url> --submit`（首次之後的版本更新一行搞定）。

## 備註

- installer 走 `zip + portable` 巢狀型式：`duduclaw.exe` 在 zip 根（2026-08-13 實測驗證），winget 會自動建 `duduclaw` 指令 alias。zip 內的 `python/` sidecar 不會被 portable 型式安裝——技能審查等 Python 功能提示使用者另裝（與 npm wrapper 行為一致的已知限制，可日後改 MSI 解）。
- 首次提交（新 package）人工審查較久（數天～兩週）；之後版本更新走自動化。
